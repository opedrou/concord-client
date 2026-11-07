#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod audio;
mod call;
mod config;
mod icon;
mod session;
mod share;
mod ui;
mod update;

use iced::{Size, window};
use ui::{App, theme};

fn main() -> iced::Result {
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
