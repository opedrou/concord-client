#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod audio;
mod call;
mod config;
mod session;
mod ui;

use iced::{Size, window};
use ui::{App, theme};

fn main() -> iced::Result {
    let window = window::Settings {
        size: Size::new(1200.0, 760.0),
        min_size: Some(Size::new(860.0, 560.0)),
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
