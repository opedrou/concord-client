//! Telas e estado do app. A rede roda em `Task`; o resto é síncrono.
mod avatar;
mod screens;
mod sidebar;
pub mod theme;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{image, operation, row, stack};
use iced::{Element, Event, Subscription, Task, Theme, event, mouse, time, window};

use crate::api::{self, AddressError, Channel, Client, Error, Member, Presence, Secret, User};
use crate::config::{Config, SIDEBAR_MAX, SIDEBAR_MIN};
use crate::session;

const POLL: Duration = Duration::from_secs(4);
/// Falhas seguidas de presença até o rodapé trocar "Online" por "Sem conexão".
const FALHAS_ATE_AVISAR: u8 = 3;

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
    avatars: HashMap<String, image::Handle>,
    requested: HashSet<String>,
    settings: Option<Tab>,
    resizing: bool,
    vault_ok: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tab {
    Server,
    Account,
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
    Avatar(String, Option<image::Handle>),
    ToggleMic,
    ToggleDeafen,
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
        let mut subs = vec![keyboard::listen().filter_map(keys)];
        if let Screen::Main(m) = &self.screen {
            subs.push(time::every(POLL).map(|_| Msg::Tick));
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
                let base = row![
                    sidebar::view(m, &self.config),
                    sidebar::resize_handle(),
                    screens::home(&m.user.username),
                ];
                match m.settings {
                    Some(tab) => stack![base, screens::settings(m, tab)].into(),
                    None => base.into(),
                }
            }
        }
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
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
                Task::none()
            }
            Msg::ToggleDeafen => {
                self.config.toggle_deafen();
                self.persist();
                Task::none()
            }
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
                Task::none()
            }
            Msg::CloseSettings | Msg::Escape => {
                if let Screen::Main(m) = &mut self.screen {
                    m.settings = None;
                }
                Task::none()
            }
            Msg::Logout => self.logout(),
            Msg::FocusNext => operation::focus_next(),
            Msg::FocusPrevious => operation::focus_previous(),
        }
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
        };
        let avatars = main.fetch_avatars();
        self.screen = Screen::Main(Box::new(main));
        Task::batch([
            avatars,
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
                self.screen = Screen::Offline(m.server.clone());
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
        self.avatars.get(self.avatar_paths.get(name)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ChannelKind, Participant};

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

    /// Gera `snapshots/*.png` sem abrir janela: `cargo test -- --ignored prints`.
    #[test]
    #[ignore]
    fn prints_das_telas() {
        use iced::{Settings, Size};
        use iced_test::simulator::Simulator;

        fn print(a: &App, nome: &str) {
            let settings = Settings {
                fonts: theme::FONTS.map(Into::into).to_vec(),
                default_font: theme::BODY,
                ..Settings::default()
            };
            let mut ui = Simulator::with_size(settings, Size::new(1200.0, 760.0), a.view());
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
            (Tab::About, "config-sobre"),
        ] {
            main_of(&mut a).settings = Some(tab);
            main_of(&mut a).vault_ok = false;
            print(&a, nome);
        }
    }
}
