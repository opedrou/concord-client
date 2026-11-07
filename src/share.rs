//! Transmissão de tela: qualidades, captura numa thread própria e publicação na sala.
#[cfg(test)]
mod bancada;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use livekit::{
    options::{
        DegradationPreference, TrackPublishOptions, VideoCodec, VideoEncoderBackend, VideoEncoding,
    },
    participant::LocalParticipant,
    track::{LocalTrack, LocalVideoTrack, TrackSource},
    webrtc::{
        desktop_capturer::{
            CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
        },
        native::yuv_helper,
        prelude::{I420Buffer, RtcVideoSource, VideoFrame, VideoResolution, VideoRotation},
        video_source::native::NativeVideoSource,
    },
};
use serde::{Deserialize, Serialize};
use tokio::{
    runtime::Handle,
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        watch,
    },
};

/// Tabela da seção 7 do plano, os mesmos presets do `lib/screenShareQuality.ts` da web.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Economica,
    Media,
    #[default]
    Alta,
    Maxima,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Self::Economica, Self::Media, Self::Alta, Self::Maxima];

    pub fn label(self) -> &'static str {
        match self {
            Self::Economica => "Econômica",
            Self::Media => "Média",
            Self::Alta => "Alta",
            Self::Maxima => "Máxima",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Economica => "720p · 15 fps · 1,5 Mbps",
            Self::Media => "720p · 30 fps · 2 Mbps",
            Self::Alta => "1080p · 30 fps · 5 Mbps",
            Self::Maxima => "Original · 30 fps · 7 Mbps",
        }
    }

    /// Caixa onde o quadro tem que caber; `None` mantém a resolução original.
    fn limit(self) -> Option<(u32, u32)> {
        match self {
            Self::Economica | Self::Media => Some((1280, 720)),
            Self::Alta => Some((1920, 1080)),
            Self::Maxima => None,
        }
    }

    fn fps(self) -> u32 {
        match self {
            Self::Economica => 15,
            _ => 30,
        }
    }

    fn bitrate(self) -> u64 {
        match self {
            Self::Economica => 1_500_000,
            Self::Media => 2_000_000,
            Self::Alta => 5_000_000,
            Self::Maxima => 7_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum State {
    #[default]
    Off,
    /// Capturando, mas sem quadro: a pessoa ainda está no seletor do sistema.
    Choosing,
    Live,
    Failed,
}

pub enum Cmd {
    Start(Quality),
    Stop,
}

/// Reduz pra caber em `limit`, sem aumentar e mantendo a proporção. Lados sempre pares,
/// que é o que o 4:2:0 divide sem sobra.
fn fit(width: u32, height: u32, limit: Option<(u32, u32)>) -> (u32, u32) {
    let (w, h) = (f64::from(width), f64::from(height));
    let scale = limit.map_or(1.0, |(lw, lh)| (f64::from(lw) / w).min(f64::from(lh) / h));
    let side = |v: f64| ((v * scale.min(1.0)).round() as u32 & !1).max(2);
    (side(w), side(h))
}

enum Note {
    /// Chegou o primeiro quadro: agora dá pra publicar.
    Ready(NativeVideoSource),
    /// Não há capturador neste sistema. Fim normal é a thread soltar o canal.
    Failed,
}

/// Saída da thread de captura: reduz cada quadro e entrega à fonte de vídeo.
pub struct Feed {
    limit: Option<(u32, u32)>,
    source: Option<NativeVideoSource>,
    notes: UnboundedSender<Note>,
}

impl Feed {
    /// Quadro em ARGB. Buffer novo a cada quadro: o encoder lê em outra thread, e a
    /// resolução pode mudar no meio, ao trocar de janela.
    pub fn frame(&mut self, argb: &[u8], stride: u32, width: u32, height: u32) {
        let (w, h) = fit(width, height, self.limit);
        let mut full = I420Buffer::new(width, height);
        let (sy, su, sv) = full.strides();
        let (y, u, v) = full.data_mut();
        let size = (width as i32, height as i32);
        yuv_helper::argb_to_i420(argb, stride, y, sy, u, su, v, sv, size.0, size.1);
        let buffer = if (w, h) == (width, height) {
            full
        } else {
            full.scale(w as i32, h as i32)
        };
        let source = self.source.get_or_insert_with(|| {
            let source = NativeVideoSource::new(
                VideoResolution {
                    width: w,
                    height: h,
                },
                true,
            );
            let _ = self.notes.send(Note::Ready(source.clone()));
            source
        });
        source.capture_frame(&VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer: &buffer,
        });
    }

    pub fn fail(&self) {
        let _ = self.notes.send(Note::Failed);
    }
}

/// Corpo da thread de captura: fps, pedido de parar e saída dos quadros. Retornar encerra
/// a transmissão.
pub type Capturer = fn(u32, &AtomicBool, Feed);

/// Captura de verdade. No Linux, `Generic` sem alvo abre o seletor do portal sozinho.
pub fn desktop(fps: u32, stop: &AtomicBool, mut feed: Feed) {
    #[cfg(windows)]
    let kind = DesktopCaptureSourceType::Screen;
    #[cfg(not(windows))]
    let kind = DesktopCaptureSourceType::Generic;
    let mut options = DesktopCapturerOptions::new(kind);
    options.set_include_cursor(true);
    let Some(mut capturer) = DesktopCapturer::new(options) else {
        feed.fail();
        return;
    };
    // ponytail: no Windows vai a primeira tela; o seletor próprio entra na etapa dele.
    #[cfg(windows)]
    let target = capturer.get_source_list().into_iter().next();
    #[cfg(not(windows))]
    let target = None;

    let ended = Arc::new(AtomicBool::new(false));
    let flag = ended.clone();
    capturer.start_capture(target, move |result| match result {
        Ok(f) => feed.frame(f.data(), f.stride(), f.width() as u32, f.height() as u32),
        // Portal recusado, cancelado ou encerrado por fora.
        Err(CaptureError::Permanent) => flag.store(true, Ordering::Relaxed),
        // É o que vem enquanto a pessoa não escolhe.
        Err(CaptureError::Temporary) => {}
    });
    pace(fps, || {
        capturer.capture_frame();
        !stop.load(Ordering::Relaxed) && !ended.load(Ordering::Relaxed)
    });
}

/// Roda `step` no ritmo do fps, descontando o tempo gasto nele, até devolver `false`.
fn pace(fps: u32, mut step: impl FnMut() -> bool) {
    let tick = Duration::from_secs(1) / fps;
    let mut next = Instant::now();
    while step() {
        next += tick;
        let now = Instant::now();
        thread::sleep(next.saturating_duration_since(now));
        next = next.max(now);
    }
}

/// Uma captura em andamento. Soltar pede pra thread parar, inclusive quando a tarefa da
/// call é abortada.
struct Running {
    quality: Quality,
    stop: Arc<AtomicBool>,
    /// Fecha quando a thread termina.
    notes: UnboundedReceiver<Note>,
    /// Sobrevive à troca de sala: republicar não abre o seletor de novo.
    source: Option<NativeVideoSource>,
    /// Publicada na sala atual.
    published: bool,
}

impl Running {
    fn start(quality: Quality, capture: Capturer) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (notes_tx, notes) = mpsc::unbounded_channel();
        let feed = Feed {
            limit: quality.limit(),
            source: None,
            notes: notes_tx,
        };
        let flag = stop.clone();
        let runtime = Handle::current();
        // Thread própria e fixa: o capturador do Windows não aceita migrar entre threads.
        thread::Builder::new()
            .name("concord-tela".into())
            .spawn(move || {
                // A fonte de vídeo nasce no primeiro quadro e abre uma tarefa no tokio.
                let _runtime = runtime.enter();
                capture(quality.fps(), &flag, feed);
            })
            .ok()?;
        Some(Self {
            quality,
            stop,
            notes,
            source: None,
            published: false,
        })
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

async fn next_note(share: &mut Option<Running>) -> Option<Note> {
    match share {
        Some(r) => r.notes.recv().await,
        None => std::future::pending().await,
    }
}

async fn publish(me: &LocalParticipant, source: NativeVideoSource, quality: Quality) -> bool {
    let track = LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source));
    let options = TrackPublishOptions {
        source: TrackSource::Screenshare,
        video_codec: VideoCodec::H264,
        video_encoder: VideoEncoderBackend::Auto,
        simulcast: false,
        video_encoding: Some(VideoEncoding {
            max_bitrate: quality.bitrate(),
            max_framerate: f64::from(quality.fps()),
        }),
        degradation_preference: Some(DegradationPreference::MaintainFramerate),
        ..Default::default()
    };
    me.publish_track(LocalTrack::Video(track), options)
        .await
        .is_ok()
}

/// Para a captura e despublica. Procura pela lista, não por um sid guardado: a retomada
/// do SDK republica com outro sid.
async fn stop(share: &mut Option<Running>, room: &watch::Receiver<Option<LocalParticipant>>) {
    *share = None;
    let me = room.borrow().clone();
    let Some(me) = me else { return };
    for (sid, publication) in me.track_publications() {
        if publication.source() == TrackSource::Screenshare {
            let _ = me.unpublish_track(&sid).await;
        }
    }
}

/// Dono da transmissão durante a call. `room` é a sala atual, `None` entre uma conexão e
/// outra. Só [`Cmd::Start`] abre captura, e ele só vem do clique da pessoa.
pub async fn run(
    mut cmds: UnboundedReceiver<Cmd>,
    mut room: watch::Receiver<Option<LocalParticipant>>,
    capture: Capturer,
    notify: impl Fn(State),
) {
    let mut share: Option<Running> = None;
    loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Start(quality)) if share.is_none() => {
                    share = Running::start(quality, capture);
                    notify(if share.is_some() { State::Choosing } else { State::Failed });
                }
                Some(Cmd::Start(_)) => {}
                Some(Cmd::Stop) => {
                    stop(&mut share, &room).await;
                    notify(State::Off);
                }
                None => return,
            },
            note = next_note(&mut share) => match note {
                Some(Note::Ready(source)) => {
                    if let Some(r) = &mut share {
                        r.source = Some(source);
                    }
                }
                Some(Note::Failed) => {
                    stop(&mut share, &room).await;
                    notify(State::Failed);
                }
                // A thread acabou sozinha: portal cancelado ou encerrado por fora.
                None => {
                    stop(&mut share, &room).await;
                    notify(State::Off);
                }
            },
            changed = room.changed() => {
                if changed.is_err() {
                    return;
                }
                if let Some(r) = &mut share {
                    r.published = false;
                }
            }
        }

        // Quem vê não pode dar com LIVE vazio: só publica depois do primeiro quadro.
        let pending = share
            .as_ref()
            .filter(|r| !r.published)
            .and_then(|r| Some((r.source.clone()?, r.quality)));
        let Some((source, quality)) = pending else {
            continue;
        };
        let me = room.borrow_and_update().clone();
        let Some(me) = me else { continue };
        if publish(&me, source, quality).await {
            if let Some(r) = &mut share {
                r.published = true;
            }
            notify(State::Live);
        } else if !room.has_changed().unwrap_or(false) {
            // Com a sala trocando no meio, não é falha: a próxima volta publica na nova.
            stop(&mut share, &room).await;
            notify(State::Failed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::time::timeout;

    #[test]
    fn tabela_de_qualidades() {
        let linhas = Quality::ALL.map(|q| (q.limit(), q.fps(), q.bitrate()));
        assert_eq!(
            linhas,
            [
                (Some((1280, 720)), 15, 1_500_000),
                (Some((1280, 720)), 30, 2_000_000),
                (Some((1920, 1080)), 30, 5_000_000),
                (None, 30, 7_000_000),
            ]
        );
        assert_eq!(Quality::default(), Quality::Alta);
    }

    #[test]
    fn qualidade_salva_com_a_mesma_chave_da_web() {
        assert_eq!(
            serde_json::to_string(&Quality::Economica).unwrap(),
            r#""economica""#
        );
        let q: Quality = serde_json::from_str(r#""maxima""#).unwrap();
        assert_eq!(q, Quality::Maxima);
    }

    const HD: Option<(u32, u32)> = Some((1280, 720));
    const FHD: Option<(u32, u32)> = Some((1920, 1080));

    #[test]
    fn reduz_mantendo_a_proporcao() {
        assert_eq!(fit(1920, 1080, HD), (1280, 720));
        assert_eq!(fit(2560, 1440, FHD), (1920, 1080));
        assert_eq!(fit(1600, 900, HD), (1280, 720));
        assert_eq!(
            fit(3440, 1440, FHD),
            (1920, 804),
            "ultrawide manda a largura"
        );
        assert_eq!(fit(1080, 1920, FHD), (608, 1080), "em pé manda a altura");
        assert_eq!(fit(1920, 1200, HD), (1152, 720));
    }

    #[test]
    fn nunca_aumenta() {
        assert_eq!(fit(1280, 720, FHD), (1280, 720));
        assert_eq!(fit(800, 600, HD), (800, 600));
        assert_eq!(fit(1600, 900, FHD), (1600, 900));
        assert_eq!(fit(3840, 2160, None), (3840, 2160));
    }

    #[test]
    fn lados_sempre_pares_e_dentro_da_caixa() {
        assert_eq!(fit(1601, 901, None), (1600, 900));
        assert_eq!(fit(1, 1, None), (2, 2));
        for (w, h) in [(1366, 768), (1921, 1081), (5120, 1440), (333, 777)] {
            let (fw, fh) = fit(w, h, HD);
            assert!(fw % 2 == 0 && fh % 2 == 0, "{fw}x{fh}");
            assert!(fw <= 1280 && fh <= 720, "{fw}x{fh}");
            let distorcao = (fw as f64 / fh as f64) / (w as f64 / h as f64);
            assert!((distorcao - 1.0).abs() < 0.02, "{fw}x{fh}");
        }
    }

    #[test]
    fn ritmo_desconta_o_tempo_do_passo() {
        let inicio = Instant::now();
        let mut n = 0;
        pace(100, || {
            thread::sleep(Duration::from_millis(5));
            n += 1;
            n < 20
        });
        let ms = inicio.elapsed().as_millis();
        assert!((190..290).contains(&ms), "20 passos a 100 fps em {ms} ms");
    }

    const ESPERA: Duration = Duration::from_secs(5);

    /// Atores só em memória: sala nenhuma, então não chega em `Live`.
    fn ator(
        capture: Capturer,
    ) -> (
        UnboundedSender<Cmd>,
        UnboundedReceiver<State>,
        tokio::task::JoinHandle<()>,
    ) {
        let (cmd, cmds) = mpsc::unbounded_channel();
        let (states, estados) = mpsc::unbounded_channel();
        let (_room, room) = watch::channel(None);
        let tarefa = tokio::spawn(async move {
            let _room = _room;
            run(cmds, room, capture, move |s| {
                let _ = states.send(s);
            })
            .await;
        });
        (cmd, estados, tarefa)
    }

    async fn proximo(estados: &mut UnboundedReceiver<State>) -> State {
        timeout(ESPERA, estados.recv())
            .await
            .expect("estado não veio")
            .expect("ator acabou")
    }

    /// Espera a contagem de threads vivas de um teste zerar.
    async fn soltou(vivas: &AtomicUsize) {
        let inicio = Instant::now();
        while vivas.load(Ordering::SeqCst) > 0 {
            assert!(inicio.elapsed() < ESPERA, "a thread não saiu");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn espera_parar(stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[tokio::test]
    async fn parar_no_seletor_solta_a_thread() {
        static VIVAS: AtomicUsize = AtomicUsize::new(0);
        fn seletor(_: u32, stop: &AtomicBool, _: Feed) {
            VIVAS.fetch_add(1, Ordering::SeqCst);
            espera_parar(stop);
            VIVAS.fetch_sub(1, Ordering::SeqCst);
        }
        let (cmd, mut estados, _t) = ator(seletor);
        cmd.send(Cmd::Start(Quality::Alta)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        cmd.send(Cmd::Stop).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Off);
        soltou(&VIVAS).await;
    }

    #[tokio::test]
    async fn portal_cancelado_volta_a_parado_sem_falha() {
        fn cancela(_: u32, _: &AtomicBool, _: Feed) {}
        let (cmd, mut estados, _t) = ator(cancela);
        cmd.send(Cmd::Start(Quality::Alta)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        assert_eq!(proximo(&mut estados).await, State::Off);
    }

    #[tokio::test]
    async fn sem_capturador_avisa_falha() {
        fn sem(_: u32, _: &AtomicBool, feed: Feed) {
            feed.fail();
        }
        let (cmd, mut estados, _t) = ator(sem);
        cmd.send(Cmd::Start(Quality::Alta)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        assert_eq!(proximo(&mut estados).await, State::Failed);
    }

    #[tokio::test]
    async fn segundo_clique_nao_abre_outra_captura() {
        static ABERTAS: AtomicUsize = AtomicUsize::new(0);
        fn uma(_: u32, stop: &AtomicBool, _: Feed) {
            ABERTAS.fetch_add(1, Ordering::SeqCst);
            espera_parar(stop);
        }
        let (cmd, mut estados, _t) = ator(uma);
        cmd.send(Cmd::Start(Quality::Alta)).unwrap();
        cmd.send(Cmd::Start(Quality::Media)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        while ABERTAS.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        cmd.send(Cmd::Stop).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Off);
        assert_eq!(ABERTAS.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn sair_da_call_derruba_a_captura_em_qualquer_caminho() {
        static VIVAS: AtomicUsize = AtomicUsize::new(0);
        fn viva(_: u32, stop: &AtomicBool, _: Feed) {
            VIVAS.fetch_add(1, Ordering::SeqCst);
            espera_parar(stop);
            VIVAS.fetch_sub(1, Ordering::SeqCst);
        }
        // Tarefa da call abortada: é o que acontece em qualquer fim da call.
        let (cmd, mut estados, tarefa) = ator(viva);
        cmd.send(Cmd::Start(Quality::Alta)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        while VIVAS.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tarefa.abort();
        soltou(&VIVAS).await;

        // Todos os `Call` soltos, sem ninguém pedir pra parar.
        let (cmd, mut estados, _t) = ator(viva);
        cmd.send(Cmd::Start(Quality::Alta)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        drop(cmd);
        soltou(&VIVAS).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolucao_muda_no_meio_sem_quebrar() {
        fn muda(_: u32, _: &AtomicBool, mut feed: Feed) {
            for (w, h) in [(1920u32, 1080u32), (1600, 900), (1601, 901), (800, 1200)] {
                let argb = vec![128u8; (w * h * 4) as usize];
                feed.frame(&argb, w * 4, w, h);
            }
        }
        let (cmd, mut estados, _t) = ator(muda);
        cmd.send(Cmd::Start(Quality::Media)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        assert_eq!(proximo(&mut estados).await, State::Off);
    }
}
