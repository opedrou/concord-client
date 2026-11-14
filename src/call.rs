//! Conexão com a sala do LiveKit: entrar, publicar o mic, ouvir os outros e reconectar.
use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::StreamExt;
use livekit::{
    DisconnectReason, Room, RoomEvent, RoomOptions,
    options::TrackPublishOptions,
    participant::LocalParticipant,
    publication::{LocalTrackPublication, RemoteTrackPublication},
    track::{
        LocalAudioTrack, LocalTrack, RemoteTrack, RemoteVideoTrack, TrackSource, VideoQuality,
    },
    webrtc::{
        audio_source::native::NativeAudioSource,
        audio_stream::native::NativeAudioStream,
        prelude::{AudioFrame, AudioSourceOptions, RtcAudioSource, VideoFormatType},
        video_stream::native::NativeVideoStream,
    },
};
use tokio::{
    sync::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        watch,
    },
    task::JoinHandle,
    time::{Instant, sleep, sleep_until, timeout},
};

use crate::api::{self, Client};
use crate::audio::{Engine, Meter, MicFrames, RATE};
use crate::config::Config;
use crate::share::{self, Capturer, Choice, Quality};

/// Máximo de tentativas de reconexão, como na web.
pub const MAX_ATTEMPTS: u32 = 10;

/// Atributo de participante com os nomes que esta pessoa deixou em volume zero.
const MUTED_ATTRIBUTE: &str = "concord.muted";
const MUTED_DEBOUNCE: Duration = Duration::from_millis(300);
/// Sem isso um `close()` travado seguraria o `Ended` pra sempre.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Quem está na sala agora. O local vem primeiro.
#[derive(Clone, Debug, PartialEq)]
pub struct Live {
    pub identity: String,
    pub name: String,
    pub is_local: bool,
    pub muted: bool,
    pub camera: bool,
    pub screen_share: bool,
    pub speaking: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum End {
    /// A pessoa saiu.
    Left,
    /// Esta conta entrou de outro lugar com a mesma instalação.
    Duplicate,
    /// Removida da sala.
    Removed,
    /// Sala fechada no servidor.
    RoomClosed,
    /// Sessão vencida ao pedir token.
    Unauthorized,
    /// Esgotou as tentativas, ou a sala recusou entrar.
    GaveUp,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Connected,
    /// Caiu e está tentando voltar: tentativa `attempt` de [`MAX_ATTEMPTS`]. Zero é a
    /// retomada do próprio SDK, que vem antes das nossas tentativas.
    Reconnecting {
        attempt: u32,
    },
    /// Lista inteira, a cada mudança.
    Participants(Vec<Live>),
    Share(share::State),
    /// Áudio da transmissão no ar: a escolha e os apps tocando.
    ShareAudio(share::View),
    /// Prévia pequena da transmissão de alguém, pela identity: um quadro só.
    Preview(String, Picture),
    /// Último evento da call.
    Ended(End),
}

enum Cmd {
    Mute(bool),
    MutedPeople(Vec<String>),
    /// Identity de quem assistir; `None` para de assistir.
    Watch(Option<String>),
    Leave,
    /// Queda simulada pelo SDK, pra bancada.
    #[cfg(test)]
    Simulate(livekit::SimulateScenario),
}

/// Quadro de transmissão em RGBA: o assistido, inteiro, ou a prévia, pequena.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Largura da prévia. Esticada no tile, ela mesma fica borrada.
const PREVIEW_WIDTH: u32 = 96;

impl Picture {
    /// Quadro do WebRTC em RGBA. ABGR do libyuv é R, G, B, A na memória: o RGBA do iced.
    fn of(frame: &livekit::webrtc::video_frame::BoxVideoFrame) -> Self {
        let (width, height) = (frame.buffer.width(), frame.buffer.height());
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        let stride = width * 4;
        frame.buffer.to_argb(
            VideoFormatType::ABGR,
            &mut rgba,
            stride,
            width as i32,
            height as i32,
        );
        Self {
            width,
            height,
            rgba,
        }
    }

    /// Reduzida pra [`PREVIEW_WIDTH`], na mesma proporção.
    fn preview(&self) -> Self {
        let width = PREVIEW_WIDTH.min(self.width).max(1);
        let height = (self.height * width / self.width.max(1)).max(1);
        let full = image::RgbaImage::from_raw(self.width, self.height, self.rgba.clone())
            .unwrap_or_default();
        Self {
            width,
            height,
            rgba: image::imageops::thumbnail(&full, width, height).into_raw(),
        }
    }
}

/// Último quadro da transmissão assistida. A interface pega no relógio dela: quadro que
/// ela não chegou a desenhar é trocado pelo novo, sem fila.
#[derive(Clone, Default)]
pub struct Screen(Arc<Mutex<Option<Picture>>>);

impl Screen {
    pub fn take(&self) -> Option<Picture> {
        self.0.lock().unwrap().take()
    }

    fn put(&self, picture: Picture) {
        *self.0.lock().unwrap() = Some(picture);
    }

    fn clear(&self) {
        self.0.lock().unwrap().take();
    }
}

/// Último quadro da câmera de cada pessoa, pela identity. Como na [`Screen`], quadro que a
/// interface não pegou é trocado pelo novo.
#[derive(Clone, Default)]
pub struct Cameras(Arc<Mutex<HashMap<String, Picture>>>);

impl Cameras {
    pub fn take(&self, identity: &str) -> Option<Picture> {
        self.0.lock().unwrap().remove(identity)
    }

    fn put(&self, identity: &str, picture: Picture) {
        self.0.lock().unwrap().insert(identity.to_owned(), picture);
    }
}

/// Camada pedida da câmera dos outros. Do site vem VP9 em 720p: a média é 640x360, o
/// tamanho de um tile numa janela comum.
// ponytail: camada fixa. Sozinho em janela grande fica meio borrado, e com 9 tiles vem mais
// que o tile mostra. Pra seguir o tile, `update_video_dimensions` com o tamanho dele.
const CAMERA_QUALITY: VideoQuality = VideoQuality::Medium;

#[derive(Clone)]
pub struct Call {
    cmd: UnboundedSender<Cmd>,
    share: UnboundedSender<share::Cmd>,
    engine: Engine,
    screen: Screen,
    cameras: Cameras,
    preview: share::Preview,
}

impl fmt::Debug for Call {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call").finish_non_exhaustive()
    }
}

impl Call {
    /// Conecta em segundo plano. `install_id` é o sufixo fixo da identity. `mic` são
    /// os blocos que o [`Engine::start`] devolveu; a call os consome até acabar.
    pub fn join(
        client: Client,
        slug: String,
        install_id: String,
        engine: Engine,
        mic: MicFrames,
        mic_muted: bool,
    ) -> (Call, UnboundedReceiver<Event>) {
        Self::join_with(
            client,
            slug,
            install_id,
            engine,
            mic,
            mic_muted,
            share::desktop,
        )
    }

    /// [`Call::join`] com a captura trocável, pra bancada publicar sem abrir o portal.
    pub(crate) fn join_with(
        client: Client,
        slug: String,
        install_id: String,
        engine: Engine,
        mic: MicFrames,
        mic_muted: bool,
        capture: Capturer,
    ) -> (Call, UnboundedReceiver<Event>) {
        // Fecha o mic antes de qualquer coisa: entrar nunca abre o mic sozinho.
        if mic_muted {
            engine.set_mic_muted(true);
        }
        let (tx, events) = mpsc::unbounded_channel();
        let (cmd, rx) = mpsc::unbounded_channel();
        let (share, share_rx) = mpsc::unbounded_channel();
        let (room, room_rx) = watch::channel(None);
        let (states, audio) = (tx.clone(), tx.clone());
        let preview = share::Preview::default();
        let sharing = share::run(
            share_rx,
            room_rx,
            capture,
            preview.clone(),
            move |state| {
                let _ = states.send(Event::Share(state));
            },
            move |view| {
                let _ = audio.send(Event::ShareAudio(view));
            },
        );
        let task = Task {
            client,
            slug,
            install_id,
            tx,
            room,
        };
        let (speech_tx, speech_rx) = mpsc::unbounded_channel();
        let (peek_tx, peek_rx) = mpsc::unbounded_channel();
        let screen = Screen::default();
        let cameras = Cameras::default();
        let ctl = Ctl {
            rx,
            engine: engine.clone(),
            muted: mic_muted,
            people: Vec::new(),
            watching: None,
            screen: screen.clone(),
            cameras: cameras.clone(),
            peek_tx,
            peek_rx,
            speech_tx,
            speech_rx,
        };
        tokio::spawn(task.run(ctl, mic, AbortOnDrop(tokio::spawn(sharing))));
        let call = Call {
            cmd,
            share,
            engine,
            screen,
            cameras,
            preview,
        };
        (call, events)
    }

    /// Assiste a transmissão de `identity`, com o áudio dela, ou para com `None`. Os
    /// quadros chegam em [`Call::screen`].
    pub fn watch(&self, identity: Option<String>) {
        let _ = self.cmd.send(Cmd::Watch(identity));
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Câmera de quem está com ela ligada, assinada ao entrar.
    pub fn cameras(&self) -> &Cameras {
        &self.cameras
    }

    /// Prévia da própria transmissão, pro tile local.
    pub fn preview(&self) -> &share::Preview {
        &self.preview
    }

    pub fn set_mic_muted(&self, muted: bool) {
        // Mutar vale na hora, sem esperar a tarefa; desmutar passa pela tarefa, que
        // abre a faixa antes do motor.
        if muted {
            self.engine.set_mic_muted(true);
        }
        let _ = self.cmd.send(Cmd::Mute(muted));
    }

    /// Nomes com voz em zero: vai pro atributo `concord.muted`.
    pub fn set_muted_people(&self, names: Vec<String>) {
        let _ = self.cmd.send(Cmd::MutedPeople(names));
    }

    /// Abre a captura; o andamento vem em [`Event::Share`] e o do áudio, se `audio`, em
    /// [`Event::ShareAudio`].
    pub fn start_share(&self, quality: Quality, audio: bool, target: Option<share::Source>) {
        let _ = self.share.send(share::Cmd::Start(quality, audio, target));
    }

    /// Troca o que sai sem parar a transmissão: `target` no Windows; no Linux, `None` abre
    /// o portal. Cancelar mantém o que já saía.
    pub fn swap_share(&self, target: Option<share::Source>) {
        let _ = self.share.send(share::Cmd::Swap(target));
    }

    pub fn choose_share_audio(&self, choice: Choice) {
        let _ = self.share.send(share::Cmd::Audio(choice));
    }

    pub fn stop_share(&self) {
        let _ = self.share.send(share::Cmd::Stop);
    }

    /// Sai de forma limpa, com `room.close()`. Termina em `Ended(End::Left)`.
    pub fn leave(&self) {
        self.stop_share();
        let _ = self.cmd.send(Cmd::Leave);
    }

    #[cfg(test)]
    pub(crate) fn simulate(&self, scenario: livekit::SimulateScenario) {
        let _ = self.cmd.send(Cmd::Simulate(scenario));
    }
}

// --- Regras puras ------------------------------------------------------------

/// Motivo de desconexão definitivo vira o fim da call. Desconhecido é transitório,
/// como na web (`isDefinitiveDisconnect`).
fn definitive(reason: DisconnectReason) -> Option<End> {
    match reason {
        DisconnectReason::ClientInitiated => Some(End::Left),
        DisconnectReason::DuplicateIdentity => Some(End::Duplicate),
        DisconnectReason::ParticipantRemoved => Some(End::Removed),
        DisconnectReason::RoomDeleted | DisconnectReason::RoomClosed => Some(End::RoomClosed),
        _ => None,
    }
}

/// Espera antes da tentativa `attempt` (a primeira é 1): `min(2000·2^n, 30000)` ms.
fn backoff(attempt: u32) -> Duration {
    let n = attempt.saturating_sub(1).min(10);
    Duration::from_millis((2000u64 << n).min(30_000))
}

/// Nomes ordenados e sem repetição, separados por vírgula: igual a `encodeMuted` da web.
fn encode_muted(names: &[String]) -> String {
    let mut v: Vec<&str> = names.iter().map(String::as_str).collect();
    v.sort_unstable();
    v.dedup();
    v.join(",")
}

/// Monta uma pessoa a partir das publicações. Sem mic publicado conta como mudo, a
/// mesma regra do servidor em `/api/channels/presence`.
fn live(
    identity: String,
    name: String,
    is_local: bool,
    pubs: impl IntoIterator<Item = (TrackSource, bool)>,
    speaking: bool,
) -> Live {
    let (mut mic_open, mut camera, mut screen_share) = (false, false, false);
    for (source, muted) in pubs {
        match source {
            TrackSource::Microphone => mic_open |= !muted,
            TrackSource::Camera => camera |= !muted,
            TrackSource::Screenshare => screen_share = true,
            _ => {}
        }
    }
    Live {
        name: if name.is_empty() {
            identity.clone()
        } else {
            name
        },
        identity,
        is_local,
        muted: !mic_open,
        camera,
        screen_share,
        speaking: speaking && mic_open,
    }
}

/// Local primeiro; os outros em ordem estável, pra lista não pular de lugar.
fn roster(local: Live, mut remotes: Vec<Live>) -> Vec<Live> {
    remotes.sort_by_cached_key(|p| (p.name.to_lowercase(), p.identity.clone()));
    let mut all = vec![local];
    all.extend(remotes);
    all
}

// --- Tarefa da call ----------------------------------------------------------

struct Task {
    client: Client,
    slug: String,
    install_id: String,
    tx: UnboundedSender<Event>,
    /// Sala atual, pra transmissão: ao mudar, ela republica sem abrir o seletor.
    room: watch::Sender<Option<LocalParticipant>>,
}

/// Estado que atravessa as reconexões.
struct Ctl {
    rx: UnboundedReceiver<Cmd>,
    engine: Engine,
    /// Último estado pedido do mic.
    muted: bool,
    people: Vec<String>,
    /// Quem está sendo assistido: vale entre reconexões.
    watching: Option<String>,
    screen: Screen,
    cameras: Cameras,
    /// Sid da tela cuja prévia já saiu: a assinatura pode ser solta.
    peek_tx: UnboundedSender<String>,
    peek_rx: UnboundedReceiver<String>,
    /// Quem começou ou parou de falar, medido no PCM.
    speech_tx: UnboundedSender<(String, bool)>,
    speech_rx: UnboundedReceiver<(String, bool)>,
}

enum Fail {
    Unauthorized,
    Leave,
    Retry,
}

enum Outcome {
    End(End),
    /// Caiu por motivo transitório.
    Dropped,
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Voice {
    identity: String,
    _task: AbortOnDrop,
}

/// Uma conexão com a sala. Soltar a sessão solta as faixas e as vozes.
/// Limiares de fala em RMS de 0 a 1: o gate de quem fala já zerou o silêncio, então
/// aqui basta saber se tem sinal. A queda segura 150 ms, pra não piscar entre palavras.
const SPEECH_RISE: f32 = 0.01;
const SPEECH_FALL: f32 = 0.005;
const SPEECH_HOLD: usize = RATE as usize * 150 / 1000;

/// Chave da pessoa local em [`Session::speaking`].
const LOCAL: &str = "";

/// Anel de fala das faixas remotas, medido no PCM que chega, como o site faz. O
/// `ActiveSpeakersChanged` dá uma volta pelo servidor, com o intervalo de agregação dele,
/// e chega atrasado. O anel local vem do [`Meter`].
#[derive(Default)]
struct Speech {
    on: bool,
    quiet: usize,
}

impl Speech {
    /// Devolve o estado novo quando ele muda.
    fn update(&mut self, block: &[i16]) -> Option<bool> {
        let energy: f32 = block.iter().map(|s| (*s as f32 / 32768.0).powi(2)).sum();
        let rms = (energy / block.len().max(1) as f32).sqrt();
        if rms >= SPEECH_RISE {
            self.quiet = 0;
            return (!std::mem::replace(&mut self.on, true)).then_some(true);
        }
        if !self.on || rms >= SPEECH_FALL {
            return None;
        }
        self.quiet += block.len();
        if self.quiet < SPEECH_HOLD {
            return None;
        }
        *self = Self::default();
        Some(false)
    }
}

struct Session {
    room: Room,
    events: UnboundedReceiver<RoomEvent>,
    mic: LocalTrackPublication,
    voices: HashMap<String, Voice>,
    last: Vec<Live>,
    /// Último `concord.muted` enviado nesta sala.
    sent_muted: String,
    reconnecting: bool,
    speaking: HashSet<String>,
    /// Vídeo e áudio da transmissão assistida, por sid da publicação.
    watched: HashMap<String, AbortOnDrop>,
    /// Telas assinadas só pra tirar a prévia, por sid.
    peeks: HashMap<String, AbortOnDrop>,
    /// Câmeras, por sid: o mesmo par de dono e tarefa da voz.
    cameras: HashMap<String, Voice>,
}

impl Ctl {
    /// Roda `fut` atendendo os comandos; `None` se pediram pra sair antes de acabar.
    async fn or_leave<T>(&mut self, fut: impl Future<Output = T>) -> Option<T> {
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return Some(r),
                cmd = self.rx.recv() => match cmd {
                    None | Some(Cmd::Leave) => return None,
                    Some(Cmd::Mute(m)) => {
                        self.muted = m;
                        self.engine.set_mic_muted(m);
                    }
                    Some(Cmd::MutedPeople(p)) => self.people = p,
                    Some(Cmd::Watch(w)) => self.watching = w,
                    #[cfg(test)]
                    Some(Cmd::Simulate(_)) => {}
                }
            }
        }
    }
}

/// Vídeo da transmissão assistida, quadro a quadro, até a tarefa cair.
fn stream_screen(video: RemoteVideoTrack, screen: Screen) -> AbortOnDrop {
    let mut stream = NativeVideoStream::new(video.rtc_track());
    AbortOnDrop(tokio::spawn(async move {
        while let Some(frame) = stream.next().await {
            screen.put(Picture::of(&frame));
        }
    }))
}

/// Câmera de alguém, quadro a quadro, até a tarefa cair. Ao cair, leva junto o quadro que
/// sobrou: o `abort` não para um quadro no meio, então limpar por fora ainda deixaria um.
fn stream_camera(video: RemoteVideoTrack, identity: String, cameras: Cameras) -> AbortOnDrop {
    struct Clear(Cameras, String);
    impl Drop for Clear {
        fn drop(&mut self) {
            self.0.take(&self.1);
        }
    }
    let mut stream = NativeVideoStream::new(video.rtc_track());
    AbortOnDrop(tokio::spawn(async move {
        let camera = Clear(cameras, identity);
        while let Some(frame) = stream.next().await {
            camera.0.put(&camera.1, Picture::of(&frame));
        }
    }))
}

/// Um quadro só, reduzido pra prévia, e avisa pela `done` que a assinatura pode cair.
fn peek(
    video: RemoteVideoTrack,
    identity: String,
    sid: String,
    events: &UnboundedSender<Event>,
    done: &UnboundedSender<String>,
) -> AbortOnDrop {
    let mut stream = NativeVideoStream::new(video.rtc_track());
    let (events, done) = (events.clone(), done.clone());
    AbortOnDrop(tokio::spawn(async move {
        if let Some(frame) = stream.next().await {
            let preview = Picture::of(&frame).preview();
            let _ = events.send(Event::Preview(identity, preview));
        }
        let _ = done.send(sid);
    }))
}

/// Faixas que todo mundo assina ao entrar: a voz e a câmera.
fn always_subscribed(source: TrackSource) -> bool {
    matches!(source, TrackSource::Microphone | TrackSource::Camera)
}

/// Faixas que só são assinadas por quem clicou em Assistir: a tela e o áudio de app,
/// pelo nome que o site usa.
fn is_watch_track(source: TrackSource, name: &str) -> bool {
    source == TrackSource::Screenshare || name == share::TRACK
}

/// Fechar o motor antes da faixa, abrir a faixa antes do motor: nenhum bloco sai
/// por uma faixa que ainda não está no estado certo.
fn apply_mute(engine: &Engine, mic: Option<&LocalTrackPublication>, muted: bool) {
    if muted {
        engine.set_mic_muted(true);
        if let Some(m) = mic {
            m.mute();
        }
    } else {
        if let Some(m) = mic {
            m.unmute();
        }
        engine.set_mic_muted(false);
    }
}

/// Dono do `MicFrames`: manda cada bloco pra faixa da sala atual, ou descarta se
/// não há sala.
async fn forward_mic(
    mut mic: MicFrames,
    meter: Meter,
    source: watch::Receiver<Option<NativeAudioSource>>,
    speech: UnboundedSender<(String, bool)>,
) {
    let mut speaking = false;
    while let Some(block) = mic.recv().await {
        // O anel local é o próprio limitador: acende quando o mic está saindo pra sala.
        let open = meter.open();
        if open != speaking {
            speaking = open;
            let _ = speech.send((LOCAL.to_owned(), open));
        }
        let Some(src) = source.borrow().clone() else {
            continue;
        };
        let frame = AudioFrame {
            samples_per_channel: block.len() as u32,
            data: block.into(),
            sample_rate: RATE,
            num_channels: 1,
        };
        let _ = src.capture_frame(&frame).await;
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => sleep_until(d).await,
        None => std::future::pending().await,
    }
}

impl Task {
    fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    async fn run(self, mut ctl: Ctl, mic: MicFrames, sharing: AbortOnDrop) {
        let (source, source_rx) = watch::channel(None);
        let speech = ctl.speech_tx.clone();
        let meter = ctl.engine.meter();
        let _forward = AbortOnDrop(tokio::spawn(forward_mic(mic, meter, source_rx, speech)));
        let end = self.lifecycle(&mut ctl, &source).await;
        log!("sala: fim da call em {} ({end:?})", self.slug);
        // Faixa e vozes já caíram aqui: `Ended` é sempre o último evento. Soltar a tarefa
        // da tela para a captura também nos fins que não passam por `leave`.
        drop(sharing);
        self.emit(Event::Ended(end));
    }

    async fn lifecycle(
        &self,
        ctl: &mut Ctl,
        source: &watch::Sender<Option<NativeAudioSource>>,
    ) -> End {
        let mut attempt = 0;
        loop {
            match self.connect(ctl, source).await {
                Ok(mut session) => {
                    attempt = 0;
                    log!("sala: conectou em {}", self.slug);
                    self.emit(Event::Connected);
                    let outcome = self.serve(&mut session, ctl).await;
                    source.send_replace(None);
                    self.room.send_replace(None);
                    drop(session);
                    if let Outcome::End(end) = outcome {
                        return end;
                    }
                }
                Err(Fail::Unauthorized) => return End::Unauthorized,
                Err(Fail::Leave) => return End::Left,
                Err(Fail::Retry) => {}
            }
            attempt += 1;
            if attempt > MAX_ATTEMPTS {
                return End::GaveUp;
            }
            log!("sala: reconectando, tentativa {attempt}");
            self.emit(Event::Reconnecting { attempt });
            if ctl.or_leave(sleep(backoff(attempt))).await.is_none() {
                return End::Left;
            }
        }
    }

    /// Token novo a cada tentativa: o de 5 min já pode ter vencido.
    async fn connect(
        &self,
        ctl: &mut Ctl,
        source: &watch::Sender<Option<NativeAudioSource>>,
    ) -> Result<Session, Fail> {
        let details = ctl
            .or_leave(self.client.connection_details(&self.slug, &self.install_id))
            .await
            .ok_or(Fail::Leave)?
            .map_err(|e| match e {
                api::Error::Unauthorized => Fail::Unauthorized,
                _ => Fail::Retry,
            })?;
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        let (room, events) = ctl
            .or_leave(Room::connect(
                &details.server_url,
                &details.participant_token,
                options,
            ))
            .await
            .ok_or(Fail::Leave)?
            .map_err(|_| Fail::Retry)?;

        // Fila zero: entrega direto. Com fila, o mudo demoraria o tamanho dela pra valer.
        let src = NativeAudioSource::new(AudioSourceOptions::default(), RATE, 1, 0);
        let track =
            LocalAudioTrack::create_audio_track("microphone", RtcAudioSource::Native(src.clone()));
        let options = TrackPublishOptions {
            source: TrackSource::Microphone,
            ..Default::default()
        };
        let mic = match room
            .local_participant()
            .publish_track(LocalTrack::Audio(track), options)
            .await
        {
            Ok(p) => p,
            Err(_) => {
                let _ = room.close().await;
                return Err(Fail::Retry);
            }
        };
        // Publica mudo antes de a faixa receber o primeiro bloco.
        apply_mute(&ctl.engine, Some(&mic), ctl.muted);
        source.send_replace(Some(src));
        self.room.send_replace(Some(room.local_participant()));

        Ok(Session {
            room,
            events,
            mic,
            voices: HashMap::new(),
            last: Vec::new(),
            sent_muted: String::new(),
            reconnecting: false,
            speaking: HashSet::new(),
            watched: HashMap::new(),
            peeks: HashMap::new(),
            cameras: HashMap::new(),
        })
    }

    async fn serve(&self, s: &mut Session, ctl: &mut Ctl) -> Outcome {
        s.subscribe_all(ctl.watching.as_deref());
        s.peek_all(ctl.watching.as_deref());
        s.send_muted(&ctl.people);
        self.refresh(s);
        let mut debounce: Option<Instant> = None;
        loop {
            tokio::select! {
                event = s.events.recv() => {
                    // Canal fechado sem `Disconnected`: queda sem motivo, transitória.
                    let Some(event) = event else { return Outcome::Dropped };
                    if let Some(outcome) = self.on_event(s, ctl, event) {
                        return outcome;
                    }
                }
                Some((who, on)) = ctl.speech_rx.recv() => {
                    if on {
                        s.speaking.insert(who);
                    } else {
                        s.speaking.remove(&who);
                    }
                    self.refresh(s);
                }
                cmd = ctl.rx.recv() => match cmd {
                    Some(Cmd::Mute(m)) => {
                        ctl.muted = m;
                        apply_mute(&ctl.engine, Some(&s.mic), m);
                        self.refresh(s);
                    }
                    Some(Cmd::MutedPeople(p)) => {
                        ctl.people = p;
                        debounce = Some(Instant::now() + MUTED_DEBOUNCE);
                    }
                    Some(Cmd::Watch(w)) => s.watch(ctl, w),
                    #[cfg(test)]
                    Some(Cmd::Simulate(scenario)) => {
                        let _ = s.room.simulate_scenario(scenario).await;
                    }
                    None | Some(Cmd::Leave) => return Outcome::End(s.close().await),
                },
                Some(sid) = ctl.peek_rx.recv() => s.end_peek(ctl, &sid),
                _ = wait_until(debounce) => {
                    debounce = None;
                    s.send_muted(&ctl.people);
                }
                // A interface soltou o receptor: sai limpo, sem ninguém pra avisar.
                _ = self.tx.closed() => return Outcome::End(s.close().await),
            }
        }
    }

    fn on_event(&self, s: &mut Session, ctl: &Ctl, event: RoomEvent) -> Option<Outcome> {
        log_event(&event);
        match &event {
            RoomEvent::Disconnected { reason } => {
                return Some(match definitive(*reason) {
                    Some(end) => Outcome::End(end),
                    None => Outcome::Dropped,
                });
            }
            RoomEvent::TrackPublished { publication, .. }
                if always_subscribed(publication.source()) =>
            {
                publication.set_subscribed(true);
            }
            RoomEvent::TrackPublished {
                publication,
                participant,
            } if is_watch_track(publication.source(), &publication.name())
                && ctl.watching.as_deref() == Some(participant.identity().as_str()) =>
            {
                publication.set_subscribed(true);
            }
            // Tela nova de quem não está sendo assistido: assina só pra tirar a prévia.
            RoomEvent::TrackPublished { publication, .. }
                if publication.source() == TrackSource::Screenshare =>
            {
                publication.set_subscribed(true);
            }
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(video),
                publication,
                participant,
            } if publication.source() == TrackSource::Screenshare => {
                let sid = publication.sid().to_string();
                let identity = participant.identity().to_string();
                if ctl.watching.as_deref() == Some(identity.as_str()) {
                    s.watched
                        .insert(sid, stream_screen(video.clone(), ctl.screen.clone()));
                } else {
                    let task = peek(video.clone(), identity, sid.clone(), &self.tx, &ctl.peek_tx);
                    s.peeks.insert(sid, task);
                }
            }
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Video(video),
                publication,
                participant,
            } if publication.source() == TrackSource::Camera => {
                // Sem simulcast o SDK só reclama no log: vem a única camada que tem.
                if publication.simulcasted() {
                    publication.set_video_quality(CAMERA_QUALITY);
                }
                let identity = participant.identity().to_string();
                let task = stream_camera(video.clone(), identity.clone(), ctl.cameras.clone());
                s.cameras.insert(
                    publication.sid().to_string(),
                    Voice {
                        identity,
                        _task: task,
                    },
                );
            }
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Audio(audio),
                publication,
                participant,
            } if publication.name() == share::TRACK
                && ctl.watching.as_deref() == Some(participant.identity().as_str()) =>
            {
                let sid = publication.sid().to_string();
                s.watched.remove(&sid);
                let name = participant.name();
                let name = if name.is_empty() {
                    participant.identity().to_string()
                } else {
                    name
                };
                // Chave própria: o volume da transmissão não mexe no da voz.
                let mut sink = ctl.engine.add_voice(sid.clone(), Config::screen_key(&name));
                let mut stream = NativeAudioStream::new(audio.rtc_track(), RATE as i32, 1);
                let task = tokio::spawn(async move {
                    while let Some(frame) = stream.next().await {
                        sink.push(&frame.data);
                    }
                });
                s.watched.insert(sid, AbortOnDrop(task));
            }
            RoomEvent::TrackSubscribed {
                track: RemoteTrack::Audio(audio),
                publication,
                participant,
            } if publication.source() == TrackSource::Microphone => {
                let sid = publication.sid().to_string();
                // Solta a voz antiga antes: a mesma chave não pode sair do mixer
                // depois de a nova entrar.
                s.voices.remove(&sid);
                let identity = participant.identity().to_string();
                let name = participant.name();
                let name = if name.is_empty() {
                    identity.clone()
                } else {
                    name
                };
                s.speaking.remove(&identity);
                let mut sink = ctl.engine.add_voice(sid.clone(), name);
                let mut stream = NativeAudioStream::new(audio.rtc_track(), RATE as i32, 1);
                let speech = ctl.speech_tx.clone();
                let who = identity.clone();
                let task = tokio::spawn(async move {
                    let mut detector = Speech::default();
                    while let Some(frame) = stream.next().await {
                        if let Some(on) = detector.update(&frame.data) {
                            let _ = speech.send((who.clone(), on));
                        }
                        sink.push(&frame.data);
                    }
                });
                s.voices.insert(
                    sid,
                    Voice {
                        identity,
                        _task: AbortOnDrop(task),
                    },
                );
            }
            // O último quadro de antes de mutar não pisca no tile quando a câmera volta.
            RoomEvent::TrackUnmuted {
                participant,
                publication,
            } if publication.source() == TrackSource::Camera => {
                ctl.cameras.take(participant.identity().as_str());
            }
            RoomEvent::TrackUnsubscribed { publication, .. } => {
                let sid = publication.sid().to_string();
                if let Some(voice) = s.voices.remove(&sid) {
                    s.speaking.remove(&voice.identity);
                }
                s.cameras.remove(&sid);
                s.peeks.remove(&sid);
                if s.watched.remove(&sid).is_some()
                    && publication.source() == TrackSource::Screenshare
                {
                    ctl.screen.clear();
                }
            }
            RoomEvent::ParticipantDisconnected(p) => {
                let identity = p.identity().to_string();
                s.voices.retain(|_, v| v.identity != identity);
                s.cameras.retain(|_, c| c.identity != identity);
                s.speaking.remove(&identity);
                if ctl.watching.as_deref() == Some(identity.as_str()) {
                    s.watched.clear();
                    ctl.screen.clear();
                }
            }
            RoomEvent::Reconnecting => {
                s.reconnecting = true;
                self.emit(Event::Reconnecting { attempt: 0 });
            }
            RoomEvent::Reconnected => {
                s.reconnecting = false;
                self.emit(Event::Connected);
                // A retomada pode ter perdido assinaturas e atributos.
                s.subscribe_all(ctl.watching.as_deref());
                s.peek_all(ctl.watching.as_deref());
                s.sent_muted.clear();
                s.send_muted(&ctl.people);
            }
            RoomEvent::LocalTrackRepublished { publication, .. }
                if publication.source() == TrackSource::Microphone =>
            {
                s.mic = publication.clone();
                apply_mute(&ctl.engine, Some(&s.mic), ctl.muted);
            }
            _ => {}
        }
        if matches!(
            event,
            RoomEvent::ParticipantConnected(_)
                | RoomEvent::ParticipantDisconnected(_)
                | RoomEvent::ParticipantNameChanged { .. }
                | RoomEvent::TrackPublished { .. }
                | RoomEvent::TrackUnpublished { .. }
                | RoomEvent::LocalTrackPublished { .. }
                | RoomEvent::LocalTrackUnpublished { .. }
                | RoomEvent::LocalTrackRepublished { .. }
                | RoomEvent::TrackMuted { .. }
                | RoomEvent::TrackUnmuted { .. }
                | RoomEvent::Reconnecting
                | RoomEvent::Reconnected
        ) {
            self.refresh(s);
        }
        None
    }

    fn refresh(&self, s: &mut Session) {
        let list = s.participants();
        let list = if s.reconnecting {
            hold_remotes(list, &s.last)
        } else {
            list
        };
        if list != s.last {
            s.last.clone_from(&list);
            self.emit(Event::Participants(list));
        }
    }
}

/// Quem entrou e saiu, e cada faixa assinada ou solta, pro `concord.log`.
fn log_event(event: &RoomEvent) {
    let track = |p: &RemoteTrackPublication| format!("{:?} \"{}\"", p.source(), p.name());
    match event {
        RoomEvent::Disconnected { reason } => log!("sala: caiu ({reason:?})"),
        RoomEvent::Reconnecting => log!("sala: reconectando pelo SDK"),
        RoomEvent::Reconnected => log!("sala: reconectou pelo SDK"),
        RoomEvent::ParticipantConnected(p) => log!("sala: {} entrou", p.identity()),
        RoomEvent::ParticipantDisconnected(p) => log!("sala: {} saiu", p.identity()),
        RoomEvent::TrackSubscribed {
            publication,
            participant,
            ..
        } => log!(
            "sala: assinou {} de {}",
            track(publication),
            participant.identity()
        ),
        RoomEvent::TrackUnsubscribed {
            publication,
            participant,
            ..
        } => log!(
            "sala: soltou {} de {}",
            track(publication),
            participant.identity()
        ),
        RoomEvent::TrackSubscriptionFailed {
            participant,
            track_sid,
            error,
        } => log!(
            "sala: assinatura de {track_sid} de {} falhou: {error}",
            participant.identity()
        ),
        _ => {}
    }
}

/// O SDK solta todo mundo ao recomeçar a conexão. Sem isto, a pessoa se veria sozinha
/// na sala enquanto a rede não volta.
fn hold_remotes(list: Vec<Live>, last: &[Live]) -> Vec<Live> {
    let remotes = last.iter().filter(|p| !p.is_local).map(|p| Live {
        speaking: false,
        ..p.clone()
    });
    list.into_iter()
        .filter(|p| p.is_local)
        .chain(remotes)
        .collect()
}

impl Session {
    /// A voz e a câmera de todo mundo. Tela e `concord-app-audio` só de quem está sendo
    /// assistido.
    fn subscribe_all(&self, watching: Option<&str>) {
        for p in self.room.remote_participants().values() {
            let watched = watching == Some(p.identity().as_str());
            for publication in p.track_publications().values() {
                let watch = is_watch_track(publication.source(), &publication.name());
                if always_subscribed(publication.source()) || (watched && watch) {
                    publication.set_subscribed(true);
                }
            }
        }
    }

    /// Troca quem está sendo assistido: solta as faixas do anterior e assina as do novo.
    fn watch(&mut self, ctl: &mut Ctl, identity: Option<String>) {
        if ctl.watching == identity {
            return;
        }
        if let Some(old) = ctl.watching.take() {
            for p in self.room.remote_participants().values() {
                if p.identity().as_str() != old {
                    continue;
                }
                for publication in p.track_publications().values() {
                    if is_watch_track(publication.source(), &publication.name()) {
                        publication.set_subscribed(false);
                    }
                }
            }
        }
        self.watched.clear();
        ctl.screen.clear();
        ctl.watching = identity;
        self.subscribe_all(ctl.watching.as_deref());
        // A tela já assinada pela prévia não gera outro `TrackSubscribed`: o vídeo
        // começa daqui, e a prévia em andamento deixa de soltar a assinatura.
        let Some(who) = ctl.watching.as_deref() else {
            return;
        };
        for p in self.room.remote_participants().values() {
            if p.identity().as_str() != who {
                continue;
            }
            for (sid, publication) in p.track_publications() {
                if let (TrackSource::Screenshare, Some(RemoteTrack::Video(video))) =
                    (publication.source(), publication.track())
                {
                    self.peeks.remove(&sid.to_string());
                    let task = stream_screen(video, ctl.screen.clone());
                    self.watched.insert(sid.to_string(), task);
                }
            }
        }
    }

    /// Prévia de todas as telas no ar, menos a assistida, que já tem o vídeo.
    fn peek_all(&self, watching: Option<&str>) {
        for p in self.room.remote_participants().values() {
            if watching == Some(p.identity().as_str()) {
                continue;
            }
            for publication in p.track_publications().values() {
                if publication.source() == TrackSource::Screenshare {
                    publication.set_subscribed(true);
                }
            }
        }
    }

    /// A prévia saiu: solta a tela, a não ser que a pessoa tenha passado a ser assistida.
    fn end_peek(&mut self, ctl: &Ctl, sid: &str) {
        self.peeks.remove(sid);
        for p in self.room.remote_participants().values() {
            if ctl.watching.as_deref() == Some(p.identity().as_str()) {
                continue;
            }
            for (id, publication) in p.track_publications() {
                if id.as_str() == sid {
                    publication.set_subscribed(false);
                }
            }
        }
    }

    fn participants(&self) -> Vec<Live> {
        let me = self.room.local_participant();
        let local = live(
            me.identity().to_string(),
            me.name(),
            true,
            me.track_publications()
                .values()
                .map(|p| (p.source(), p.is_muted())),
            self.speaking.contains(LOCAL),
        );
        let remotes = self
            .room
            .remote_participants()
            .values()
            .map(|p| {
                live(
                    p.identity().to_string(),
                    p.name(),
                    false,
                    p.track_publications()
                        .values()
                        .map(|t| (t.source(), t.is_muted())),
                    self.speaking.contains(p.identity().as_str()),
                )
            })
            .collect();
        roster(local, remotes)
    }

    /// Anuncia `concord.muted` só se mudou. Em segundo plano, pra não segurar os eventos.
    fn send_muted(&mut self, people: &[String]) {
        let value = encode_muted(people);
        if value == self.sent_muted {
            return;
        }
        self.sent_muted.clone_from(&value);
        let me = self.room.local_participant();
        tokio::spawn(async move {
            let _ = me
                .set_attributes(HashMap::from([(MUTED_ATTRIBUTE.to_owned(), value)]))
                .await;
        });
    }

    /// Saída limpa: sem isso o nome fica preso na lista do servidor até o timeout.
    async fn close(&self) -> End {
        let _ = timeout(CLOSE_TIMEOUT, self.room.close()).await;
        End::Left
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn so_tela_e_audio_de_app_esperam_o_assistir() {
        assert!(is_watch_track(TrackSource::Screenshare, "screen_share"));
        assert!(is_watch_track(TrackSource::Unknown, share::TRACK));
        assert!(!is_watch_track(TrackSource::Microphone, "microphone"));
        assert!(!is_watch_track(TrackSource::Unknown, "outra"));
    }

    #[test]
    fn voz_e_camera_assinam_ao_entrar_e_o_resto_espera() {
        assert!(always_subscribed(TrackSource::Microphone));
        assert!(always_subscribed(TrackSource::Camera));
        for source in [
            TrackSource::Screenshare,
            TrackSource::ScreenshareAudio,
            TrackSource::Unknown,
        ] {
            assert!(!always_subscribed(source), "{source:?}");
        }
        // A câmera não é faixa de assistir: parar de assistir não solta ela.
        assert!(!is_watch_track(TrackSource::Camera, "camera"));
    }

    #[test]
    fn camera_guarda_o_ultimo_quadro_de_cada_pessoa() {
        let cameras = Cameras::default();
        let quadro = |w| Picture {
            width: w,
            height: 1,
            rgba: vec![0; w as usize * 4],
        };
        cameras.put("ana__1", quadro(1));
        cameras.put("ana__1", quadro(2));
        cameras.put("bia__1", quadro(3));
        assert_eq!(cameras.take("ana__1").map(|p| p.width), Some(2));
        assert!(cameras.take("ana__1").is_none(), "sem quadro novo, nada");
        assert_eq!(cameras.take("bia__1").map(|p| p.width), Some(3));
    }

    #[test]
    fn previa_fica_pequena_na_mesma_proporcao() {
        let quadro = Picture {
            width: 1920,
            height: 1080,
            rgba: vec![200; 1920 * 1080 * 4],
        };
        let p = quadro.preview();
        assert_eq!((p.width, p.height), (96, 54));
        assert_eq!(p.rgba.len(), 96 * 54 * 4);
        assert_eq!(p.rgba[0], 200, "cor preservada");
    }

    #[test]
    fn quadro_novo_substitui_o_que_nao_foi_desenhado() {
        let screen = Screen::default();
        let quadro = |w| Picture {
            width: w,
            height: 1,
            rgba: vec![0; w as usize * 4],
        };
        screen.put(quadro(1));
        screen.put(quadro(2));
        assert_eq!(screen.take().map(|p| p.width), Some(2));
        assert!(screen.take().is_none(), "sem quadro novo, nada");
    }

    #[tokio::test]
    async fn anel_local_segue_o_limitador_e_so_emite_quando_muda() {
        let (frames, mic) = mpsc::channel(8);
        let meter = Meter::new();
        let (_source, source_rx) = watch::channel(None);
        let (speech_tx, mut speech) = mpsc::unbounded_channel();
        let task = tokio::spawn(forward_mic(mic, meter.clone(), source_rx, speech_tx));
        for open in [false, true, true, false, false, true] {
            meter.update(-30.0, open);
            frames.send(vec![0; 480]).await.unwrap();
            tokio::task::yield_now().await;
        }
        drop(frames);
        task.await.unwrap();
        let mut got = Vec::new();
        while let Ok((who, on)) = speech.try_recv() {
            assert_eq!(who, LOCAL);
            got.push(on);
        }
        assert_eq!(got, [true, false, true]);
    }

    #[test]
    fn fala_acende_no_primeiro_bloco_e_segura_a_queda() {
        let voz = [800i16; 480];
        let silencio = [0i16; 480];
        let mut s = Speech::default();
        assert_eq!(s.update(&silencio), None);
        assert_eq!(s.update(&voz), Some(true), "10 ms depois de chegar som");
        assert_eq!(s.update(&voz), None);
        for _ in 0..14 {
            assert_eq!(s.update(&silencio), None);
        }
        assert_eq!(s.update(&silencio), Some(false), "150 ms de silêncio");
        assert_eq!(s.update(&voz), Some(true));
    }

    #[test]
    fn pausa_curta_entre_palavras_nao_apaga() {
        let voz = [800i16; 480];
        let silencio = [0i16; 480];
        let mut s = Speech::default();
        s.update(&voz);
        for _ in 0..3 {
            for _ in 0..10 {
                assert_eq!(s.update(&silencio), None);
            }
            assert_eq!(s.update(&voz), None);
        }
    }

    #[test]
    fn nivel_entre_os_limiares_mantem_o_estado() {
        let meio = [245i16; 480];
        let voz = [800i16; 480];
        let mut s = Speech::default();
        assert_eq!(s.update(&meio), None, "não acende");
        s.update(&voz);
        for _ in 0..40 {
            assert_eq!(s.update(&meio), None, "não apaga");
        }
    }

    fn vivo(name: &str, is_local: bool, speaking: bool) -> Live {
        Live {
            identity: format!("{name}__1"),
            name: name.into(),
            is_local,
            muted: false,
            camera: false,
            screen_share: false,
            speaking,
        }
    }

    #[test]
    fn reconectando_mantem_quem_estava_na_sala() {
        let antes = [vivo("eu", true, false), vivo("ana", false, true)];
        let agora = vec![vivo("eu", true, false)];
        let lista = hold_remotes(agora, &antes);
        assert_eq!(lista.len(), 2);
        assert!(lista[0].is_local);
        assert_eq!(lista[1].name, "ana");
        assert!(!lista[1].speaking, "ninguém fala sem conexão");
    }

    #[test]
    fn retomada_sem_soltar_ninguem_nao_duplica() {
        let antes = [vivo("eu", true, false), vivo("ana", false, false)];
        let lista = hold_remotes(antes.to_vec(), &antes);
        assert_eq!(lista.len(), 2);
    }

    #[test]
    fn motivos_definitivos() {
        use DisconnectReason::*;
        assert_eq!(definitive(ClientInitiated), Some(End::Left));
        assert_eq!(definitive(DuplicateIdentity), Some(End::Duplicate));
        assert_eq!(definitive(ParticipantRemoved), Some(End::Removed));
        assert_eq!(definitive(RoomDeleted), Some(End::RoomClosed));
        assert_eq!(definitive(RoomClosed), Some(End::RoomClosed));
    }

    #[test]
    fn motivos_transitorios() {
        use DisconnectReason::*;
        for r in [
            UnknownReason,
            ServerShutdown,
            StateMismatch,
            JoinFailure,
            Migration,
            SignalClose,
            ConnectionTimeout,
            MediaFailure,
        ] {
            assert_eq!(definitive(r), None, "{r:?}");
        }
    }

    #[test]
    fn espera_dobra_ate_30s() {
        let ms: Vec<u128> = (1..=MAX_ATTEMPTS + 1)
            .map(|n| backoff(n).as_millis())
            .collect();
        assert_eq!(
            ms,
            [
                2000, 4000, 8000, 16000, 30000, 30000, 30000, 30000, 30000, 30000, 30000
            ]
        );
        // Sem estouro em contagem absurda.
        assert_eq!(backoff(u32::MAX).as_millis(), 30_000);
    }

    #[test]
    fn muted_ordenado_sem_repetir() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(encode_muted(&[]), "");
        assert_eq!(
            encode_muted(&n(&["zeca", "ana", "zeca", "bia"])),
            "ana,bia,zeca"
        );
    }

    fn pessoa(id: &str, name: &str, pubs: &[(TrackSource, bool)]) -> Live {
        live(id.into(), name.into(), false, pubs.iter().copied(), false)
    }

    #[test]
    fn sem_mic_conta_como_mudo() {
        assert!(pessoa("a__1", "a", &[]).muted);
        assert!(pessoa("a__1", "a", &[(TrackSource::Microphone, true)]).muted);
        assert!(!pessoa("a__1", "a", &[(TrackSource::Microphone, false)]).muted);
    }

    #[test]
    fn camera_tela_e_nome() {
        let p = pessoa(
            "a__1",
            "",
            &[
                (TrackSource::Microphone, false),
                (TrackSource::Camera, true),
                (TrackSource::Screenshare, true),
                (TrackSource::ScreenshareAudio, false),
            ],
        );
        assert_eq!(p.name, "a__1");
        assert!(!p.camera, "câmera muda não conta");
        assert!(p.screen_share, "tela conta mesmo pausada");
        let c = pessoa("a__1", "a", &[(TrackSource::Camera, false)]);
        assert!(c.camera);
    }

    #[test]
    fn local_primeiro_e_ordem_estavel() {
        let local = live("eu__1".into(), "Zé".into(), true, [], false);
        let all = roster(
            local.clone(),
            vec![
                pessoa("b__1", "bia", &[]),
                pessoa("a__2", "Ana", &[]),
                pessoa("a__1", "Ana", &[]),
            ],
        );
        let ids: Vec<_> = all.iter().map(|p| p.identity.as_str()).collect();
        assert_eq!(ids, ["eu__1", "a__1", "a__2", "b__1"]);
        assert!(all[0].is_local);
    }
}
