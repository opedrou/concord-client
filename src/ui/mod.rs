//! Telas e estado do app. A rede roda em `Task`; o resto é síncrono.
mod avatar;
mod screens;
mod sidebar;
mod sounds;
mod stage;
pub mod theme;
mod voice;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{image, operation, row, stack};
use iced::{Color, Element, Event, Subscription, Task, Theme, event, mouse, time, window};
use voice::{ActiveCall, Boot, Probe};

use crate::api::{
    self, AddressError, Channel, ChannelKind, Client, Error, Member, Presence, Secret, User,
};
use crate::audio::{self, Devices, Effect, Engine, MANUAL_MIN_DB, Meter, NoiseLevel};
use crate::call::{self, Call, End};
use crate::config::{Config, SIDEBAR_MAX, SIDEBAR_MIN};
use crate::session;
use crate::share::{Quality, Source, Thumb};
use crate::update::Check;

const POLL: Duration = Duration::from_secs(4);
/// De quanto em quanto tempo confere se saiu versão nova, com o app aberto.
const CONFERE_UPDATE: Duration = Duration::from_secs(15 * 60);
/// Tempo na tela principal pra versão aberta contar como boa (`update::healthy`). A
/// primeira conferência de atualização espera isso, pra não trocar a versão ainda em teste.
const DE_PE: Duration = Duration::from_secs(10);
/// Falhas seguidas de presença até o rodapé trocar "Online" por "Sem conexão".
const FALHAS_ATE_AVISAR: u8 = 3;

/// Tempo que a saída espera o `Ended` antes de fechar a janela mesmo assim.
const ESPERA_AO_FECHAR: Duration = Duration::from_secs(2);
/// Relógio do anel de fala: o hold é de 400 ms, então 100 ms apaga sem atraso visível.
const QUADRO: Duration = Duration::from_millis(100);
/// Relógio do medidor de entrada: uns 30 quadros por segundo.
const MEDIDOR: Duration = Duration::from_millis(33);
/// Relógio da transmissão assistida: 60 por segundo, pra não atrasar a de 30 fps.
const TELA: Duration = Duration::from_millis(16);
/// Relógio da prévia da própria transmissão, no ritmo em que ela é gerada.
const PREVIA: Duration = Duration::from_millis(200);
/// Relógio das câmeras: 30 por segundo, o ritmo de uma câmera.
const CAMERA: Duration = Duration::from_millis(33);

// Quantas vezes `persist` rodou nesta thread de teste.
#[cfg(test)]
thread_local! {
    static SAVES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

const ID_SERVIDOR: &str = "campo-servidor";
const ID_USUARIO: &str = "campo-usuario";

pub struct App {
    config: Config,
    screen: Screen,
}

enum Screen {
    Connecting,
    Server(ServerForm),
    Login(LoginForm),
    Offline(String),
    Main(Box<Main>),
}

struct ServerForm {
    input: String,
    error: Option<String>,
    busy: bool,
}

struct LoginForm {
    server: String,
    username: String,
    password: Secret,
    error: Option<String>,
    busy: bool,
}

struct Main {
    server: String,
    client: Client,
    user: User,
    channels: Vec<Channel>,
    presence: Presence,
    polling: bool,
    failures: u8,
    /// Username -> caminho do avatar.
    avatar_paths: HashMap<String, String>,
    /// Caminho -> imagem. O caminho muda quando a foto muda, então nunca envelhece.
    // ponytail: cache só na memória, baixa de novo a cada abertura; disco se pesar.
    avatars: HashMap<String, (image::Handle, Color)>,
    requested: HashSet<String>,
    settings: Option<Tab>,
    resizing: bool,
    vault_ok: bool,
    call: Option<ActiveCall>,
    next_call: u64,
    /// Aviso curto na home: motivo de a call ter acabado ou de não ter aberto.
    notice: Option<String>,
    /// Lista do sistema, lida ao abrir a aba Voz.
    devices: Option<Devices>,
    /// Motor de teste da aba Voz; só existe sem call.
    probe: Probe,
    next_probe: u64,
    /// Última leitura do mic, em dBFS, e se o limitador está aberto.
    level: f32,
    open: bool,
    /// Janela que pediu pra fechar e espera a call terminar.
    closing: Option<window::Id>,
    /// Modo em que a janela está de verdade: segue a tela cheia da transmissão assistida.
    full_window: bool,
    /// Versão nova já baixada e instalada, que vale na próxima abertura.
    update: Option<String>,
    /// Última conferência de atualização, pra linha do Sobre.
    check: Option<Check>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tab {
    Server,
    Account,
    Voice,
    About,
}

#[derive(Clone, Debug)]
pub enum Msg {
    Connected {
        server: String,
        cookie: Option<Secret>,
        result: Result<User, Error>,
    },
    ServerInput(String),
    ServerSubmit,
    Retry,
    ChangeServer,
    Username(String),
    Password(Secret),
    LoginSubmit,
    LoginDone(Result<(Secret, User), Error>),
    VaultSaved(bool),
    Channels(Result<Vec<Channel>, Error>),
    Members(Result<Vec<Member>, Error>),
    Tick,
    PollIf(Option<bool>),
    Presence(Result<Presence, Error>),
    Avatar(String, Option<(image::Handle, Color)>),
    ToggleMic,
    ToggleDeafen,
    JoinChannel(String),
    EngineStarted(u64, Result<Boot, String>),
    CallEvent(u64, call::Event),
    /// Relógio do anel de fala.
    Frame(Instant),
    Leave,
    OpenVolume(String),
    CloseVolume,
    Volume(String, u8),
    Master(u8),
    /// Solta um slider: grava na config.
    VolumeDone,
    /// Botão de tela da barra.
    ShareButton,
    ShareQuality(Quality),
    /// Interruptor "Transmitir áudio".
    ShareAudio(bool),
    ShareStart,
    ShareStop,
    /// "Trocar" no tile da própria transmissão: outra tela sem parar.
    ShareSwap,
    /// Telas e janelas lidas pro seletor do Windows, com a miniatura.
    ShareSources(Vec<(Source, Option<Thumb>)>),
    ShareSource(Source),
    /// Aba do seletor: `true` em Aplicativos.
    ShareTab(bool),
    /// Assiste a transmissão de alguém, pela identity; `None` volta pro palco.
    Watch(Option<String>),
    /// Relógio da transmissão assistida: pega o quadro mais novo.
    ScreenTick,
    /// Quadro da transmissão já na GPU.
    ScreenReady(Option<image::Allocation>),
    /// Relógio da prévia própria e a prévia já na GPU.
    PreviewTick,
    PreviewReady(Option<image::Allocation>),
    /// Relógio das câmeras e o quadro de uma, pela identity, já na GPU.
    CameraTick,
    CameraReady(String, Option<image::Allocation>),
    /// A janela ganhou ou perdeu o foco.
    Focus(bool),
    /// Põe a transmissão assistida em tela cheia, pelo menu do botão direito.
    Fullscreen,
    /// Clique na transmissão assistida: alterna entre o tile e grande com as pessoas do lado.
    FocusScreen,
    /// Mouse perto ou longe do pé do palco: mostra ou esconde a barra.
    BarNear(bool),
    /// Mouse no palco: onde está e se está perto do pé.
    StageMouse(iced::Point, bool),
    /// Andamento e fim da conferência de atualização.
    Updated(Check),
    /// Clique no ícone de atualização: abre a versão nova agora.
    RestartForUpdate,
    /// Hora de conferir de novo se saiu versão nova, ou o botão do Sobre.
    CheckUpdate,
    /// A versão aberta ficou de pé na tela principal.
    Healthy,
    /// Seta do botão de transmitir: abre e fecha o menu.
    ShareMenu,
    /// Liga ou desliga o áudio da transmissão no ar.
    ShareSound,
    Input(Option<String>),
    Output(Option<String>),
    Noise(NoiseLevel),
    /// Interruptor "Determinar automaticamente".
    AutoSensitivity(bool),
    /// Marcador do limiar manual, em dBFS.
    Threshold(f32),
    /// Interruptor do ganho de entrada automático.
    AutoGain(bool),
    /// Ganho de entrada manual, em dB.
    Gain(f32),
    /// Relógio do medidor de entrada.
    MeterTick,
    ProbeStarted(u64, Result<Boot, String>),
    Devices(Devices),
    CloseRequested(window::Id),
    CloseNow(window::Id),
    DragStart,
    DragMove(f32),
    DragEnd,
    OpenSettings(Tab),
    CloseSettings,
    Logout,
    FocusNext,
    FocusPrevious,
    Escape,
}

/// Telas e janelas pro seletor do Windows. Listar e tirar as miniaturas bloqueia: lê fora
/// da thread da interface.
fn read_sources(call: &mut ActiveCall) -> Task<Msg> {
    call.share_sources = None;
    Task::future(async {
        let sources = tokio::task::spawn_blocking(crate::share::sources).await;
        Msg::ShareSources(sources.unwrap_or_default())
    })
}

/// Confere atualização. Avisa quando começa a baixar e de novo no fim.
fn check_update() -> Task<Msg> {
    Task::run(
        iced::stream::channel(2, async |mut done| {
            let mut early = done.clone();
            let end = crate::update::run(move |version| {
                let _ = early.try_send(Check::Downloading(version));
            })
            .await;
            let _ = done.try_send(end);
        }),
        Msg::Updated,
    )
}

fn connect(server: String) -> Task<Msg> {
    Task::future(async move {
        let cookie = session::load(server.clone()).await;
        let result = Client::new(&server, cookie.clone()).me().await;
        Msg::Connected {
            server,
            cookie,
            result,
        }
    })
}

fn address_error(e: AddressError) -> &'static str {
    match e {
        AddressError::Empty => "Digite o endereço do servidor.",
        AddressError::Invalid => "Esse endereço não é válido.",
        AddressError::NotHttps => "O endereço precisa começar com https://.",
    }
}

fn connect_error(e: Error) -> String {
    match e {
        Error::NotConcord => "Esse endereço não é um servidor Concord.".to_owned(),
        Error::Server(code) => format!("O servidor respondeu com erro ({code}). Tente de novo."),
        _ => "Não foi possível conectar. Tente de novo.".to_owned(),
    }
}

fn login_error(e: Error) -> String {
    match e {
        Error::BadCredentials => "Usuário ou senha inválidos.".to_owned(),
        Error::TooManyAttempts(s) if s > 60 => {
            format!(
                "Muitas tentativas. Tente de novo em {} min.",
                s.div_ceil(60)
            )
        }
        Error::TooManyAttempts(s) => format!("Muitas tentativas. Tente de novo em {s} s."),
        e => connect_error(e),
    }
}

impl App {
    pub fn boot() -> (Self, Task<Msg>) {
        let config = Config::load();
        match config.server.clone() {
            Some(server) => (
                Self {
                    config,
                    screen: Screen::Connecting,
                },
                connect(server),
            ),
            None => {
                let screen = Screen::Server(ServerForm {
                    input: String::new(),
                    error: None,
                    busy: false,
                });
                (Self { config, screen }, operation::focus(ID_SERVIDOR))
            }
        }
    }

    pub fn theme(&self) -> Theme {
        theme::theme()
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let mut subs = vec![
            keyboard::listen().filter_map(keys),
            window::close_requests().map(Msg::CloseRequested),
            event::listen_with(focus),
        ];
        if let Screen::Main(m) = &self.screen {
            subs.push(time::every(POLL).map(|_| Msg::Tick));
            subs.push(time::every(CONFERE_UPDATE).map(|_| Msg::CheckUpdate));
            if m.call.as_ref().is_some_and(ActiveCall::holding) {
                subs.push(time::every(QUADRO).map(Msg::Frame));
            }
            if m.call.as_ref().is_some_and(|c| c.watching.is_some()) {
                subs.push(time::every(TELA).map(|_| Msg::ScreenTick));
            }
            if m.call.as_ref().is_some_and(ActiveCall::preview_clock) {
                subs.push(time::every(PREVIA).map(|_| Msg::PreviewTick));
            }
            if m.call.as_ref().is_some_and(ActiveCall::camera_clock) {
                subs.push(time::every(CAMERA).map(|_| Msg::CameraTick));
            }
            if m.meter_clock() {
                subs.push(time::every(MEDIDOR).map(|_| Msg::MeterTick));
            }
            if m.resizing {
                subs.push(event::listen_with(drag));
            }
        }
        Subscription::batch(subs)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        match &self.screen {
            Screen::Connecting => screens::connecting(),
            Screen::Server(f) => screens::server(f),
            Screen::Login(f) => screens::login(f),
            Screen::Offline(server) => screens::offline(server),
            Screen::Main(m) => {
                if let Some(call) = m.call.as_ref().filter(|c| c.fullscreen()) {
                    return stage::full(call, &self.config);
                }
                let content = match &m.call {
                    Some(call) => stage::view(m, call, &self.config),
                    None => screens::home(&m.user.username, m.notice.as_deref()),
                };
                let base = row![
                    sidebar::view(m, &self.config),
                    sidebar::resize_handle(),
                    content,
                ];
                match m.settings {
                    Some(tab) => stack![base, screens::settings(m, &self.config, tab)].into(),
                    None => base.into(),
                }
            }
        }
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        let task = self.handle(msg);
        Task::batch([task, self.sync_window()])
    }

    /// A janela sai da tela cheia por qualquer caminho que pare de assistir: botão, Esc,
    /// a pessoa parar de transmitir ou a call acabar.
    fn sync_window(&mut self) -> Task<Msg> {
        let watching = matches!(&self.screen, Screen::Main(m)
            if m.call.as_ref().is_some_and(|c| c.watching.is_some()));
        crate::awake::keep(watching);
        let Screen::Main(m) = &mut self.screen else {
            return Task::none();
        };
        let want = m.call.as_ref().is_some_and(ActiveCall::fullscreen);
        if want == m.full_window {
            return Task::none();
        }
        m.full_window = want;
        let mode = if want {
            window::Mode::Fullscreen
        } else {
            window::Mode::Windowed
        };
        window::latest().and_then(move |id| window::set_mode(id, mode))
    }

    fn handle(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Connected {
                server,
                cookie,
                result,
            } => self.connected(server, cookie, result),
            Msg::ServerInput(input) => {
                if let Screen::Server(f) = &mut self.screen {
                    f.input = input;
                    f.error = None;
                }
                Task::none()
            }
            Msg::ServerSubmit => self.submit_server(),
            Msg::Retry => match &self.screen {
                Screen::Offline(server) => {
                    let server = server.clone();
                    self.screen = Screen::Connecting;
                    connect(server)
                }
                _ => Task::none(),
            },
            Msg::ChangeServer => self.change_server(),
            Msg::Username(username) => {
                if let Screen::Login(f) = &mut self.screen {
                    f.username = username;
                }
                Task::none()
            }
            Msg::Password(password) => {
                if let Screen::Login(f) = &mut self.screen {
                    f.password = password;
                }
                Task::none()
            }
            Msg::LoginSubmit => self.submit_login(),
            Msg::LoginDone(result) => self.login_done(result),
            Msg::VaultSaved(ok) => {
                if let Screen::Main(m) = &mut self.screen {
                    m.vault_ok = ok;
                }
                Task::none()
            }
            Msg::Channels(Ok(channels)) => {
                if let Screen::Main(m) = &mut self.screen {
                    m.channels = channels;
                }
                Task::none()
            }
            Msg::Channels(Err(e)) => self.main_failed(e),
            Msg::Members(Ok(members)) => {
                let Screen::Main(m) = &mut self.screen else {
                    return Task::none();
                };
                m.avatar_paths.extend(
                    members
                        .into_iter()
                        .filter_map(|p| Some((p.username, p.avatar_url?))),
                );
                m.fetch_avatars()
            }
            // Sem a lista, os avatares caem nas iniciais.
            Msg::Members(Err(_)) => Task::none(),
            Msg::Tick => window::oldest()
                .and_then(window::is_minimized)
                .map(Msg::PollIf),
            Msg::PollIf(minimized) => {
                let Screen::Main(m) = &mut self.screen else {
                    return Task::none();
                };
                // Wayland não informa se minimizou: `None` segue consultando.
                if minimized == Some(true) || m.polling {
                    return Task::none();
                }
                m.polling = true;
                let client = m.client.clone();
                Task::future(async move { Msg::Presence(client.presence().await) })
            }
            Msg::Presence(result) => self.presence(result),
            Msg::Avatar(path, handle) => {
                if let (Screen::Main(m), Some(handle)) = (&mut self.screen, handle) {
                    m.avatars.insert(path, handle);
                }
                Task::none()
            }
            Msg::ToggleMic => {
                self.config.toggle_mic();
                self.persist();
                self.apply_mute();
                Task::none()
            }
            Msg::ToggleDeafen => {
                self.config.toggle_deafen();
                self.persist();
                self.apply_mute();
                Task::none()
            }
            Msg::JoinChannel(slug) => self.join_channel(slug),
            Msg::EngineStarted(id, result) => self.engine_started(id, result),
            Msg::CallEvent(id, event) => self.call_event(id, event),
            Msg::Frame(now) => {
                if let Some(call) = self.call_mut() {
                    call.tick(now);
                }
                Task::none()
            }
            Msg::Leave => {
                self.leave_call();
                Task::none()
            }
            Msg::OpenVolume(name) => {
                if let Some(call) = self.call_mut() {
                    call.volume_panel = Some(name);
                    call.volume_at = call.cursor;
                    call.share_panel = false;
                    call.share_menu = false;
                }
                Task::none()
            }
            Msg::CloseVolume => {
                self.close_volume();
                Task::none()
            }
            Msg::Volume(name, slider) => {
                let muted = self.config.set_volume(&name, slider);
                if let Some(call) = self.call_mut() {
                    if let (Some(muted), Some(c)) = (muted, &call.call) {
                        c.set_muted_people(muted);
                    }
                    if let Some(engine) = &call.engine {
                        engine.set_person_volume(&name, slider);
                    }
                }
                Task::none()
            }
            Msg::Master(slider) => {
                self.config.master_volume = slider;
                if let Some(engine) = self.engine() {
                    engine.set_master(slider);
                }
                Task::none()
            }
            Msg::VolumeDone => {
                self.persist();
                Task::none()
            }
            Msg::ShareButton => {
                let Some(call) = self.call_mut() else {
                    return Task::none();
                };
                call.share_button();
                if !(cfg!(windows) && call.share_panel) {
                    return Task::none();
                }
                read_sources(call)
            }
            // No Linux a própria captura abre o portal; no Windows abre o nosso seletor.
            Msg::ShareSwap => {
                let Some(call) = self.call_mut() else {
                    return Task::none();
                };
                if !cfg!(windows) {
                    call.swap_share();
                    return Task::none();
                }
                call.share_panel = true;
                call.share_menu = false;
                call.volume_panel = None;
                read_sources(call)
            }
            Msg::ShareSources(sources) => {
                if let Some(call) = self.call_mut() {
                    call.set_share_sources(sources);
                }
                Task::none()
            }
            Msg::ShareSource(source) => {
                if let Some(call) = self.call_mut() {
                    call.share_source = Some(source);
                }
                Task::none()
            }
            Msg::ShareTab(windows) => {
                if let Some(call) = self.call_mut() {
                    call.set_share_tab(windows);
                }
                Task::none()
            }
            Msg::ShareQuality(quality) => {
                self.config.share_quality = quality;
                self.persist();
                Task::none()
            }
            Msg::ShareAudio(on) => {
                self.config.share_audio = on;
                self.persist();
                Task::none()
            }
            Msg::ShareStart => {
                let quality = self.config.share_quality;
                let audio = self.config.share_audio;
                if let Some(call) = self.call_mut() {
                    call.start_share(quality, audio);
                }
                Task::none()
            }
            Msg::ShareMenu => {
                if let Some(call) = self.call_mut() {
                    call.share_menu = !call.share_menu;
                }
                Task::none()
            }
            Msg::ShareSound => {
                if let Some(call) = self.call_mut() {
                    call.toggle_share_sound();
                }
                Task::none()
            }
            Msg::ShareStop => {
                if let Some(call) = self.call_mut() {
                    call.stop_share();
                }
                Task::none()
            }
            Msg::Input(name) => {
                self.config.input = name.clone();
                self.persist();
                if let Some(engine) = self.engine() {
                    engine.set_input(name);
                }
                Task::none()
            }
            Msg::Output(name) => {
                self.config.output = name.clone();
                self.persist();
                if let Some(engine) = self.engine() {
                    engine.set_output(name);
                }
                Task::none()
            }
            Msg::Noise(level) => {
                self.config.noise = level;
                self.persist();
                if let Some(engine) = self.engine() {
                    engine.set_noise(level);
                }
                Task::none()
            }
            Msg::AutoSensitivity(auto) => {
                self.config.set_auto(auto);
                self.persist();
                self.apply_sensitivity();
                Task::none()
            }
            // Grava na config só ao soltar o slider (`VolumeDone`).
            Msg::Threshold(db) => {
                self.config.set_threshold(db);
                self.apply_sensitivity();
                Task::none()
            }
            Msg::AutoGain(auto) => {
                self.config.set_auto_gain(auto);
                self.persist();
                self.apply_gain();
                Task::none()
            }
            // Como o limiar: grava ao soltar o slider.
            Msg::Gain(db) => {
                self.config.set_gain(db);
                self.apply_gain();
                Task::none()
            }
            Msg::MeterTick => {
                if let Screen::Main(m) = &mut self.screen {
                    let reading = m.meter().map(|x| (x.level_db(), x.open()));
                    if let Some((db, open)) = reading {
                        m.level = if db.is_nan() { MANUAL_MIN_DB } else { db };
                        m.open = open;
                    }
                }
                Task::none()
            }
            Msg::ProbeStarted(id, result) => {
                if let Screen::Main(m) = &mut self.screen
                    && matches!(m.probe, Probe::Opening(open) if open == id)
                {
                    // Sem MicFrames, os blocos do mic só são descartados pelo motor.
                    m.probe = match result.ok().and_then(|boot| boot.take()) {
                        Some((engine, _frames)) => {
                            let meter = engine.meter();
                            Probe::On(engine, meter)
                        }
                        None => Probe::Failed,
                    };
                }
                // Resposta de uma abertura já cancelada: o motor cai aqui.
                Task::none()
            }
            Msg::Devices(devices) => {
                if let Screen::Main(m) = &mut self.screen {
                    m.devices = Some(devices);
                }
                Task::none()
            }
            Msg::CloseRequested(id) => self.close_requested(id),
            Msg::CloseNow(id) => window::close(id),
            Msg::DragStart => {
                if let Screen::Main(m) = &mut self.screen {
                    m.resizing = true;
                }
                Task::none()
            }
            Msg::DragMove(x) => {
                self.config.sidebar_width = x.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
                Task::none()
            }
            Msg::DragEnd => {
                if let Screen::Main(m) = &mut self.screen {
                    m.resizing = false;
                }
                self.persist();
                Task::none()
            }
            Msg::OpenSettings(tab) => {
                if let Screen::Main(m) = &mut self.screen {
                    m.settings = Some(tab);
                }
                if tab != Tab::Voice {
                    self.stop_probe();
                    return Task::none();
                }
                // `devices()` bloqueia: lê fora da thread da interface.
                let devices = Task::future(async {
                    let devices = tokio::task::spawn_blocking(audio::devices).await;
                    Msg::Devices(devices.unwrap_or_default())
                });
                Task::batch([devices, self.start_probe()])
            }
            Msg::CloseSettings => {
                self.close_settings();
                Task::none()
            }
            Msg::Escape => {
                match &mut self.screen {
                    Screen::Main(m) if m.settings.is_some() => self.close_settings(),
                    Screen::Main(m) if m.call.as_ref().is_some_and(ActiveCall::fullscreen) => {
                        if let Some(c) = &mut m.call {
                            c.view = voice::View::Focus;
                        }
                    }
                    _ => self.close_volume(),
                }
                Task::none()
            }
            Msg::Watch(identity) => {
                if let Some(call) = self.call_mut() {
                    call.watch(identity);
                }
                Task::none()
            }
            Msg::ScreenTick => match self.call_mut().and_then(ActiveCall::pull_screen) {
                // Handle novo direto no widget pisca enquanto sobe pra GPU: só troca
                // depois do `allocate`.
                Some(handle) => image::allocate(handle).map(|r| Msg::ScreenReady(r.ok())),
                None => Task::none(),
            },
            Msg::ScreenReady(allocation) => {
                if let Some(call) = self.call_mut() {
                    call.screen_ready(allocation);
                }
                Task::none()
            }
            // Mesmo cuidado da transmissão assistida: só troca depois do `allocate`.
            Msg::PreviewTick => match self.call_mut().and_then(ActiveCall::pull_own) {
                Some(handle) => image::allocate(handle).map(|r| Msg::PreviewReady(r.ok())),
                None => Task::none(),
            },
            Msg::PreviewReady(allocation) => {
                if let Some(call) = self.call_mut() {
                    call.own_ready(allocation);
                }
                Task::none()
            }
            // Mesmo cuidado, uma câmera por vez: só troca depois do `allocate`.
            Msg::CameraTick => {
                let frames = self.call_mut().map(ActiveCall::pull_cameras);
                Task::batch(frames.into_iter().flatten().map(|(identity, handle)| {
                    image::allocate(handle).map(move |r| Msg::CameraReady(identity.clone(), r.ok()))
                }))
            }
            Msg::CameraReady(identity, allocation) => {
                if let Some(call) = self.call_mut() {
                    call.camera_ready(identity, allocation);
                }
                Task::none()
            }
            Msg::Focus(focused) => {
                if let Some(call) = self.call_mut() {
                    call.set_focus(focused);
                }
                Task::none()
            }
            Msg::Fullscreen => {
                if let Some(call) = self.call_mut().filter(|c| c.watching.is_some()) {
                    call.view = voice::View::Full;
                    call.volume_panel = None;
                }
                Task::none()
            }
            Msg::FocusScreen => {
                if let Some(call) = self.call_mut().filter(|c| c.watching.is_some()) {
                    call.view = match call.view {
                        voice::View::Tile => voice::View::Focus,
                        _ => voice::View::Tile,
                    };
                }
                Task::none()
            }
            Msg::BarNear(near) => {
                if let Some(call) = self.call_mut() {
                    call.bar_near = near;
                }
                Task::none()
            }
            Msg::Updated(check) => {
                if let Screen::Main(m) = &mut self.screen
                    && check != Check::Busy
                {
                    if let Check::Ready(version) = &check {
                        m.update = Some(version.clone());
                    }
                    m.check = Some(check);
                }
                Task::none()
            }
            // Com call, sai pelo mesmo caminho de fechar a janela, que deixa a sala limpo.
            Msg::RestartForUpdate => match &self.screen {
                Screen::Main(m) if m.update.is_some() && crate::update::restart() => {
                    window::latest().and_then(|id| Task::done(Msg::CloseRequested(id)))
                }
                _ => Task::none(),
            },
            Msg::CheckUpdate => match &mut self.screen {
                Screen::Main(m) if !m.checking() => {
                    m.check = Some(Check::Searching);
                    check_update()
                }
                _ => Task::none(),
            },
            Msg::Healthy => {
                crate::update::healthy();
                Task::done(Msg::CheckUpdate)
            }
            Msg::StageMouse(at, near) => {
                if let Some(call) = self.call_mut() {
                    call.cursor = at;
                    call.bar_near = near;
                }
                Task::none()
            }
            Msg::Logout => self.logout(),
            Msg::FocusNext => operation::focus_next(),
            Msg::FocusPrevious => operation::focus_previous(),
        }
    }

    fn call_mut(&mut self) -> Option<&mut ActiveCall> {
        match &mut self.screen {
            Screen::Main(m) => m.call.as_mut(),
            _ => None,
        }
    }

    /// Motor da call ou, na aba Voz sem call, o de teste: mudar mic ou limiar vale pros dois.
    fn engine(&self) -> Option<&Engine> {
        let Screen::Main(m) = &self.screen else {
            return None;
        };
        match (&m.call, &m.probe) {
            (Some(call), _) => call.engine.as_ref(),
            (None, Probe::On(engine, _)) => Some(engine),
            _ => None,
        }
    }

    fn apply_sensitivity(&self) {
        if let Some(engine) = self.engine() {
            engine.set_sensitivity(self.config.sensitivity);
        }
    }

    fn apply_gain(&self) {
        if let Some(engine) = self.engine() {
            engine.set_mic_gain(self.config.mic_gain);
        }
    }

    fn close_settings(&mut self) {
        if let Screen::Main(m) = &mut self.screen {
            m.settings = None;
        }
        self.stop_probe();
    }

    /// Solta o motor de teste. Uma abertura em andamento fica sem dono e é descartada
    /// quando responder.
    fn stop_probe(&mut self) {
        if let Screen::Main(m) = &mut self.screen {
            m.stop_probe();
        }
    }

    /// Abre o motor de teste se a aba Voz está aberta e não há call nem teste.
    fn start_probe(&mut self) -> Task<Msg> {
        let Screen::Main(m) = &mut self.screen else {
            return Task::none();
        };
        let idle = matches!(m.probe, Probe::Off | Probe::Failed);
        if m.settings != Some(Tab::Voice) || m.call.is_some() || !idle {
            return Task::none();
        }
        m.next_probe += 1;
        let id = m.next_probe;
        m.probe = Probe::Opening(id);
        let prefs = self.config.audio_prefs();
        // `Engine::start` bloqueia: abre o áudio fora da thread da interface.
        Task::future(async move {
            let started = tokio::task::spawn_blocking(move || Engine::start(prefs))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            Msg::ProbeStarted(id, started.map(Boot::new))
        })
    }

    fn apply_mute(&self) {
        if let Screen::Main(m) = &self.screen
            && let Some(call) = &m.call
        {
            call.apply_mute(&self.config);
        }
    }

    fn close_volume(&mut self) {
        if let Some(call) = self.call_mut() {
            call.volume_panel = None;
            call.share_panel = false;
            call.share_menu = false;
        }
    }

    /// Clicar no canal da call volta pra ela; em outro, sai e entra no novo.
    fn join_channel(&mut self, slug: String) -> Task<Msg> {
        let Screen::Main(m) = &mut self.screen else {
            return Task::none();
        };
        let is_voice = m
            .channels
            .iter()
            .any(|c| c.slug == slug && c.kind == ChannelKind::Voice);
        let same = m.call.as_ref().is_some_and(|c| c.slug == slug);
        if !is_voice || same || m.closing.is_some() {
            return Task::none();
        }
        if let Some(old) = m.call.take() {
            old.leave();
        }
        m.notice = None;
        // Dois motores abertos ao mesmo tempo brigam pelo mic.
        m.stop_probe();
        m.next_call += 1;
        let id = m.next_call;
        m.call = Some(ActiveCall::new(id, slug));
        let prefs = self.config.audio_prefs();
        // `Engine::start` bloqueia: abre o áudio fora da thread da interface.
        Task::future(async move {
            let started = tokio::task::spawn_blocking(move || Engine::start(prefs))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            Msg::EngineStarted(id, started.map(Boot::new))
        })
    }

    fn engine_started(&mut self, id: u64, result: Result<Boot, String>) -> Task<Msg> {
        let Screen::Main(m) = &mut self.screen else {
            return Task::none();
        };
        // Saiu ou trocou de canal enquanto o áudio abria: o motor cai aqui.
        let Some(active) = m.call.as_mut().filter(|c| c.id == id) else {
            return Task::none();
        };
        let started =
            result.and_then(|boot| boot.take().ok_or_else(|| "motor indisponível".into()));
        let (engine, mic) = match started {
            Ok(started) => started,
            Err(e) => {
                m.call = None;
                m.notice = Some(format!("Não foi possível abrir o áudio: {e}"));
                return Task::none();
            }
        };
        engine.set_deafened(self.config.deafened);
        let (call, events) = Call::join(
            m.client.clone(),
            active.slug.clone(),
            self.config.install_id.clone(),
            engine.clone(),
            mic,
            self.config.mic_muted,
        );
        let muted = self.config.muted_people();
        if !muted.is_empty() {
            call.set_muted_people(muted);
        }
        active.attach(call, engine);
        let events = futures_util::stream::unfold(events, |mut rx| async {
            rx.recv().await.map(|event| (event, rx))
        });
        Task::run(events, move |event| Msg::CallEvent(id, event))
    }

    fn call_event(&mut self, id: u64, event: call::Event) -> Task<Msg> {
        let Some(active) = self.call_mut().filter(|c| c.id == id) else {
            return Task::none();
        };
        match event {
            call::Event::Connected => {
                active.status = voice::Status::Connected;
                active.cue_own_join(Instant::now());
            }
            call::Event::Reconnecting { attempt } => {
                active.status = voice::Status::Reconnecting(attempt);
            }
            call::Event::Participants(list) => active.participants(list, Instant::now()),
            call::Event::Share(state) => active.set_share(state),
            call::Event::ShareAudio(view) => active.set_share_audio(view),
            call::Event::Viewers(viewers) => active.viewers = viewers,
            call::Event::Preview(identity, picture) => active.set_preview(identity, picture),
            call::Event::Ended(end) => return self.call_ended(end),
        }
        Task::none()
    }

    fn call_ended(&mut self, end: End) -> Task<Msg> {
        let Screen::Main(m) = &mut self.screen else {
            return Task::none();
        };
        // Saída da própria pessoa (a do botão já tirou a call antes; ver `leave_call`).
        // Fechando a janela o processo acaba logo, e o som sairia cortado.
        if m.closing.is_none()
            && let Some(engine) = m.call.as_ref().and_then(|c| c.engine.as_ref())
        {
            engine.play(Effect::Leave);
        }
        m.call = None;
        let closing = m.closing.take();
        m.notice = voice::end_notice(end).map(str::to_owned);
        let (expired, probe) = match end {
            End::Unauthorized => (self.session_expired(), Task::none()),
            // Configurações abertas na aba Voz voltam a medir o mic.
            _ => (Task::none(), self.start_probe()),
        };
        Task::batch([
            expired,
            probe,
            closing.map_or_else(Task::none, window::close),
        ])
    }

    /// Sair da call deixa a tela na home na hora; o `Ended(Left)` que vem depois é
    /// ignorado porque a call já não existe.
    fn leave_call(&mut self) {
        if let Screen::Main(m) = &mut self.screen
            && let Some(active) = m.call.take()
        {
            if let Some(engine) = &active.engine {
                engine.play(Effect::Leave);
            }
            active.leave();
        }
    }

    fn close_requested(&mut self, id: window::Id) -> Task<Msg> {
        self.stop_probe();
        if let Screen::Main(m) = &mut self.screen
            && m.closing.is_none()
            && let Some(call) = m.call.as_ref().and_then(|c| c.call.as_ref())
        {
            call.leave();
            m.closing = Some(id);
            return Task::future(async move {
                tokio::time::sleep(ESPERA_AO_FECHAR).await;
                Msg::CloseNow(id)
            });
        }
        window::close(id)
    }

    fn connected(
        &mut self,
        server: String,
        cookie: Option<Secret>,
        result: Result<User, Error>,
    ) -> Task<Msg> {
        let from_form = matches!(self.screen, Screen::Server(_));
        match result {
            Ok(user) => {
                self.remember_server(&server);
                self.enter_main(server, cookie, user, true)
            }
            Err(Error::Unauthorized) => {
                self.remember_server(&server);
                let forget = match cookie {
                    Some(_) => Task::future(session::delete(server.clone())).discard(),
                    None => Task::none(),
                };
                self.screen = Screen::Login(LoginForm::new(server, None));
                Task::batch([forget, operation::focus(ID_USUARIO)])
            }
            Err(e) if from_form => {
                if let Screen::Server(f) = &mut self.screen {
                    f.busy = false;
                    f.error = Some(connect_error(e));
                }
                Task::none()
            }
            Err(_) => {
                self.screen = Screen::Offline(server);
                Task::none()
            }
        }
    }

    fn submit_server(&mut self) -> Task<Msg> {
        let Screen::Server(f) = &mut self.screen else {
            return Task::none();
        };
        if f.busy {
            return Task::none();
        }
        match api::normalize_server(&f.input) {
            Ok(server) => {
                f.busy = true;
                f.error = None;
                connect(server)
            }
            Err(e) => {
                f.error = Some(address_error(e).to_owned());
                Task::none()
            }
        }
    }

    fn change_server(&mut self) -> Task<Msg> {
        self.leave_call();
        let input = match &self.screen {
            Screen::Login(f) => f.server.clone(),
            Screen::Offline(server) => server.clone(),
            Screen::Main(m) => m.server.clone(),
            _ => return Task::none(),
        };
        self.screen = Screen::Server(ServerForm {
            input,
            error: None,
            busy: false,
        });
        operation::focus(ID_SERVIDOR)
    }

    fn submit_login(&mut self) -> Task<Msg> {
        let Screen::Login(f) = &mut self.screen else {
            return Task::none();
        };
        if f.busy || f.username.is_empty() || f.password.0.is_empty() {
            return Task::none();
        }
        f.busy = true;
        f.error = None;
        let (server, username, password) =
            (f.server.clone(), f.username.clone(), f.password.clone());
        Task::future(async move {
            let result = async {
                let cookie = Client::new(&server, None)
                    .login(&username, &password)
                    .await?;
                let user = Client::new(&server, Some(cookie.clone())).me().await?;
                Ok((cookie, user))
            };
            Msg::LoginDone(result.await)
        })
    }

    fn login_done(&mut self, result: Result<(Secret, User), Error>) -> Task<Msg> {
        let Screen::Login(f) = &mut self.screen else {
            return Task::none();
        };
        match result {
            Ok((cookie, user)) => {
                let server = f.server.clone();
                let save = Task::future(session::save(server.clone(), cookie.clone()))
                    .map(Msg::VaultSaved);
                Task::batch([self.enter_main(server, Some(cookie), user, false), save])
            }
            Err(e) => {
                f.busy = false;
                f.error = Some(login_error(e));
                Task::none()
            }
        }
    }

    fn enter_main(
        &mut self,
        server: String,
        cookie: Option<Secret>,
        user: User,
        vault_ok: bool,
    ) -> Task<Msg> {
        let client = Client::new(&server, cookie);
        let (a, b, c) = (client.clone(), client.clone(), client.clone());
        let mut avatar_paths = HashMap::new();
        if let Some(path) = &user.avatar_url {
            avatar_paths.insert(user.username.clone(), path.clone());
        }
        let mut main = Main {
            server,
            client,
            user,
            channels: Vec::new(),
            presence: Presence::new(),
            polling: true,
            failures: 0,
            avatar_paths,
            avatars: HashMap::new(),
            requested: HashSet::new(),
            settings: None,
            resizing: false,
            vault_ok,
            call: None,
            full_window: false,
            next_call: 0,
            notice: None,
            devices: None,
            probe: Probe::Off,
            next_probe: 0,
            level: MANUAL_MIN_DB,
            open: false,
            closing: None,
            update: None,
            check: None,
        };
        let avatars = main.fetch_avatars();
        self.screen = Screen::Main(Box::new(main));
        Task::batch([
            avatars,
            Task::future(async {
                tokio::time::sleep(DE_PE).await;
                Msg::Healthy
            }),
            Task::future(async move { Msg::Channels(a.channels().await) }),
            Task::future(async move { Msg::Members(b.members().await) }),
            Task::future(async move { Msg::Presence(c.presence().await) }),
        ])
    }

    fn presence(&mut self, result: Result<Presence, Error>) -> Task<Msg> {
        let Screen::Main(m) = &mut self.screen else {
            return Task::none();
        };
        m.polling = false;
        match result {
            Ok(presence) => {
                m.presence = presence;
                m.failures = 0;
                m.fetch_avatars()
            }
            Err(Error::Unauthorized) => self.session_expired(),
            // Fica com a última lista: uma falha de rede não derruba a tela.
            Err(_) => {
                m.failures = m.failures.saturating_add(1);
                Task::none()
            }
        }
    }

    fn main_failed(&mut self, e: Error) -> Task<Msg> {
        match (e, &self.screen) {
            (Error::Unauthorized, _) => self.session_expired(),
            (_, Screen::Main(m)) => {
                let server = m.server.clone();
                self.leave_call();
                self.screen = Screen::Offline(server);
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn session_expired(&mut self) -> Task<Msg> {
        let Screen::Main(m) = &self.screen else {
            return Task::none();
        };
        let server = m.server.clone();
        self.leave_call();
        let error = Some("Sua sessão expirou. Entre de novo.".to_owned());
        self.screen = Screen::Login(LoginForm::new(server.clone(), error));
        Task::batch([
            Task::future(session::delete(server)).discard(),
            operation::focus(ID_USUARIO),
        ])
    }

    fn logout(&mut self) -> Task<Msg> {
        let Screen::Main(m) = &self.screen else {
            return Task::none();
        };
        let (server, client) = (m.server.clone(), m.client.clone());
        self.leave_call();
        self.screen = Screen::Login(LoginForm::new(server.clone(), None));
        // Apaga do cofre antes: com o servidor fora, o logout demora até o tempo esgotar.
        let leave = Task::future(async move {
            session::delete(server).await;
            let _ = client.logout().await;
        });
        Task::batch([leave.discard(), operation::focus(ID_USUARIO)])
    }

    fn remember_server(&mut self, server: &str) {
        if self.config.server.as_deref() != Some(server) {
            self.config.server = Some(server.to_owned());
            self.persist();
        }
    }

    fn persist(&self) {
        if cfg!(test) {
            #[cfg(test)]
            SAVES.with(|n| n.set(n.get() + 1));
            return;
        }
        if let Err(e) = self.config.save() {
            eprintln!("config não salva: {e}");
        }
    }
}

impl LoginForm {
    fn new(server: String, error: Option<String>) -> Self {
        Self {
            server,
            username: String::new(),
            password: Secret(String::new()),
            error,
            busy: false,
        }
    }
}

impl Main {
    fn stop_probe(&mut self) {
        self.probe = Probe::Off;
        self.level = MANUAL_MIN_DB;
        self.open = false;
    }

    /// Conferência de atualização no meio: o botão do Sobre espera ela acabar.
    fn checking(&self) -> bool {
        matches!(self.check, Some(Check::Searching | Check::Downloading(_)))
    }

    /// Só a aba Voz aberta com medidor pede o relógio; fora disso a janela fica parada.
    fn meter_clock(&self) -> bool {
        self.settings == Some(Tab::Voice) && self.meter().is_some()
    }

    /// Medidor da call ou, sem call, do motor de teste.
    fn meter(&self) -> Option<&Meter> {
        match (&self.call, &self.probe) {
            (Some(call), _) => call.meter.as_ref(),
            (None, Probe::On(_, meter)) => Some(meter),
            _ => None,
        }
    }

    /// Só baixa quem aparece na tela: quem está em canal, e a própria pessoa.
    fn fetch_avatars(&mut self) -> Task<Msg> {
        let mut paths: Vec<String> = self
            .presence
            .values()
            .flatten()
            .map(|p| p.display_name())
            .chain([self.user.username.as_str()])
            .filter_map(|name| self.avatar_paths.get(name))
            .filter(|path| !self.requested.contains(*path))
            .cloned()
            .collect();
        paths.sort();
        paths.dedup();
        Task::batch(paths.into_iter().map(|path| {
            self.requested.insert(path.clone());
            let client = self.client.clone();
            Task::future(async move {
                let handle = match client.avatar(&path).await {
                    Ok(bytes) => avatar::decode_off_thread(bytes).await,
                    Err(_) => None,
                };
                Msg::Avatar(path, handle)
            })
        }))
    }

    fn avatar(&self, name: &str) -> Option<&image::Handle> {
        Some(&self.avatars.get(self.avatar_paths.get(name)?)?.0)
    }

    /// Cor dominante da foto, pro fundo do tile.
    fn tint(&self, name: &str) -> Option<Color> {
        Some(self.avatars.get(self.avatar_paths.get(name)?)?.1)
    }
}

fn keys(event: keyboard::Event) -> Option<Msg> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
        return None;
    };
    match key {
        Key::Named(Named::Tab) if modifiers.shift() => Some(Msg::FocusPrevious),
        Key::Named(Named::Tab) => Some(Msg::FocusNext),
        Key::Named(Named::Escape) => Some(Msg::Escape),
        _ => None,
    }
}

fn drag(event: Event, _: event::Status, _: window::Id) -> Option<Msg> {
    match event {
        Event::Mouse(mouse::Event::CursorMoved { position }) => Some(Msg::DragMove(position.x)),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => Some(Msg::DragEnd),
        _ => None,
    }
}

fn focus(event: Event, _: event::Status, _: window::Id) -> Option<Msg> {
    match event {
        Event::Window(window::Event::Focused) => Some(Msg::Focus(true)),
        Event::Window(window::Event::Unfocused) => Some(Msg::Focus(false)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ChannelKind, Participant};
    use crate::share::{Choice, State as ShareState};

    fn app(screen: Screen) -> App {
        App {
            config: Config::default(),
            screen,
        }
    }

    fn form() -> Screen {
        Screen::Server(ServerForm {
            input: "a.b".into(),
            error: None,
            busy: true,
        })
    }

    fn login() -> Screen {
        Screen::Login(LoginForm::new("https://a.b".into(), None))
    }

    fn user() -> User {
        User {
            id: 1,
            username: "Opedro".into(),
            is_admin: true,
            avatar_url: None,
        }
    }

    fn main_app() -> App {
        let mut a = app(login());
        let _ = a.enter_main("https://a.b".into(), None, user(), true);
        a
    }

    fn main_of(a: &mut App) -> &mut Main {
        match &mut a.screen {
            Screen::Main(m) => m,
            _ => panic!("fora da tela principal"),
        }
    }

    #[test]
    fn servidor_sem_sessao_vai_pro_login_e_guarda_endereco() {
        let mut a = app(form());
        let _ = a.update(Msg::Connected {
            server: "https://a.b".into(),
            cookie: None,
            result: Err(Error::Unauthorized),
        });
        assert!(matches!(a.screen, Screen::Login(_)));
        assert_eq!(a.config.server.as_deref(), Some("https://a.b"));
    }

    #[test]
    fn sessao_valida_entra_direto() {
        let mut a = app(Screen::Connecting);
        let _ = a.update(Msg::Connected {
            server: "https://a.b".into(),
            cookie: Some(Secret("session=x".into())),
            result: Ok(user()),
        });
        assert!(matches!(a.screen, Screen::Main(_)));
    }

    #[test]
    fn erro_no_formulario_fica_no_formulario() {
        let mut a = app(form());
        let _ = a.update(Msg::Connected {
            server: "https://a.b".into(),
            cookie: None,
            result: Err(Error::NotConcord),
        });
        let Screen::Server(f) = &a.screen else {
            panic!()
        };
        assert!(!f.busy);
        assert_eq!(
            f.error.as_deref(),
            Some("Esse endereço não é um servidor Concord.")
        );
        assert_eq!(a.config.server, None);
    }

    #[test]
    fn servidor_fora_na_abertura_mostra_offline() {
        let mut a = app(Screen::Connecting);
        let _ = a.update(Msg::Connected {
            server: "https://a.b".into(),
            cookie: None,
            result: Err(Error::Offline),
        });
        assert!(matches!(&a.screen, Screen::Offline(s) if s == "https://a.b"));
        let _ = a.update(Msg::Retry);
        assert!(matches!(a.screen, Screen::Connecting));
    }

    #[test]
    fn erros_de_login_na_lingua_da_web() {
        assert_eq!(
            login_error(Error::BadCredentials),
            "Usuário ou senha inválidos."
        );
        assert_eq!(
            login_error(Error::TooManyAttempts(30)),
            "Muitas tentativas. Tente de novo em 30 s."
        );
        assert_eq!(
            login_error(Error::TooManyAttempts(61)),
            "Muitas tentativas. Tente de novo em 2 min."
        );
    }

    #[test]
    fn login_vazio_nao_envia() {
        let mut a = app(login());
        let _ = a.update(Msg::LoginSubmit);
        let Screen::Login(f) = &a.screen else {
            panic!()
        };
        assert!(!f.busy);
    }

    #[test]
    fn sessao_vencida_volta_pro_login_com_aviso() {
        let mut a = main_app();
        let _ = a.update(Msg::Presence(Err(Error::Unauthorized)));
        let Screen::Login(f) = &a.screen else {
            panic!()
        };
        assert_eq!(
            f.error.as_deref(),
            Some("Sua sessão expirou. Entre de novo.")
        );
    }

    #[test]
    fn falha_de_presenca_mantem_a_lista_e_avisa_depois_de_tres() {
        let mut a = main_app();
        let lista = Presence::from([("call".to_owned(), vec![])]);
        let _ = a.update(Msg::Presence(Ok(lista)));
        for _ in 0..FALHAS_ATE_AVISAR {
            let _ = a.update(Msg::PollIf(None));
            let _ = a.update(Msg::Presence(Err(Error::Offline)));
        }
        let m = main_of(&mut a);
        assert!(m.presence.contains_key("call"));
        assert_eq!(m.failures, FALHAS_ATE_AVISAR);
        let _ = a.update(Msg::Presence(Ok(Presence::new())));
        assert_eq!(main_of(&mut a).failures, 0);
    }

    #[test]
    fn presenca_para_com_janela_minimizada_e_nao_sobrepoe() {
        let mut a = main_app();
        let _ = a.update(Msg::Presence(Ok(Presence::new())));
        let _ = a.update(Msg::PollIf(Some(true)));
        assert!(!main_of(&mut a).polling);
        let _ = a.update(Msg::PollIf(Some(false)));
        assert!(main_of(&mut a).polling);
        let _ = a.update(Msg::PollIf(None));
        assert!(main_of(&mut a).polling);
    }

    #[test]
    fn arrastar_respeita_os_limites() {
        let mut a = main_app();
        let _ = a.update(Msg::DragMove(50.0));
        assert_eq!(a.config.sidebar_width, SIDEBAR_MIN);
        let _ = a.update(Msg::DragMove(5000.0));
        assert_eq!(a.config.sidebar_width, SIDEBAR_MAX);
    }

    #[test]
    fn avatar_baixa_uma_vez_e_so_de_quem_aparece() {
        let mut a = main_app();
        let m = main_of(&mut a);
        m.avatar_paths
            .insert("Opedro".into(), "/api/avatars/1?v=a".into());
        m.avatar_paths
            .insert("ninguem".into(), "/api/avatars/2?v=b".into());
        let _ = m.fetch_avatars();
        assert_eq!(
            m.requested,
            HashSet::from(["/api/avatars/1?v=a".to_owned()])
        );
        let _ = m.fetch_avatars();
        assert_eq!(m.requested.len(), 1);
    }

    #[test]
    fn senha_nao_aparece_no_debug_da_mensagem() {
        let msg = Msg::Password(Secret("hunter2".into()));
        assert!(!format!("{msg:?}").contains("hunter2"));
    }

    fn canal(id: i64, name: &str, kind: ChannelKind) -> Channel {
        Channel {
            id,
            name: name.into(),
            slug: name.to_lowercase(),
            kind,
        }
    }

    /// Principal com dois canais de voz e um de texto, sem call.
    fn voice_app() -> App {
        let mut a = main_app();
        main_of(&mut a).channels = vec![
            canal(1, "Geral", ChannelKind::Text),
            canal(2, "Call", ChannelKind::Voice),
            canal(3, "Jogando", ChannelKind::Voice),
        ];
        a
    }

    fn call_of(a: &mut App) -> &mut ActiveCall {
        main_of(a).call.as_mut().expect("sem call")
    }

    fn call_id(a: &mut App) -> u64 {
        call_of(a).id
    }

    /// Entra em "call" e simula o que o `Call::join` mandaria, sem tocar no contrato.
    fn in_call(names: &[&str]) -> App {
        let mut a = voice_app();
        let _ = a.update(Msg::JoinChannel("call".into()));
        let id = call_id(&mut a);
        let _ = a.update(Msg::CallEvent(id, call::Event::Connected));
        let list = names.iter().map(|n| voice::test_live(n, false)).collect();
        let _ = a.update(Msg::CallEvent(id, call::Event::Participants(list)));
        a
    }

    #[test]
    fn clicar_no_canal_de_voz_entra_sem_tela_previa() {
        let mut a = voice_app();
        a.config.mic_muted = true;
        let _ = a.update(Msg::JoinChannel("call".into()));
        let c = call_of(&mut a);
        assert_eq!(
            (c.slug.as_str(), c.status),
            ("call", voice::Status::Connecting)
        );
        assert!(a.config.mic_muted, "o mic segue o estado salvo");
    }

    #[test]
    fn canal_de_texto_ou_desconhecido_nao_entra() {
        let mut a = voice_app();
        let _ = a.update(Msg::JoinChannel("geral".into()));
        let _ = a.update(Msg::JoinChannel("nao-existe".into()));
        assert!(main_of(&mut a).call.is_none());
    }

    #[test]
    fn clicar_no_canal_atual_volta_pra_call_sem_reconectar() {
        let mut a = in_call(&["Ana"]);
        let id = call_id(&mut a);
        let _ = a.update(Msg::JoinChannel("call".into()));
        assert_eq!(call_id(&mut a), id);
        assert!(call_of(&mut a).live.is_some(), "a sala não foi zerada");
    }

    #[test]
    fn trocar_de_canal_sai_e_entra_no_novo_e_ignora_eventos_do_antigo() {
        let mut a = in_call(&["Ana"]);
        let antigo = call_id(&mut a);
        let _ = a.update(Msg::JoinChannel("jogando".into()));
        let novo = call_id(&mut a);
        assert_ne!(antigo, novo);
        assert_eq!(call_of(&mut a).slug, "jogando");
        assert!(call_of(&mut a).live.is_none());
        let _ = a.update(Msg::CallEvent(antigo, call::Event::Ended(End::Left)));
        let _ = a.update(Msg::EngineStarted(antigo, Err("velho".into())));
        assert_eq!(
            call_id(&mut a),
            novo,
            "o fim da call antiga não derruba a nova"
        );
    }

    #[test]
    fn sair_volta_pra_home_e_o_ended_atrasado_nao_faz_nada() {
        let mut a = in_call(&["Ana"]);
        let id = call_id(&mut a);
        let _ = a.update(Msg::Leave);
        assert!(main_of(&mut a).call.is_none());
        let _ = a.update(Msg::CallEvent(id, call::Event::Ended(End::Left)));
        assert!(main_of(&mut a).notice.is_none());
    }

    #[test]
    fn mudo_e_surdo_valem_com_e_sem_call() {
        for com_call in [false, true] {
            let mut a = if com_call {
                in_call(&["Ana"])
            } else {
                voice_app()
            };
            let _ = a.update(Msg::ToggleDeafen);
            assert!(a.config.deafened && a.config.mic_muted);
            let _ = a.update(Msg::ToggleMic);
            assert!(
                !a.config.deafened && !a.config.mic_muted,
                "desmutar tira o surdo"
            );
            let _ = a.update(Msg::ToggleMic);
            assert!(a.config.mic_muted);
        }
    }

    #[test]
    fn conectando_reconectando_e_conectado() {
        let mut a = in_call(&[]);
        let id = call_id(&mut a);
        assert_eq!(call_of(&mut a).status, voice::Status::Connected);
        let _ = a.update(Msg::CallEvent(id, call::Event::Reconnecting { attempt: 3 }));
        assert_eq!(call_of(&mut a).status, voice::Status::Reconnecting(3));
        let _ = a.update(Msg::CallEvent(id, call::Event::Connected));
        assert_eq!(call_of(&mut a).status, voice::Status::Connected);
    }

    #[test]
    fn ended_volta_pra_home_com_aviso_conforme_o_motivo() {
        for (end, aviso) in [
            (End::Left, None),
            (
                End::Duplicate,
                Some("Você entrou nessa call de outro lugar."),
            ),
            (End::Removed, Some("Você foi removido da call.")),
            (End::RoomClosed, Some("A sala foi fechada.")),
            (End::GaveUp, Some("Não foi possível reconectar à call.")),
        ] {
            let mut a = in_call(&["Ana"]);
            let id = call_id(&mut a);
            let _ = a.update(Msg::CallEvent(id, call::Event::Ended(end)));
            let m = main_of(&mut a);
            assert!(m.call.is_none(), "{end:?}");
            assert_eq!(m.notice.as_deref(), aviso, "{end:?}");
        }
    }

    #[test]
    fn sessao_vencida_na_call_usa_o_caminho_de_sessao_expirada() {
        let mut a = in_call(&["Ana"]);
        let id = call_id(&mut a);
        let _ = a.update(Msg::CallEvent(id, call::Event::Ended(End::Unauthorized)));
        let Screen::Login(f) = &a.screen else {
            panic!("não voltou pro login")
        };
        assert_eq!(
            f.error.as_deref(),
            Some("Sua sessão expirou. Entre de novo.")
        );
    }

    #[test]
    fn ended_com_a_janela_fechando_limpa_o_pedido() {
        let mut a = in_call(&["Ana"]);
        let id = call_id(&mut a);
        main_of(&mut a).closing = Some(window::Id::unique());
        let _ = a.update(Msg::CallEvent(id, call::Event::Ended(End::Left)));
        assert!(main_of(&mut a).closing.is_none());
    }

    #[test]
    fn falha_do_motor_mostra_o_erro_e_volta_pra_home() {
        let mut a = voice_app();
        let _ = a.update(Msg::JoinChannel("call".into()));
        let id = call_id(&mut a);
        let _ = a.update(Msg::EngineStarted(id, Err("sem placa de som".into())));
        let m = main_of(&mut a);
        assert!(m.call.is_none());
        assert_eq!(
            m.notice.as_deref(),
            Some("Não foi possível abrir o áudio: sem placa de som")
        );
        let _ = a.update(Msg::JoinChannel("call".into()));
        assert!(main_of(&mut a).notice.is_none(), "entrar limpa o aviso");
    }

    #[test]
    fn sidebar_da_call_usa_a_sala_sem_nome_repetido_e_o_anel_segura_400_ms() {
        let mut a = voice_app();
        let _ = a.update(Msg::JoinChannel("call".into()));
        let id = call_id(&mut a);
        let antes = Instant::now();
        let falando = voice::test_live("Zé", true);
        let mut fantasma = voice::test_live("ana", false);
        fantasma.identity = "ana__2".into();
        let lista = vec![falando, voice::test_live("ana", false), fantasma];
        let _ = a.update(Msg::CallEvent(id, call::Event::Participants(lista)));
        let nomes: Vec<_> = call_of(&mut a)
            .live
            .iter()
            .flatten()
            .map(|l| l.name.clone())
            .collect();
        assert_eq!(nomes, ["ana", "Zé"]);
        assert!(call_of(&mut a).speaking("Zé"));

        let _ = a.update(Msg::Frame(antes + Duration::from_secs(5)));
        assert!(call_of(&mut a).speaking("Zé"), "falando não apaga sozinho");

        let lista = vec![
            voice::test_live("Zé", false),
            voice::test_live("ana", false),
        ];
        let _ = a.update(Msg::CallEvent(id, call::Event::Participants(lista)));
        let parou = Instant::now();
        let _ = a.update(Msg::Frame(parou + Duration::from_millis(300)));
        assert!(call_of(&mut a).speaking("Zé"));
        let _ = a.update(Msg::Frame(parou + Duration::from_secs(1)));
        assert!(!call_of(&mut a).speaking("Zé"));
    }

    #[test]
    fn espectadores_viram_nomes_na_ordem_e_sem_quem_saiu() {
        let mut a = in_call(&["Ana", "Bia", "Cai"]);
        let id = call_id(&mut a);
        assert!(call_of(&mut a).viewers_of("Ana__1").is_empty());
        let quem = vec![
            "Cai__1".to_owned(),
            "Zeca__1".to_owned(),
            "Bia__1".to_owned(),
        ];
        let _ = a.update(Msg::CallEvent(
            id,
            call::Event::Viewers(HashMap::from([("Ana__1".to_owned(), quem)])),
        ));
        assert_eq!(call_of(&mut a).viewers_of("Ana__1"), ["Cai", "Bia"]);
        assert!(call_of(&mut a).viewers_of("Bia__1").is_empty());
        let _ = a.update(Msg::CallEvent(id, call::Event::Viewers(HashMap::new())));
        assert!(call_of(&mut a).viewers_of("Ana__1").is_empty());
    }

    #[test]
    fn assistir_tela_cheia_e_volta_quando_a_pessoa_para() {
        let mut a = in_call(&["Ana"]);
        let id = call_id(&mut a);
        let transmitindo = |on| {
            let mut l = voice::test_live("Ana", false);
            l.screen_share = on;
            vec![l]
        };
        let _ = a.update(Msg::CallEvent(
            id,
            call::Event::Participants(transmitindo(true)),
        ));
        let _ = a.update(Msg::Watch(Some("Ana__1".into())));
        assert_eq!(call_of(&mut a).watching.as_deref(), Some("Ana__1"));
        assert_eq!(call_of(&mut a).view, voice::View::Tile, "começa no tile");

        let _ = a.update(Msg::FocusScreen);
        assert_eq!(call_of(&mut a).view, voice::View::Focus, "clique aumenta");
        let _ = a.update(Msg::FocusScreen);
        assert_eq!(
            call_of(&mut a).view,
            voice::View::Tile,
            "outro clique volta"
        );

        let _ = a.update(Msg::Fullscreen);
        assert!(main_of(&mut a).full_window, "a janela segue a tela cheia");
        let _ = a.update(Msg::Escape);
        assert_eq!(
            call_of(&mut a).view,
            voice::View::Focus,
            "Esc volta pro grande"
        );
        assert_eq!(
            call_of(&mut a).watching.as_deref(),
            Some("Ana__1"),
            "e segue assistindo"
        );
        assert!(!main_of(&mut a).full_window);

        let _ = a.update(Msg::Fullscreen);
        let _ = a.update(Msg::CallEvent(
            id,
            call::Event::Participants(transmitindo(false)),
        ));
        assert_eq!(call_of(&mut a).watching, None, "parou de transmitir");
        assert!(
            !main_of(&mut a).full_window,
            "a janela não fica presa em tela cheia"
        );
    }

    #[test]
    fn arrastar_o_volume_grava_por_nome_e_so_lista_quem_esta_em_zero() {
        let mut a = in_call(&["Ana", "Bia"]);
        let _ = a.update(Msg::OpenVolume("Ana".into()));
        assert_eq!(call_of(&mut a).volume_panel.as_deref(), Some("Ana"));
        assert_eq!(a.config.volume_of("Ana"), 71);

        for v in [60, 30, 0] {
            let _ = a.update(Msg::Volume("Ana".into(), v));
        }
        assert_eq!(a.config.volume_of("Ana"), 0);
        assert_eq!(a.config.muted_people(), ["Ana"]);
        let _ = a.update(Msg::Volume("Bia".into(), 0));
        assert_eq!(a.config.muted_people(), ["Ana", "Bia"]);
        let _ = a.update(Msg::Volume("Ana".into(), 12));
        assert_eq!(a.config.muted_people(), ["Bia"]);
        let _ = a.update(Msg::VolumeDone);

        let _ = a.update(Msg::Escape);
        assert!(call_of(&mut a).volume_panel.is_none(), "a call segue");
    }

    #[test]
    fn config_de_voz_vale_com_e_sem_call() {
        let mut a = in_call(&["Ana"]);
        let _ = a.update(Msg::Input(Some("USB".into())));
        let _ = a.update(Msg::Output(None));
        let _ = a.update(Msg::Noise(NoiseLevel::Off));
        let _ = a.update(Msg::Master(40));
        let p = a.config.audio_prefs();
        assert_eq!(p.input.as_deref(), Some("USB"));
        assert_eq!((p.output, p.noise, p.master), (None, NoiseLevel::Off, 40));
    }

    #[test]
    fn trocar_de_servidor_e_encerrar_sessao_saem_da_call() {
        for msg in [Msg::ChangeServer, Msg::Logout] {
            let mut a = in_call(&["Ana"]);
            let _ = a.update(msg);
            assert!(!matches!(a.screen, Screen::Main(_)));
        }
    }

    fn share(a: &mut App, state: ShareState) {
        let id = call_id(a);
        let _ = a.update(Msg::CallEvent(id, call::Event::Share(state)));
    }

    #[test]
    fn painel_de_tela_abre_escolhe_e_grava() {
        let mut a = in_call(&["Ana"]);
        assert!(!call_of(&mut a).share_panel);
        assert_eq!(a.config.share_quality, Quality::Alta);
        let _ = a.update(Msg::ShareButton);
        assert!(call_of(&mut a).share_panel);

        let antes = saves();
        let _ = a.update(Msg::ShareQuality(Quality::Economica));
        assert_eq!(a.config.share_quality, Quality::Economica);
        assert_eq!(saves(), antes + 1);
        assert!(call_of(&mut a).share_panel, "trocar não fecha");

        let _ = a.update(Msg::ShareStart);
        assert!(!call_of(&mut a).share_panel);
    }

    fn audio(a: &mut App, choice: Choice, apps: &[&str], ask: bool) {
        let id = call_id(a);
        let apps = apps.iter().map(|s| s.to_string()).collect();
        let view = crate::share::View { choice, apps, ask };
        let _ = a.update(Msg::CallEvent(id, call::Event::ShareAudio(view)));
    }

    #[test]
    fn menu_da_seta_liga_e_desliga_o_audio_e_some_com_a_transmissao() {
        let mut a = in_call(&["Ana"]);
        let antes = saves();
        let _ = a.update(Msg::ShareAudio(false));
        assert!(!a.config.share_audio);
        assert_eq!(saves(), antes + 1);

        share(&mut a, ShareState::Live);
        audio(&mut a, Choice::All, &["Jogo"], false);
        assert!(call_of(&mut a).share_sound, "tela inteira leva o som");
        audio(&mut a, Choice::Silent, &["A", "B"], true);
        assert!(
            !call_of(&mut a).share_sound,
            "janela sem app óbvio começa muda"
        );
        assert!(!call_of(&mut a).menu_open(), "nada abre sozinho");

        let _ = a.update(Msg::ShareMenu);
        assert!(call_of(&mut a).share_menu);
        assert!(call_of(&mut a).menu_open());
        let _ = a.update(Msg::ShareSound);
        assert!(call_of(&mut a).share_sound, "ligar");
        assert!(!call_of(&mut a).share_menu, "escolher fecha o menu");
        let _ = a.update(Msg::ShareSound);
        assert!(!call_of(&mut a).share_sound, "desligar");

        let _ = a.update(Msg::ShareMenu);
        let _ = a.update(Msg::Escape);
        assert!(!call_of(&mut a).share_menu);

        let _ = a.update(Msg::ShareMenu);
        share(&mut a, ShareState::Live);
        assert!(call_of(&mut a).share_audio.is_some(), "sala nova mantém");
        share(&mut a, ShareState::Off);
        assert!(call_of(&mut a).share_audio.is_none());
        assert!(!call_of(&mut a).share_menu);
    }

    #[test]
    fn clicar_fora_fecha_qualquer_painel() {
        let mut a = in_call(&["Ana"]);
        let _ = a.update(Msg::OpenVolume("Ana".into()));
        assert!(call_of(&mut a).menu_open());
        let _ = a.update(Msg::CloseVolume);
        assert!(!call_of(&mut a).menu_open());
        share(&mut a, ShareState::Live);
        let _ = a.update(Msg::ShareMenu);
        let _ = a.update(Msg::CloseVolume);
        assert!(!call_of(&mut a).menu_open());
    }

    #[test]
    fn escape_e_painel_de_volume_fecham_o_painel_de_tela() {
        let mut a = in_call(&["Ana"]);
        let _ = a.update(Msg::ShareButton);
        let _ = a.update(Msg::Escape);
        assert!(!call_of(&mut a).share_panel);

        let _ = a.update(Msg::ShareButton);
        let _ = a.update(Msg::OpenVolume("Ana".into()));
        assert!(!call_of(&mut a).share_panel);
        let _ = a.update(Msg::ShareButton);
        assert!(call_of(&mut a).volume_panel.is_none(), "um painel por vez");
    }

    #[test]
    fn estados_da_transmissao_vem_da_call() {
        let mut a = in_call(&["Ana"]);
        for state in [ShareState::Choosing, ShareState::Live, ShareState::Off] {
            share(&mut a, state);
            assert_eq!(call_of(&mut a).share, state);
        }
    }

    #[test]
    fn com_transmissao_em_andamento_o_botao_nao_abre_o_painel() {
        for state in [ShareState::Choosing, ShareState::Live] {
            let mut a = in_call(&["Ana"]);
            share(&mut a, state);
            let _ = a.update(Msg::ShareButton);
            assert!(!call_of(&mut a).share_panel);
        }
    }

    /// Prévia já "na GPU", pros testes e prints: tela azul com uma janela clara.
    fn alocada(w: u32, h: u32) -> image::Allocation {
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let janela = (w / 6..w * 5 / 6).contains(&x) && (h / 6..h * 5 / 6).contains(&y);
                let px = match (janela, y < h / 6 + h / 12) {
                    (true, true) => [70, 76, 92, 255],
                    (true, false) => [236, 238, 242, 255],
                    _ => [(40 + x * 60 / w) as u8, (60 + y * 80 / h) as u8, 150, 255],
                };
                rgba.extend_from_slice(&px);
            }
        }
        let handle = image::Handle::from_rgba(w, h, rgba);
        // SAFETY: renderer nenhum guarda memória por esta `Allocation`; ela só leva o handle,
        // que o tiny-skia carrega na hora de desenhar.
        unsafe { iced_test::core::image::allocate(&handle, iced::Size::new(w, h)) }
    }

    #[test]
    fn previa_propria_para_sem_foco_e_some_com_a_transmissao() {
        let mut a = in_call(&["Ana"]);
        assert!(
            !call_of(&mut a).preview_clock(),
            "sem transmissão, sem relógio"
        );
        share(&mut a, ShareState::Live);
        assert!(call_of(&mut a).preview_clock());
        let _ = a.update(Msg::Focus(false));
        assert!(call_of(&mut a).preview_paused);
        assert!(!call_of(&mut a).preview_clock(), "sem foco nada sobe");
        let _ = a.update(Msg::Focus(true));
        assert!(call_of(&mut a).preview_clock());

        let _ = a.update(Msg::PreviewReady(Some(alocada(48, 27))));
        assert!(call_of(&mut a).own.is_some());
        share(&mut a, ShareState::Off);
        assert!(call_of(&mut a).own.is_none());
        let _ = a.update(Msg::PreviewReady(Some(alocada(48, 27))));
        assert!(call_of(&mut a).own.is_none(), "atrasada, depois de parar");
    }

    #[test]
    fn camera_aparece_ligada_e_volta_pra_foto_muda() {
        let mut a = in_call(&["Ana"]);
        let id = call_id(&mut a);
        let camera = |on| {
            let mut l = voice::test_live("Ana", false);
            l.camera = on;
            vec![l]
        };
        assert!(!call_of(&mut a).camera_clock(), "sem câmera, sem relógio");
        let _ = a.update(Msg::CameraReady("Ana__1".into(), Some(alocada(64, 36))));
        assert!(
            call_of(&mut a).cameras.is_empty(),
            "atrasado, câmera desligada"
        );

        let _ = a.update(Msg::CallEvent(id, call::Event::Participants(camera(true))));
        assert!(call_of(&mut a).camera_clock());
        let _ = a.update(Msg::CameraReady("Ana__1".into(), Some(alocada(64, 36))));
        assert!(call_of(&mut a).cameras.contains_key("Ana__1"));

        let _ = a.update(Msg::CallEvent(id, call::Event::Participants(camera(false))));
        assert!(call_of(&mut a).cameras.is_empty(), "muda: volta pra foto");
        assert!(!call_of(&mut a).camera_clock());
    }

    #[test]
    fn trocar_no_ar_nao_para_a_transmissao() {
        let mut a = in_call(&["Ana"]);
        share(&mut a, ShareState::Live);
        let _ = a.update(Msg::ShareSwap);
        // No Linux o portal abre direto; no Windows abre o nosso seletor.
        assert_eq!(call_of(&mut a).share_panel, cfg!(windows));
        call_of(&mut a).share_panel = true;
        let _ = a.update(Msg::ShareButton);
        assert!(
            !call_of(&mut a).share_panel,
            "o X do seletor fecha ele, sem parar"
        );
        assert_eq!(call_of(&mut a).share, ShareState::Live);
    }

    #[test]
    fn falha_fica_ate_a_pessoa_tentar_de_novo() {
        let mut a = in_call(&["Ana"]);
        share(&mut a, ShareState::Failed);
        assert_eq!(call_of(&mut a).share, ShareState::Failed);
        let _ = a.update(Msg::ShareButton);
        assert_eq!(call_of(&mut a).share, ShareState::Off);
        assert!(call_of(&mut a).share_panel);
    }

    #[test]
    fn sair_ou_trocar_de_canal_zera_a_transmissao() {
        for msg in [Msg::Leave, Msg::JoinChannel("jogando".into())] {
            let mut a = in_call(&["Ana"]);
            let antiga = call_id(&mut a);
            share(&mut a, ShareState::Live);
            let _ = a.update(msg);
            // A call antiga ainda avisa que parou; isso não pode cair na nova.
            let _ = a.update(Msg::CallEvent(
                antiga,
                call::Event::Share(ShareState::Failed),
            ));
            if let Some(nova) = main_of(&mut a).call.as_ref() {
                assert_eq!(nova.share, ShareState::Off);
            }
        }
    }

    #[test]
    fn fim_da_call_leva_a_transmissao_junto() {
        for end in [End::RoomClosed, End::Unauthorized] {
            let mut a = in_call(&["Ana"]);
            let id = call_id(&mut a);
            share(&mut a, ShareState::Live);
            let _ = a.update(Msg::CallEvent(id, call::Event::Ended(end)));
            if let Screen::Main(m) = &a.screen {
                assert!(m.call.is_none());
            }
        }
    }

    fn probe_of(a: &mut App) -> &Probe {
        &main_of(a).probe
    }

    fn voice_tab() -> App {
        let mut a = voice_app();
        let _ = a.update(Msg::OpenSettings(Tab::Voice));
        a
    }

    fn saves() -> u32 {
        SAVES.with(|n| n.get())
    }

    #[test]
    fn automatico_e_manual_guardam_o_valor() {
        use crate::audio::{MANUAL_DEFAULT_DB, Sensitivity};
        let mut a = voice_tab();
        assert_eq!(a.config.sensitivity, Sensitivity::Auto);
        let _ = a.update(Msg::AutoSensitivity(false));
        assert_eq!(a.config.sensitivity, Sensitivity::Manual(MANUAL_DEFAULT_DB));
        let _ = a.update(Msg::Threshold(-55.0));
        let _ = a.update(Msg::AutoSensitivity(true));
        assert_eq!(a.config.sensitivity, Sensitivity::Auto);
        assert_eq!(a.config.audio_prefs().sensitivity, Sensitivity::Auto);
        let _ = a.update(Msg::Threshold(-10.0));
        let _ = a.update(Msg::AutoSensitivity(false));
        assert_eq!(a.config.sensitivity, Sensitivity::Manual(-55.0));
        assert_eq!(
            a.config.audio_prefs().sensitivity,
            Sensitivity::Manual(-55.0)
        );
    }

    #[test]
    fn ganho_automatico_e_manual_guardam_o_valor() {
        use crate::audio::MicGain;
        let mut a = voice_tab();
        assert_eq!(a.config.mic_gain, MicGain::Auto);
        let antes = saves();
        let _ = a.update(Msg::AutoGain(false));
        assert_eq!(a.config.mic_gain, MicGain::Manual(0.0));
        assert_eq!(saves(), antes + 1);
        let _ = a.update(Msg::Gain(6.0));
        assert_eq!(saves(), antes + 1, "grava só ao soltar");
        let _ = a.update(Msg::VolumeDone);
        let _ = a.update(Msg::AutoGain(true));
        assert_eq!(a.config.audio_prefs().mic_gain, MicGain::Auto);
        let _ = a.update(Msg::AutoGain(false));
        assert_eq!(a.config.audio_prefs().mic_gain, MicGain::Manual(6.0));
    }

    #[test]
    fn slider_do_limiar_respeita_a_faixa() {
        use crate::audio::{MANUAL_MAX_DB, MANUAL_MIN_DB, Sensitivity};
        let mut a = voice_tab();
        let _ = a.update(Msg::AutoSensitivity(false));
        let _ = a.update(Msg::Threshold(-900.0));
        assert_eq!(a.config.sensitivity, Sensitivity::Manual(MANUAL_MIN_DB));
        let _ = a.update(Msg::Threshold(15.0));
        assert_eq!(a.config.sensitivity, Sensitivity::Manual(MANUAL_MAX_DB));
    }

    #[test]
    fn config_so_grava_ao_soltar_o_slider() {
        let mut a = voice_tab();
        let _ = a.update(Msg::AutoSensitivity(false));
        let antes = saves();
        for db in [-50.0, -45.0, -42.0] {
            let _ = a.update(Msg::Threshold(db));
        }
        assert_eq!(saves(), antes, "arrastar não grava");
        let _ = a.update(Msg::VolumeDone);
        assert_eq!(saves(), antes + 1);
        let _ = a.update(Msg::AutoSensitivity(true));
        assert_eq!(saves(), antes + 2, "o interruptor grava na hora");
    }

    #[test]
    fn motor_de_teste_abre_so_na_aba_voz_sem_call() {
        let mut a = voice_app();
        let _ = a.update(Msg::OpenSettings(Tab::Account));
        assert!(matches!(probe_of(&mut a), Probe::Off));
        let _ = a.update(Msg::OpenSettings(Tab::Voice));
        assert!(matches!(probe_of(&mut a), Probe::Opening(1)));
        let _ = a.update(Msg::OpenSettings(Tab::Voice));
        assert!(
            matches!(probe_of(&mut a), Probe::Opening(1)),
            "abrir de novo não abre um segundo motor"
        );
        let _ = a.update(Msg::OpenSettings(Tab::About));
        assert!(
            matches!(probe_of(&mut a), Probe::Off),
            "trocar de aba solta"
        );
    }

    #[test]
    fn linha_de_atualizacao_segue_a_conferencia() {
        let mut a = main_app();
        let _ = a.update(Msg::CheckUpdate);
        assert_eq!(main_of(&mut a).check, Some(Check::Searching));
        let _ = a.update(Msg::Updated(Check::Downloading("0.2.0".into())));
        let _ = a.update(Msg::CheckUpdate);
        assert_eq!(
            main_of(&mut a).check,
            Some(Check::Downloading("0.2.0".into())),
            "no meio não começa outra"
        );
        let _ = a.update(Msg::Updated(Check::Busy));
        assert!(main_of(&mut a).checking(), "a ocupada não apaga a linha");
        let _ = a.update(Msg::Updated(Check::Ready("0.2.0".into())));
        assert_eq!(
            main_of(&mut a).update.as_deref(),
            Some("0.2.0"),
            "seta verde"
        );
        let _ = a.update(Msg::Updated(Check::Failed("x".into())));
        assert_eq!(
            main_of(&mut a).update.as_deref(),
            Some("0.2.0"),
            "a seta fica"
        );
    }

    #[test]
    fn fechar_as_configuracoes_solta_o_motor_de_teste() {
        for msg in [Msg::CloseSettings, Msg::Escape] {
            let mut a = voice_tab();
            let _ = a.update(msg);
            assert!(matches!(probe_of(&mut a), Probe::Off));
            assert!(main_of(&mut a).settings.is_none());
        }
    }

    #[test]
    fn motor_de_teste_que_falha_mostra_erro_e_tenta_de_novo_ao_reabrir() {
        let mut a = voice_tab();
        let _ = a.update(Msg::ProbeStarted(1, Err("sem placa".into())));
        assert!(matches!(probe_of(&mut a), Probe::Failed));
        assert!(!main_of(&mut a).meter_clock());
        let _ = a.update(Msg::OpenSettings(Tab::Voice));
        assert!(matches!(probe_of(&mut a), Probe::Opening(2)));
    }

    #[test]
    fn resposta_atrasada_do_motor_de_teste_e_descartada() {
        let mut a = voice_tab();
        let _ = a.update(Msg::CloseSettings);
        let _ = a.update(Msg::ProbeStarted(1, Err("tarde".into())));
        assert!(matches!(probe_of(&mut a), Probe::Off), "aba já fechada");

        let _ = a.update(Msg::OpenSettings(Tab::Voice));
        assert!(matches!(probe_of(&mut a), Probe::Opening(2)));
        let _ = a.update(Msg::ProbeStarted(1, Err("da abertura antiga".into())));
        assert!(
            matches!(probe_of(&mut a), Probe::Opening(2)),
            "outra abertura em andamento não é afetada"
        );
    }

    #[test]
    fn entrar_na_call_solta_o_motor_de_teste_e_nao_abre_outro() {
        let mut a = voice_tab();
        let _ = a.update(Msg::JoinChannel("call".into()));
        assert!(matches!(probe_of(&mut a), Probe::Off));
        assert!(main_of(&mut a).call.is_some());
        let _ = a.update(Msg::ProbeStarted(1, Err("tarde".into())));
        assert!(matches!(probe_of(&mut a), Probe::Off));
        let _ = a.update(Msg::OpenSettings(Tab::Voice));
        assert!(
            matches!(probe_of(&mut a), Probe::Off),
            "com call, a aba usa o motor da call"
        );
    }

    #[test]
    fn fim_da_call_com_a_aba_voz_aberta_volta_a_medir() {
        let mut a = in_call(&["Ana"]);
        let _ = a.update(Msg::OpenSettings(Tab::Voice));
        let id = call_id(&mut a);
        let _ = a.update(Msg::CallEvent(id, call::Event::Ended(End::RoomClosed)));
        assert!(matches!(probe_of(&mut a), Probe::Opening(_)));
    }

    #[test]
    fn sem_medidor_nao_ha_relogio_nem_leitura() {
        let mut a = voice_tab();
        assert!(!main_of(&mut a).meter_clock());
        let _ = a.update(Msg::MeterTick);
        assert_eq!(main_of(&mut a).level, MANUAL_MIN_DB);
        let _ = a.update(Msg::ProbeStarted(1, Err("x".into())));
        let _ = a.update(Msg::MeterTick);
        assert!(!main_of(&mut a).open);
    }

    /// Gera `snapshots/*.png` sem abrir janela: `cargo test -- --ignored prints`.
    #[test]
    #[ignore]
    fn prints_das_telas() {
        use iced::{Settings, Size};
        use iced_test::simulator::Simulator;

        fn print(a: &App, nome: &str) {
            print_at(a, nome, Size::new(1200.0, 760.0));
        }

        fn print_at(a: &App, nome: &str, size: Size) {
            let settings = Settings {
                fonts: theme::FONTS.map(Into::into).to_vec(),
                default_font: theme::BODY,
                ..Settings::default()
            };
            let mut ui = Simulator::with_size(settings, size, a.view());
            let print = ui.snapshot(&a.theme()).unwrap();
            assert!(
                print
                    .matches_image(format!("snapshots/{nome}.png"))
                    .unwrap()
            );
        }

        let _ = std::fs::remove_dir_all("snapshots");
        let erro = Some("Esse endereço não é um servidor Concord.".to_owned());
        print(
            &app(Screen::Server(ServerForm {
                input: "call.exemplo.com".into(),
                error: erro,
                busy: false,
            })),
            "servidor",
        );
        let mut l = LoginForm::new(
            "https://call.exemplo.com".into(),
            Some("Usuário ou senha inválidos.".into()),
        );
        l.username = "Opedro".into();
        l.password = Secret("segredo".into());
        print(&app(Screen::Login(l)), "login");
        print(
            &app(Screen::Offline("https://call.exemplo.com".into())),
            "offline",
        );
        print(&app(Screen::Connecting), "conectando");

        let canal = |id, name: &str, kind| Channel {
            id,
            name: name.into(),
            slug: name.into(),
            kind,
        };
        let pessoa = |name: &str, muted, camera, screen_share| Participant {
            identity: format!("{name}__1"),
            name: name.into(),
            muted,
            camera,
            screen_share,
        };
        let mut a = main_app();
        let m = main_of(&mut a);
        m.channels = vec![
            canal(1, "geral", ChannelKind::Text),
            canal(2, "memes", ChannelKind::Text),
            canal(3, "Call", ChannelKind::Voice),
            canal(4, "Jogando", ChannelKind::Voice),
        ];
        m.presence = Presence::from([(
            "Call".to_owned(),
            vec![
                pessoa("Zé da Silva", true, false, false),
                pessoa("Ana", false, true, false),
                pessoa(
                    "Bruno com nome bem comprido demais pra caber",
                    false,
                    false,
                    true,
                ),
                pessoa("Ana", true, false, false),
            ],
        )]);
        print(&a, "principal");

        a.config.toggle_deafen();
        main_of(&mut a).failures = FALHAS_ATE_AVISAR;
        for (tab, nome) in [
            (Tab::Server, "config-servidor"),
            (Tab::Account, "config-conta"),
            (Tab::Voice, "config-voz"),
            (Tab::About, "config-sobre"),
        ] {
            main_of(&mut a).settings = Some(tab);
            main_of(&mut a).vault_ok = false;
            if tab == Tab::Voice {
                a.config.input = Some("Microfone USB (hw:1,0)".into());
                main_of(&mut a).devices = Some(Devices {
                    inputs: vec!["Microfone USB (hw:1,0)".into()],
                    outputs: vec!["Alto-falantes".into()],
                });
            }
            print(&a, nome);
        }

        // Linha do "Procurar atualização": baixando (botão parado), pronta e erro.
        main_of(&mut a).settings = Some(Tab::About);
        main_of(&mut a).check = Some(Check::Downloading("0.2.0".into()));
        print(&a, "sobre-baixando");
        main_of(&mut a).check = Some(Check::Ready("0.2.0".into()));
        main_of(&mut a).update = Some("0.2.0".into());
        print(&a, "sobre-pronta");
        main_of(&mut a).update = None;
        main_of(&mut a).check = Some(Check::Failed(
            "A versão 0.2.0 não abriu neste computador. Fica esta até sair a próxima.".into(),
        ));
        print(&a, "sobre-erro");
        main_of(&mut a).check = None;

        // Sensibilidade de entrada: automático (com voz e em silêncio), manual (abaixo e
        // acima do limiar), erro do motor e a janela mínima.
        main_of(&mut a).settings = Some(Tab::Voice);
        main_of(&mut a).vault_ok = true;
        let voz = |a: &mut App, auto: bool, db: f32, level: f32, open: bool| {
            a.config.set_auto(auto);
            a.config.set_threshold(db);
            let m = main_of(a);
            (m.level, m.open, m.probe) = (level, open, Probe::Off);
        };
        voz(&mut a, true, -40.0, -28.0, true);
        print(&a, "voz-auto-falando");
        voz(&mut a, true, -40.0, -62.0, false);
        print(&a, "voz-auto-silencio");
        voz(&mut a, false, -40.0, -55.0, false);
        print(&a, "voz-manual-abaixo");
        voz(&mut a, false, -40.0, -22.0, true);
        print(&a, "voz-manual-acima");
        print_at(&a, "voz-manual-janela-minima", Size::new(860.0, 560.0));
        // Ganho de entrada manual, com a sensibilidade manual acima mostrando o nível.
        a.config.set_auto_gain(false);
        a.config.set_gain(6.0);
        print(&a, "voz-ganho-manual");
        a.config.set_auto_gain(true);
        voz(&mut a, false, -40.0, -22.0, true);
        main_of(&mut a).probe = Probe::Failed;
        print(&a, "voz-erro");

        // Palco: 1, 4 e 9 pessoas, com quem fala e quem está mudo.
        let nomes = [
            "Opedro",
            "Ana",
            "Bruno com nome bem comprido demais",
            "Carla",
            "Duda",
            "Edu",
            "Fabi",
            "Gabi",
            "Hugo",
            "Iara",
            "Jota",
        ];
        let palco = |n: usize| {
            let mut a = voice_app();
            let _ = a.update(Msg::JoinChannel("call".into()));
            let id = call_id(&mut a);
            let _ = a.update(Msg::CallEvent(id, call::Event::Connected));
            let lista = nomes[..n]
                .iter()
                .enumerate()
                .map(|(i, nome)| {
                    let mut l = voice::test_live(nome, i == 1 || i == 3);
                    l.muted = i == 2 || i == 0;
                    l.is_local = i == 0;
                    l
                })
                .collect();
            let _ = a.update(Msg::CallEvent(id, call::Event::Participants(lista)));
            (a, id)
        };
        for (n, nome) in [
            (1, "palco-1"),
            (4, "palco-4"),
            (9, "palco-9"),
            (11, "palco-11"),
        ] {
            print(&palco(n).0, nome);
        }
        // Transmissão da Ana com 1, 3 e 5 espectadores, vista de quem não assiste.
        for (quantos, nome) in [
            (1, "tela-1-espectador"),
            (3, "tela-3-espectadores"),
            (5, "tela-5-espectadores"),
        ] {
            let (mut a, id) = palco(7);
            let mut lista = call_of(&mut a).live.clone().unwrap();
            lista
                .iter_mut()
                .filter(|l| l.name == "Ana")
                .for_each(|l| l.screen_share = true);
            let _ = a.update(Msg::CallEvent(id, call::Event::Participants(lista)));
            let quem = nomes[2..2 + quantos]
                .iter()
                .map(|n| format!("{n}__1"))
                .collect();
            let _ = a.update(Msg::CallEvent(
                id,
                call::Event::Viewers(HashMap::from([("Ana__1".to_owned(), quem)])),
            ));
            print(&a, nome);
        }
        // Fotos sintéticas: o fundo do tile toma a cor dominante de cada uma.
        let (mut a, _) = palco(4);
        for (nome, cor) in
            nomes
                .iter()
                .zip([[82, 38, 15], [121, 82, 88], [255, 216, 69], [110, 75, 45]])
        {
            let foto =
                ::image::RgbaImage::from_pixel(8, 8, ::image::Rgba([cor[0], cor[1], cor[2], 255]));
            let tom = Color::from_rgb8(cor[0], cor[1], cor[2]);
            let m = main_of(&mut a);
            m.avatar_paths.insert((*nome).into(), format!("/{nome}"));
            m.avatars.insert(
                format!("/{nome}"),
                (image::Handle::from_rgba(8, 8, foto.into_raw()), tom),
            );
        }
        print(&a, "palco-cores");
        // Câmera ligada: o quadro cobre o tile, em 16:9 e em 4:3, com o anel por cima.
        let (mut a, id) = palco(4);
        let lista = nomes[..4]
            .iter()
            .enumerate()
            .map(|(i, nome)| {
                let mut l = voice::test_live(nome, i == 1);
                l.is_local = i == 0;
                l.camera = i == 1 || i == 2;
                l
            })
            .collect();
        let _ = a.update(Msg::CallEvent(id, call::Event::Participants(lista)));
        for (who, h) in [
            ("Ana__1", 360),
            ("Bruno com nome bem comprido demais__1", 480),
        ] {
            let _ = a.update(Msg::CameraReady(who.into(), Some(alocada(640, h))));
        }
        print(&a, "palco-camera");
        let (mut a, id) = palco(4);
        let _ = a.update(Msg::CallEvent(id, call::Event::Reconnecting { attempt: 3 }));
        let _ = a.update(Msg::OpenVolume("Ana".into()));
        a.config.volumes.insert("Ana".into(), 0);
        print(&a, "palco-reconectando-volume");
        let _ = a.update(Msg::CallEvent(id, call::Event::Connected));
        let _ = a.update(Msg::ToggleDeafen);
        a.config.volumes.insert("Ana".into(), 40);
        print(&a, "palco-surdo-volume");

        // Transmissão: painel de qualidade, seletor do sistema, no ar e falha.
        let (mut a, id) = palco(4);
        a.config.share_quality = Quality::Media;
        let _ = a.update(Msg::ShareButton);
        print(&a, "tela-painel");
        print_at(&a, "tela-painel-janela-minima", Size::new(860.0, 560.0));
        // Clique de verdade: a camada do painel não pode tampar a barra de baixo.
        let settings = Settings {
            fonts: theme::FONTS.map(Into::into).to_vec(),
            default_font: theme::BODY,
            ..Settings::default()
        };
        // Seletor do Windows, que no Linux não abre: nome comprido corta na miniatura.
        let janela = |id, title: &str| {
            let source = crate::share::Source {
                window: true,
                id,
                title: title.into(),
            };
            (source, None)
        };
        call_of(&mut a).set_share_tab(true);
        call_of(&mut a).set_share_sources(vec![
            janela(
                1,
                "Concord - chamada de voz self-hosted, sem anúncio - Google Chrome",
            ),
            janela(2, "Spotify"),
            janela(3, "WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW"),
            janela(4, "Planilha de gastos de novembro.xlsx - Excel"),
        ]);
        if let Screen::Main(m) = &a.screen {
            let seletor = stage::picker(m.call.as_ref().unwrap(), &a.config);
            let mut ui = Simulator::with_size(settings.clone(), Size::new(660.0, 480.0), seletor);
            let foto = ui.snapshot(&a.theme()).unwrap();
            assert!(foto.matches_image("snapshots/tela-seletor.png").unwrap());
        }
        call_of(&mut a).set_share_tab(false);
        let mut ui = Simulator::with_size(settings, Size::new(1200.0, 760.0), a.view());
        ui.click("Transmitir").unwrap();
        ui.click("Sair").unwrap();
        let cliques: Vec<Msg> = ui.into_messages().collect();
        assert!(
            // Com o painel aberto, clicar na barra só fecha o painel: o Sair não chega.
            matches!(cliques[..], [Msg::ShareStart, Msg::CloseVolume]),
            "{cliques:?}"
        );
        let _ = a.update(Msg::ShareStart);
        share(&mut a, ShareState::Choosing);
        print(&a, "tela-escolhendo");
        let lista = nomes[..4]
            .iter()
            .enumerate()
            .map(|(i, nome)| {
                let mut l = voice::test_live(nome, false);
                l.is_local = i == 0;
                l.screen_share = i == 0;
                l
            })
            .collect();
        let _ = a.update(Msg::CallEvent(id, call::Event::Participants(lista)));
        share(&mut a, ShareState::Live);
        print(&a, "tela-transmitindo");
        call_of(&mut a).own = Some(alocada(480, 270));
        print(&a, "tela-propria");
        let _ = a.update(Msg::Focus(false));
        print(&a, "tela-propria-pausada");
        let _ = a.update(Msg::Focus(true));
        audio(&mut a, Choice::All, &["Google Chrome", "Spotify"], false);
        let _ = a.update(Msg::ShareMenu);
        print(&a, "tela-menu-audio-ligado");
        audio(&mut a, Choice::Silent, &[], false);
        print(&a, "tela-menu-audio-mudo");
        let (mut a, _) = palco(4);
        share(&mut a, ShareState::Failed);
        print(&a, "tela-falha");

        let mut a = voice_app();
        let _ = a.update(Msg::JoinChannel("call".into()));
        print(&a, "palco-conectando");
        let id = call_id(&mut a);
        let _ = a.update(Msg::CallEvent(id, call::Event::Ended(End::RoomClosed)));
        print(&a, "home-aviso");
    }
}
