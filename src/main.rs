#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Uma linha no `concord.log`; ver `log::line`. Definido antes dos módulos pra valer em
/// todos eles.
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log::line(format_args!($($arg)*))
    };
}

mod api;
mod audio;
mod awake;
mod call;
mod config;
mod icon;
mod log;
mod session;
mod share;
mod ui;
mod update;

use iced::{Size, window};
use ui::{App, theme};

fn main() -> iced::Result {
    // Pânico numa tarefa da sala some calado: ao menos fica no log.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log!("pânico: {info}");
        default_hook(info);
    }));
    // `--register` e `--uninstall` rodam sem janela e sem tocar no log.
    #[cfg(target_os = "linux")]
    if update::cli() {
        return Ok(());
    }
    log!("Concord {} começou", env!("CARGO_PKG_VERSION"));
    #[cfg(windows)]
    if update::setup() {
        return Ok(());
    }
    update::cleanup();
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
    app.run()
}
