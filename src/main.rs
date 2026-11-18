#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Uma linha no log desta execução; ver `log::line`. Definido antes dos módulos pra valer
/// em todos eles.
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::line(format_args!($($arg)*))
    };
}

mod api;
mod audio;
mod autostart;
mod awake;
mod call;
mod chat;
mod config;
mod health;
mod icon;
mod instance;
mod log;
mod session;
mod share;
mod ui;
mod update;

use ui::{App, Msg, theme};

fn main() -> iced::Result {
    // A atualização roda o binário baixado assim pra ver se ele abre neste computador.
    if std::env::args_os().any(|a| a == "--version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    log::capture_crates();
    // Pânico numa tarefa da sala some calado: ao menos fica no log, com o caminho até ele.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let trace = std::backtrace::Backtrace::force_capture();
        log!("pânico: {info}\n{trace}");
        default_hook(info);
    }));
    // `--register` e `--uninstall` rodam sem janela e sem tocar no log.
    #[cfg(target_os = "linux")]
    if update::cli() {
        return Ok(());
    }
    // Antes do cabeçalho: a cópia de fora que só abre a instalada não gasta um arquivo de log.
    #[cfg(windows)]
    if update::setup() {
        return Ok(());
    }
    // Já tem um Concord aberto: abertura normal mostra a janela dele, a do autostart não
    // pede nada. Antes do primeiro `log!`, pra não gastar um arquivo de log.
    let background = autostart::background();
    if instance::hand_off((!background).then_some(instance::Command::Show)) {
        return Ok(());
    }
    // Em segundo plano sem servidor ou sem login não tem o que escutar.
    if background
        && !config::Config::read()
            .server
            .is_some_and(|s| session::stored_blocking(&s))
    {
        return Ok(());
    }
    if update::start() {
        return Ok(());
    }
    instance::bind();
    #[cfg(windows)]
    update::win::set_app_id();
    health::start();
    #[cfg(target_os = "linux")]
    update::register();
    // Daemon: fechar a janela não encerra o processo, e ele pode abrir sem janela.
    let mut app = iced::daemon(App::boot, App::update, view)
        .title("Concord")
        .theme(|a: &App, _| a.theme())
        .subscription(App::subscription)
        .default_font(theme::BODY);
    for font in theme::FONTS {
        app = app.font(font);
    }
    let result = app.run();
    instance::release();
    if result.is_ok() {
        update::closed();
    }
    result
}

/// Uma janela só: a mesma tela pra qualquer id.
fn view(app: &App, _: iced::window::Id) -> iced::Element<'_, Msg> {
    app.view()
}
