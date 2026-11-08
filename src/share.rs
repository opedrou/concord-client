//! Transmissão de tela: qualidades, captura numa thread própria e publicação na sala.
// No Windows só os tipos `Choice` e `View` e os testes do grafo são usados daqui.
#[cfg_attr(windows, allow(dead_code))]
mod app_audio;
#[cfg(test)]
mod bancada;
#[cfg(windows)]
mod loopback;

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use livekit::{
    options::{
        AudioEncoding, DegradationPreference, TrackPublishOptions, VideoCodec, VideoEncoderBackend,
        VideoEncoding,
    },
    participant::LocalParticipant,
    track::{LocalAudioTrack, LocalTrack, LocalVideoTrack, TrackSource},
    webrtc::{
        audio_source::native::NativeAudioSource,
        desktop_capturer::{
            CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions,
        },
        native::yuv_helper,
        prelude::{
            I420Buffer, RtcAudioSource, RtcVideoSource, VideoBuffer, VideoFormatType, VideoFrame,
            VideoResolution, VideoRotation,
        },
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

#[cfg(not(windows))]
use app_audio::AppAudio as Audio;
pub use app_audio::{Choice, TRACK, View};
#[cfg(windows)]
use loopback::Loopback as Audio;

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

/// Pro menu do seletor do Windows: "Alta · 1080p · 30 fps".
impl std::fmt::Display for Quality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let resumo = self.detail().rsplit_once(" · ").map_or("", |(r, _)| r);
        write!(f, "{} · {resumo}", self.label())
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

/// Tela ou janela escolhida no nosso seletor, pelo id do WebRTC. Só o Windows usa: no Linux
/// quem escolhe é o portal.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Source {
    pub window: bool,
    pub id: u64,
    pub title: String,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const MAX: usize = 48;
        if self.title.chars().count() > MAX {
            let curto: String = self.title.chars().take(MAX - 1).collect();
            write!(f, "{curto}…")
        } else {
            f.write_str(&self.title)
        }
    }
}

/// Miniatura RGBA do seletor, e a prévia da própria transmissão.
#[derive(Clone, Debug)]
pub struct Thumb {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Caixa da miniatura: a interface mostra em 16:9.
#[cfg_attr(not(windows), allow(dead_code))]
const THUMB: (u32, u32) = (320, 180);

/// Quadro BGRA do WebRTC reduzido pra caber em [`THUMB`], já em RGBA.
#[cfg_attr(not(windows), allow(dead_code))]
fn thumb_of(bgra: &[u8], stride: u32, width: u32, height: u32) -> Thumb {
    let mut full = image::RgbaImage::new(width, height);
    for (y, row) in full.rows_mut().enumerate() {
        let line = &bgra[y * stride as usize..];
        for (x, px) in row.enumerate() {
            let p = &line[x * 4..x * 4 + 4];
            *px = image::Rgba([p[2], p[1], p[0], 255]);
        }
    }
    let (w, h) = fit(width, height, Some(THUMB));
    Thumb {
        width: w,
        height: h,
        rgba: image::imageops::thumbnail(&full, w, h).into_raw(),
    }
}

/// Um quadro da fonte, num capturador só dela. A captura de janela pode demorar uns
/// quadros pra entregar o primeiro.
#[cfg(windows)]
fn snapshot(kind: DesktopCaptureSourceType, id: u64) -> Option<Thumb> {
    let mut capturer = DesktopCapturer::new(DesktopCapturerOptions::new(kind))?;
    let source = capturer
        .get_source_list()
        .into_iter()
        .find(|s| s.id() == id)?;
    let slot = Arc::new(Mutex::new(None));
    let out = slot.clone();
    capturer.start_capture(Some(source), move |result| {
        if let Ok(f) = result {
            let thumb = thumb_of(f.data(), f.stride(), f.width() as u32, f.height() as u32);
            *out.lock().unwrap() = Some(thumb);
        }
    });
    for _ in 0..10 {
        capturer.capture_frame();
        if let Some(thumb) = slot.lock().unwrap().take() {
            return Some(thumb);
        }
        thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Telas primeiro, depois as janelas com título, cada uma com a miniatura. Bloqueia:
/// chamar fora da interface.
#[cfg(windows)]
pub fn sources() -> Vec<(Source, Option<Thumb>)> {
    let list = |window: bool| {
        let kind = if window {
            DesktopCaptureSourceType::Window
        } else {
            DesktopCaptureSourceType::Screen
        };
        let found = DesktopCapturer::new(DesktopCapturerOptions::new(kind))
            .map(|c| c.get_source_list())
            .unwrap_or_default();
        found
            .into_iter()
            .enumerate()
            .filter(|(_, s)| !window || !s.title().trim().is_empty())
            .map(|(i, s)| {
                let source = Source {
                    window,
                    id: s.id(),
                    title: if window {
                        s.title()
                    } else {
                        format!("Tela {}", i + 1)
                    },
                };
                let thumb = snapshot(kind, source.id);
                (source, thumb)
            })
            .collect::<Vec<_>>()
    };
    let mut all = list(false);
    all.extend(list(true));
    all
}

#[cfg(not(windows))]
pub fn sources() -> Vec<(Source, Option<Thumb>)> {
    Vec::new()
}

pub enum Cmd {
    /// Qualidade, se leva o áudio junto e, no Windows, o que transmitir (`None`: a
    /// primeira tela).
    Start(Quality, bool, Option<Source>),
    /// Troca o que sai sem despublicar: no Windows, o alvo do seletor; no Linux, `None` abre
    /// o portal de novo.
    Swap(Option<Source>),
    Stop,
    /// Troca o áudio de uma transmissão no ar.
    Audio(Choice),
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
    /// Chegou o primeiro quadro, com o tamanho original: agora dá pra publicar.
    Ready(NativeVideoSource, (u32, u32)),
    /// Não há capturador neste sistema. Fim normal é a thread soltar o canal.
    Failed,
}

/// Largura da prévia própria, perto da de um tile.
// ponytail: fixa; tile bem maior que isso estica a prévia, e aí vale passar a largura dele.
const PREVIEW_WIDTH: u32 = 480;
/// Uma prévia a cada 200 ms: 5 por segundo.
const PREVIEW_EVERY: Duration = Duration::from_millis(200);

/// Prévia da própria transmissão, pro tile local: o quadro mais novo, já pequeno. Pausada
/// (janela sem foco), o [`Feed`] nem converte.
#[derive(Clone, Default)]
pub struct Preview {
    picture: Arc<Mutex<Option<Thumb>>>,
    paused: Arc<AtomicBool>,
}

impl Preview {
    pub fn take(&self) -> Option<Thumb> {
        self.picture.lock().unwrap().take()
    }

    pub fn pause(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }
}

/// Saída da thread de captura: reduz cada quadro e entrega à fonte de vídeo. Leva junto o
/// alvo escolhido no seletor do Windows.
pub struct Feed {
    limit: Option<(u32, u32)>,
    /// Na troca já vem com a fonte publicada: a faixa segue a mesma.
    source: Option<NativeVideoSource>,
    notes: UnboundedSender<Note>,
    target: Option<Source>,
    /// Já avisou o primeiro quadro.
    started: bool,
    preview: Preview,
    previewed: Option<Instant>,
}

impl Feed {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn target(&self) -> Option<&Source> {
        self.target.as_ref()
    }

    /// Quadro em ARGB. Buffer novo a cada quadro: o encoder lê em outra thread, e a
    /// resolução pode mudar no meio, ao trocar de janela.
    pub fn frame(&mut self, argb: &[u8], stride: u32, width: u32, height: u32) {
        let (w, h) = fit(width, height, self.limit);
        let mut full = I420Buffer::new(width, height);
        let (sy, su, sv) = full.strides();
        let (y, u, v) = full.data_mut();
        let size = (width as i32, height as i32);
        yuv_helper::argb_to_i420(argb, stride, y, sy, u, su, v, sv, size.0, size.1);
        let mut buffer = if (w, h) == (width, height) {
            full
        } else {
            full.scale(w as i32, h as i32)
        };
        let source = self.source.get_or_insert_with(|| {
            NativeVideoSource::new(
                VideoResolution {
                    width: w,
                    height: h,
                },
                true,
            )
        });
        if !self.started {
            self.started = true;
            let _ = self
                .notes
                .send(Note::Ready(source.clone(), (width, height)));
        }
        source.capture_frame(&VideoFrame {
            rotation: VideoRotation::VideoRotation0,
            timestamp_us: 0,
            frame_metadata: None,
            buffer: &buffer,
        });
        self.preview(&mut buffer);
    }

    /// Prévia do quadro que acabou de sair, a cada [`PREVIEW_EVERY`] e só sem pausa.
    fn preview(&mut self, buffer: &mut I420Buffer) {
        let due = self.previewed.is_none_or(|t| t.elapsed() >= PREVIEW_EVERY);
        if !due || self.preview.paused.load(Ordering::Relaxed) {
            return;
        }
        self.previewed = Some(Instant::now());
        let limit = Some((PREVIEW_WIDTH, u32::MAX));
        let (w, h) = fit(buffer.width(), buffer.height(), limit);
        let small = buffer.scale(w as i32, h as i32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        // ABGR do libyuv é R, G, B, A na memória: o RGBA do iced.
        let small: &dyn VideoBuffer = &small;
        small.to_argb(VideoFormatType::ABGR, &mut rgba, w * 4, w as i32, h as i32);
        let thumb = Thumb {
            width: w,
            height: h,
            rgba,
        };
        *self.preview.picture.lock().unwrap() = Some(thumb);
    }

    pub fn fail(&self) {
        let _ = self.notes.send(Note::Failed);
    }
}

/// Corpo da thread de captura: fps, pedido de parar e saída dos quadros. Retornar encerra
/// a transmissão.
pub type Capturer = fn(u32, &AtomicBool, Feed);

/// Captura de verdade. No Linux, `Generic` sem alvo abre o seletor do portal sozinho. No
/// Windows vai o alvo do nosso seletor, achado pelo id num capturador desta thread.
pub fn desktop(fps: u32, stop: &AtomicBool, mut feed: Feed) {
    #[cfg(windows)]
    let kind = if feed.target().is_some_and(|t| t.window) {
        DesktopCaptureSourceType::Window
    } else {
        DesktopCaptureSourceType::Screen
    };
    #[cfg(not(windows))]
    let kind = DesktopCaptureSourceType::Generic;
    let mut options = DesktopCapturerOptions::new(kind);
    options.set_include_cursor(true);
    let Some(mut capturer) = DesktopCapturer::new(options) else {
        feed.fail();
        return;
    };
    #[cfg(windows)]
    let target = {
        let list = capturer.get_source_list().into_iter();
        let found = match feed.target() {
            Some(t) => list.into_iter().find(|s| s.id() == t.id),
            None => list.into_iter().next(),
        };
        // A janela fechou entre o seletor e o clique.
        if found.is_none() {
            feed.fail();
            return;
        }
        found
    };
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

/// Uma thread de captura. Soltar pede pra ela parar, inclusive quando a tarefa da call é
/// abortada.
struct Capture {
    stop: Arc<AtomicBool>,
    /// Fecha quando a thread termina.
    notes: UnboundedReceiver<Note>,
    /// Alvo do seletor: no Windows decide o que o áudio leva.
    #[cfg_attr(not(windows), allow(dead_code))]
    target: Option<Source>,
}

impl Capture {
    /// `source` é a fonte já publicada, na troca; `None` cria uma no primeiro quadro.
    fn start(
        quality: Quality,
        target: Option<Source>,
        source: Option<NativeVideoSource>,
        preview: &Preview,
        capture: Capturer,
    ) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let (notes_tx, notes) = mpsc::unbounded_channel();
        let feed = Feed {
            limit: quality.limit(),
            source,
            notes: notes_tx,
            target: target.clone(),
            started: false,
            preview: preview.clone(),
            previewed: None,
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
            stop,
            notes,
            target,
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Uma transmissão em andamento.
struct Running {
    quality: Quality,
    capture: Capture,
    /// Troca em andamento: vira a `capture` no primeiro quadro dela. Até lá a antiga segue
    /// no ar, e se a troca acabar sem quadro (seletor cancelado), fica a antiga.
    next: Option<Capture>,
    /// Sobrevive à troca de sala e à de tela: a faixa publicada é sempre esta.
    source: Option<NativeVideoSource>,
    /// Publicada na sala atual.
    published: bool,
    with_audio: bool,
    /// Tamanho do primeiro quadro: no Linux decide entre tela e janela.
    #[cfg_attr(windows, allow(dead_code))]
    frame: (u32, u32),
    /// Nasce depois do vídeo no ar e morre com o `Running`.
    audio: Option<Audio>,
}

impl Running {
    fn start(
        quality: Quality,
        with_audio: bool,
        target: Option<Source>,
        preview: &Preview,
        capture: Capturer,
    ) -> Option<Self> {
        Some(Self {
            quality,
            capture: Capture::start(quality, target, None, preview, capture)?,
            next: None,
            source: None,
            published: false,
            with_audio,
            frame: (0, 0),
            audio: None,
        })
    }

    /// Primeiro quadro da troca: a captura nova assume e a antiga para. O áudio vai junto.
    fn swapped(&mut self, frame: (u32, u32)) {
        let Some(next) = self.next.take() else { return };
        self.capture = next;
        self.frame = frame;
        #[cfg(not(windows))]
        if let Some(a) = &self.audio {
            a.reframe(frame);
        }
        #[cfg(windows)]
        if let Some(a) = &mut self.audio {
            a.retarget(self.capture.target.as_ref());
        }
    }
}

/// Próximo aviso da captura no ar ou da troca; `true` quando veio da troca.
async fn next_note(share: &mut Option<Running>) -> (bool, Option<Note>) {
    let Some(r) = share else {
        return std::future::pending().await;
    };
    match &mut r.next {
        Some(next) => tokio::select! {
            n = r.capture.notes.recv() => (false, n),
            n = next.notes.recv() => (true, n),
        },
        None => (false, r.capture.notes.recv().await),
    }
}

async fn publish(me: &LocalParticipant, source: NativeVideoSource, quality: Quality) -> bool {
    let track = LocalVideoTrack::create_video_track("screen_share", RtcVideoSource::Native(source));
    let options = TrackPublishOptions {
        source: TrackSource::Screenshare,
        // Windows sem NVENC cai no OpenH264 em software, que estoura o bitrate (lag) e depois o
        // WebRTC corta. VP8 mediu menos CPU e controla melhor a taxa. Linux segue em H.264 com VA-API.
        video_codec: if cfg!(windows) {
            VideoCodec::VP8
        } else {
            VideoCodec::H264
        },
        video_encoder: VideoEncoderBackend::Auto,
        simulcast: false,
        video_encoding: Some(VideoEncoding {
            max_bitrate: quality.bitrate(),
            max_framerate: f64::from(quality.fps()),
        }),
        // Igual ao site (livekit-client) e ao default do SDK pra tela: sob congestão cai fps,
        // não resolução. Com MaintainFramerate o texto virava pixel.
        degradation_preference: Some(DegradationPreference::MaintainResolution),
        ..Default::default()
    };
    me.publish_track(LocalTrack::Video(track), options)
        .await
        .is_ok()
}

/// Abre a captura de áudio da transmissão. No Windows a escolha já veio do seletor: não há
/// lista pra mostrar.
fn start_audio(r: &Running, audio: &(impl Fn(View) + Clone + Send + 'static)) -> Option<Audio> {
    #[cfg(not(windows))]
    {
        Audio::start(r.frame, audio.clone())
    }
    #[cfg(windows)]
    {
        let _ = audio;
        Audio::start(r.capture.target.as_ref())
    }
}

/// `Unknown` e com nome fixo, como o site: ele só toca pra quem clicou em Assistir.
async fn publish_audio(me: &LocalParticipant, source: NativeAudioSource) {
    let track =
        LocalAudioTrack::create_audio_track(app_audio::TRACK, RtcAudioSource::Native(source));
    let options = TrackPublishOptions {
        source: TrackSource::Unknown,
        audio_encoding: Some(AudioEncoding {
            max_bitrate: 128_000,
        }),
        // Sem DTX: música baixa não pode virar silêncio.
        dtx: false,
        ..Default::default()
    };
    let _ = me.publish_track(LocalTrack::Audio(track), options).await;
}

/// Para a captura e despublica. Procura pela lista, não por um sid guardado: a retomada
/// do SDK republica com outro sid.
async fn stop(share: &mut Option<Running>, room: &watch::Receiver<Option<LocalParticipant>>) {
    *share = None;
    let me = room.borrow().clone();
    let Some(me) = me else { return };
    for (sid, publication) in me.track_publications() {
        if publication.source() == TrackSource::Screenshare
            || publication.name() == app_audio::TRACK
        {
            let _ = me.unpublish_track(&sid).await;
        }
    }
}

/// Dono da transmissão durante a call. `room` é a sala atual, `None` entre uma conexão e
/// outra. Só [`Cmd::Start`] e [`Cmd::Swap`] abrem captura, e eles só vêm do clique da
/// pessoa. `audio` recebe o estado do áudio, de outra thread; `preview`, a prévia própria.
pub async fn run(
    mut cmds: UnboundedReceiver<Cmd>,
    mut room: watch::Receiver<Option<LocalParticipant>>,
    capture: Capturer,
    preview: Preview,
    notify: impl Fn(State),
    audio: impl Fn(View) + Clone + Send + 'static,
) {
    let mut share: Option<Running> = None;
    loop {
        tokio::select! {
            // Avisos antes dos comandos: um Trocar logo depois do primeiro quadro já acha a
            // fonte.
            biased;
            note = next_note(&mut share) => match note {
                (false, Some(Note::Ready(source, frame))) => {
                    if let Some(r) = &mut share {
                        r.source = Some(source);
                        r.frame = frame;
                    }
                }
                (true, Some(Note::Ready(_, frame))) => {
                    if let Some(r) = &mut share {
                        r.swapped(frame);
                    }
                }
                // Troca sem quadro: seletor cancelado, ou a janela fechou. Fica a antiga.
                (true, Some(Note::Failed) | None) => {
                    if let Some(r) = &mut share {
                        r.next = None;
                    }
                }
                (false, Some(Note::Failed)) => {
                    stop(&mut share, &room).await;
                    notify(State::Failed);
                }
                // A thread acabou sozinha: portal cancelado ou encerrado por fora.
                (false, None) => {
                    stop(&mut share, &room).await;
                    notify(State::Off);
                }
            },
            cmd = cmds.recv() => match cmd {
                Some(Cmd::Start(quality, with_audio, target)) if share.is_none() => {
                    // Sem sobra da transmissão anterior no tile.
                    preview.take();
                    share = Running::start(quality, with_audio, target, &preview, capture);
                    notify(if share.is_some() { State::Choosing } else { State::Failed });
                }
                Some(Cmd::Start(..)) => {}
                // Só com a fonte criada: é nela que a captura nova entra. Outro Trocar no
                // meio solta a troca anterior.
                Some(Cmd::Swap(target)) => {
                    if let Some(r) = share.as_mut().filter(|r| r.source.is_some()) {
                        let source = r.source.clone();
                        r.next = Capture::start(r.quality, target, source, &preview, capture);
                    }
                }
                Some(Cmd::Audio(choice)) => {
                    let me = room.borrow().clone();
                    if let Some(r) = share.as_mut().filter(|r| r.published) {
                        // Saiu sem áudio e a pessoa ligou agora: nasce e publica aqui.
                        if r.audio.is_none() && choice != Choice::Silent {
                            r.with_audio = true;
                            r.audio = start_audio(r, &audio);
                            if let (Some(me), Some(a)) = (&me, &r.audio) {
                                publish_audio(me, a.source.clone()).await;
                            }
                        }
                        if let Some(a) = &r.audio {
                            a.choose(choice);
                        }
                    }
                }
                Some(Cmd::Stop) => {
                    stop(&mut share, &room).await;
                    notify(State::Off);
                }
                None => return,
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
            notify(State::Live);
            // O áudio vai depois do vídeo, e na sala nova junto com ele.
            if let Some(r) = &mut share {
                r.published = true;
                if r.with_audio && r.audio.is_none() {
                    r.audio = start_audio(r, &audio);
                }
                if let Some(a) = &r.audio {
                    publish_audio(&me, a.source.clone()).await;
                }
            }
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

    #[test]
    fn titulo_longo_corta_com_reticencias() {
        let janela = |title: &str| Source {
            window: true,
            id: 1,
            title: title.into(),
        };
        assert_eq!(janela("Tela 1").to_string(), "Tela 1");
        let longo = "á".repeat(60);
        let curto = janela(&longo).to_string();
        assert_eq!(curto.chars().count(), 48);
        assert!(curto.ends_with('…'));
    }

    /// Lista as fontes de verdade com o tempo das miniaturas, um quadro de cada:
    /// `cargo test --release -- --ignored fontes_do_windows --nocapture`.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn fontes_do_windows() {
        let inicio = Instant::now();
        let fontes = sources();
        for (f, thumb) in &fontes {
            let tamanho = thumb.as_ref().map(|t| (t.width, t.height));
            let tipo = if f.window { "JANELA" } else { "TELA  " };
            println!("{tipo} {:>10} {tamanho:?} {f}", f.id);
        }
        println!(
            "{} fontes em {} ms",
            fontes.len(),
            inicio.elapsed().as_millis()
        );
        assert!(
            fontes.first().is_some_and(|(f, _)| !f.window),
            "falta a tela"
        );
    }

    #[test]
    fn miniatura_vira_rgba_e_cabe_na_caixa() {
        // 640x360 todo azul em BGRA, com 16 bytes de sobra por linha.
        let (w, h, stride) = (640u32, 360u32, 640 * 4 + 16);
        let mut bgra = vec![0u8; (stride * h) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * stride as usize + x * 4;
                bgra[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let t = thumb_of(&bgra, stride, w, h);
        assert_eq!((t.width, t.height), (320, 180));
        assert_eq!(t.rgba.len(), 320 * 180 * 4);
        assert_eq!(&t.rgba[..4], &[0, 0, 255, 255], "azul em RGBA");
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
            run(
                cmds,
                room,
                capture,
                Preview::default(),
                move |s| {
                    let _ = states.send(s);
                },
                |_| {},
            )
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
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        cmd.send(Cmd::Stop).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Off);
        soltou(&VIVAS).await;
    }

    #[tokio::test]
    async fn portal_cancelado_volta_a_parado_sem_falha() {
        fn cancela(_: u32, _: &AtomicBool, _: Feed) {}
        let (cmd, mut estados, _t) = ator(cancela);
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        assert_eq!(proximo(&mut estados).await, State::Off);
    }

    #[tokio::test]
    async fn sem_capturador_avisa_falha() {
        fn sem(_: u32, _: &AtomicBool, feed: Feed) {
            feed.fail();
        }
        let (cmd, mut estados, _t) = ator(sem);
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
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
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
        cmd.send(Cmd::Start(Quality::Media, false, None)).unwrap();
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
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        while VIVAS.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tarefa.abort();
        soltou(&VIVAS).await;

        // Todos os `Call` soltos, sem ninguém pedir pra parar.
        let (cmd, mut estados, _t) = ator(viva);
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
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
        cmd.send(Cmd::Start(Quality::Media, false, None)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        assert_eq!(proximo(&mut estados).await, State::Off);
    }

    /// Um quadro pequeno: cria a fonte, ou faz a troca assumir.
    fn quadro(feed: &mut Feed) {
        let argb = vec![128u8; 64 * 36 * 4];
        feed.frame(&argb, 64 * 4, 64, 36);
    }

    async fn ate(cond: impl Fn() -> bool) {
        let inicio = Instant::now();
        while !cond() {
            assert!(inicio.elapsed() < ESPERA, "não aconteceu");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn troca_entra_na_mesma_fonte_e_a_antiga_so_para_no_primeiro_quadro_novo() {
        use futures_util::StreamExt;
        use livekit::webrtc::video_stream::native::NativeVideoStream;
        static ABERTAS: AtomicUsize = AtomicUsize::new(0);
        static ANTIGA: AtomicUsize = AtomicUsize::new(0);
        static FONTE: Mutex<Option<NativeVideoSource>> = Mutex::new(None);
        static LIBERA: AtomicBool = AtomicBool::new(false);
        fn tela(_: u32, stop: &AtomicBool, mut feed: Feed) {
            if ABERTAS.fetch_add(1, Ordering::SeqCst) == 0 {
                ANTIGA.fetch_add(1, Ordering::SeqCst);
                quadro(&mut feed);
                *FONTE.lock().unwrap() = feed.source.clone();
                espera_parar(stop);
                ANTIGA.fetch_sub(1, Ordering::SeqCst);
                return;
            }
            // A da troca fica "no seletor" até o teste liberar, e aí manda 96x54.
            while !LIBERA.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
            let argb = vec![200u8; 96 * 54 * 4];
            while !stop.load(Ordering::SeqCst) {
                feed.frame(&argb, 96 * 4, 96, 54);
                thread::sleep(Duration::from_millis(10));
            }
        }
        let (cmd, mut estados, _t) = ator(tela);
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        ate(|| FONTE.lock().unwrap().is_some()).await;
        // A faixa como a sala vê: o que entrar na fonte publicada sai aqui.
        let fonte = FONTE.lock().unwrap().clone().unwrap();
        let faixa = LocalVideoTrack::create_video_track("t", RtcVideoSource::Native(fonte));
        let mut sai = NativeVideoStream::new(faixa.rtc_track());

        cmd.send(Cmd::Swap(None)).unwrap();
        ate(|| ABERTAS.load(Ordering::SeqCst) == 2).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            ANTIGA.load(Ordering::SeqCst),
            1,
            "sem quadro novo, a antiga segue"
        );
        LIBERA.store(true, Ordering::SeqCst);
        let nova = timeout(ESPERA, async {
            while let Some(f) = sai.next().await {
                if f.buffer.width() == 96 {
                    return;
                }
            }
        });
        nova.await
            .expect("a faixa publicada não recebeu a captura nova");
        soltou(&ANTIGA).await;
        assert!(estados.try_recv().is_err(), "trocar não mexe no estado");
        cmd.send(Cmd::Stop).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Off);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelar_ou_falhar_na_troca_mantem_a_antiga_sem_falha() {
        static ABERTAS: AtomicUsize = AtomicUsize::new(0);
        static ANTIGA: AtomicUsize = AtomicUsize::new(0);
        static PRONTA: AtomicBool = AtomicBool::new(false);
        fn tela(_: u32, stop: &AtomicBool, mut feed: Feed) {
            match ABERTAS.fetch_add(1, Ordering::SeqCst) {
                0 => {
                    ANTIGA.fetch_add(1, Ordering::SeqCst);
                    quadro(&mut feed);
                    PRONTA.store(true, Ordering::SeqCst);
                    espera_parar(stop);
                    ANTIGA.fetch_sub(1, Ordering::SeqCst);
                }
                // Portal cancelado: a thread acaba sem quadro.
                1 => {}
                // A janela fechou entre o seletor e o clique.
                _ => feed.fail(),
            }
        }
        let (cmd, mut estados, _t) = ator(tela);
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        ate(|| PRONTA.load(Ordering::SeqCst)).await;
        for abertas in [2, 3] {
            cmd.send(Cmd::Swap(None)).unwrap();
            ate(|| ABERTAS.load(Ordering::SeqCst) == abertas).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(ANTIGA.load(Ordering::SeqCst), 1, "a antiga segue no ar");
        assert!(estados.try_recv().is_err(), "nem falha nem parado");
        cmd.send(Cmd::Stop).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Off);
        soltou(&ANTIGA).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parar_no_meio_da_troca_solta_as_duas_threads() {
        static ABERTAS: AtomicUsize = AtomicUsize::new(0);
        static VIVAS: AtomicUsize = AtomicUsize::new(0);
        static PRONTA: AtomicBool = AtomicBool::new(false);
        fn tela(_: u32, stop: &AtomicBool, mut feed: Feed) {
            VIVAS.fetch_add(1, Ordering::SeqCst);
            // A primeira manda um quadro; a da troca fica no seletor.
            if ABERTAS.fetch_add(1, Ordering::SeqCst) == 0 {
                quadro(&mut feed);
                PRONTA.store(true, Ordering::SeqCst);
            }
            espera_parar(stop);
            VIVAS.fetch_sub(1, Ordering::SeqCst);
        }
        let (cmd, mut estados, _t) = ator(tela);
        cmd.send(Cmd::Start(Quality::Alta, false, None)).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Choosing);
        ate(|| PRONTA.load(Ordering::SeqCst)).await;
        cmd.send(Cmd::Swap(None)).unwrap();
        ate(|| VIVAS.load(Ordering::SeqCst) == 2).await;
        cmd.send(Cmd::Stop).unwrap();
        assert_eq!(proximo(&mut estados).await, State::Off);
        soltou(&VIVAS).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn previa_pequena_poucas_por_segundo_e_nenhuma_sem_foco() {
        let (notes, _avisos) = mpsc::unbounded_channel();
        let preview = Preview::default();
        let mut feed = Feed {
            limit: Quality::Alta.limit(),
            source: None,
            notes,
            target: None,
            started: false,
            preview: preview.clone(),
            previewed: None,
        };
        // 1080p todo azul, em BGRA.
        let azul = [255u8, 0, 0, 255].repeat(1920 * 1080);
        let mut quadro = || feed.frame(&azul, 1920 * 4, 1920, 1080);
        quadro();
        let p = preview.take().expect("o primeiro quadro já dá prévia");
        assert_eq!((p.width, p.height), (PREVIEW_WIDTH, 270));
        assert_eq!(p.rgba.len(), 480 * 270 * 4);
        let px = &p.rgba[..4];
        assert!(
            px[0] < 40 && px[2] > 200 && px[3] == 255,
            "azul em RGBA: {px:?}"
        );
        quadro();
        assert!(
            preview.take().is_none(),
            "antes de 200 ms não converte de novo"
        );
        thread::sleep(PREVIEW_EVERY);
        preview.pause(true);
        quadro();
        assert!(preview.take().is_none(), "sem foco não converte");
        preview.pause(false);
        quadro();
        assert!(
            preview.take().is_some(),
            "com foco volta no quadro seguinte"
        );
    }
}
