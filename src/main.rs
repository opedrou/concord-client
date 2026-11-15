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
mod awake;
mod call;
mod chat;
mod config;
mod health;
mod icon;
mod log;
mod session;
mod share;
mod ui;
mod update;

use iced::{Size, window};
use ui::{App, theme};

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
    if update::start() {
        return Ok(());
    }
    health::start();
    #[cfg(target_os = "linux")]
    update::register();
    let window = window::Settings {
        size: Size::new(1200.0, 760.0),
        min_size: Some(Size::new(860.0, 560.0)),
        icon: icon::window(),
        #[cfg(target_os = "linux")]
        platform_specific: window::settings::PlatformSpecific {
            application_id: update::APP_ID.into(),
            ..Default::default()
        },
        // Com call ativa, a saída espera o `Ended` antes de fechar.
        exit_on_close_request: false,
        ..window::Settings::default()
    };
    let mut app = iced::application(App::boot, App::update, App::view)
        .title("Concord")
        .theme(App::theme)
        .subscription(App::subscription)
        .default_font(theme::BODY)
        .window(window);
    for font in theme::FONTS {
        app = app.font(font);
    }
    let result = app.run();
    if result.is_ok() {
        update::closed();
    }
    result
}
