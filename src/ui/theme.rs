//! Tokens do `styles/globals.css` da web, tema escuro.
use iced::widget::{button, container, overlay::menu, pick_list, scrollable, text_input};
use iced::{
    Background, Border, Color, Font, Gradient, Radians, Shadow, Theme, Vector, border, font,
    gradient,
};

/// Fundo do palco da call (`--stage-bg`).
pub const STAGE: Color = Color::BLACK;
pub const BG: Color = Color::from_rgb8(0x18, 0x18, 0x18);
pub const BG2: Color = Color::from_rgb8(0x12, 0x12, 0x14);
pub const BG3: Color = Color::from_rgb8(0x20, 0x20, 0x24);
pub const SURFACE: Color = Color::from_rgb8(0x3d, 0x3d, 0x3d);
pub const FG: Color = Color::WHITE;
pub const FG2: Color = Color::from_rgb8(0xaa, 0xaa, 0xaa);
pub const FG3: Color = Color::from_rgba8(255, 255, 255, 0.42);
pub const BORDER: Color = Color::from_rgba8(255, 255, 255, 0.10);
pub const BORDER2: Color = Color::from_rgba8(255, 255, 255, 0.16);
pub const ACCENT: Color = Color::from_rgb8(0x9d, 0x4e, 0xdd);
pub const ACCENT2: Color = Color::from_rgb8(0xc7, 0x7d, 0xff);
pub const ACCENT3: Color = Color::from_rgb8(0x7b, 0x2c, 0xbf);
pub const SOFT: Color = Color::from_rgba8(157, 78, 221, 0.20);
pub const GREEN: Color = Color::from_rgb8(0x7e, 0xc8, 0x82);
pub const DANGER: Color = Color::from_rgb8(0xf0, 0x62, 0x5d);
pub const DANGER_SOFT: Color = Color::from_rgba8(240, 98, 93, 0.18);
pub const IDLE: Color = Color::from_rgb8(0xf0, 0xb4, 0x5e);
pub const HOVER: Color = Color::from_rgba8(255, 255, 255, 0.06);
pub const HOVER_STRONG: Color = Color::from_rgba8(255, 255, 255, 0.10);
const BACKDROP: Color = Color::from_rgba8(0, 0, 0, 0.6);

pub const BODY: Font = Font::with_name("Hanken Grotesk");
pub const MEDIUM: Font = Font {
    weight: font::Weight::Medium,
    ..BODY
};
pub const SEMIBOLD: Font = Font {
    weight: font::Weight::Semibold,
    ..BODY
};
pub const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..BODY
};
pub const DISPLAY: Font = Font {
    weight: font::Weight::Bold,
    ..Font::with_name("Bricolage Grotesque")
};
pub const DISPLAY_HEAVY: Font = Font {
    weight: font::Weight::ExtraBold,
    ..DISPLAY
};

/// Pesos estáticos: o `iced` não aplica eixo de fonte variável.
pub const FONTS: [&[u8]; 6] = [
    include_bytes!("../../assets/fonts/HankenGrotesk-400.ttf"),
    include_bytes!("../../assets/fonts/HankenGrotesk-500.ttf"),
    include_bytes!("../../assets/fonts/HankenGrotesk-600.ttf"),
    include_bytes!("../../assets/fonts/HankenGrotesk-700.ttf"),
    include_bytes!("../../assets/fonts/BricolageGrotesque-700.ttf"),
    include_bytes!("../../assets/fonts/BricolageGrotesque-800.ttf"),
];

pub fn theme() -> Theme {
    Theme::custom(
        "Concord",
        iced::theme::Palette {
            background: BG,
            text: FG,
            primary: ACCENT,
            success: GREEN,
            warning: IDLE,
            danger: DANGER,
        },
    )
}

pub fn fill(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style::default().background(color)
}

pub fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(BG2.into()),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 18.0.into(),
        },
        ..Default::default()
    }
}

pub fn backdrop(_: &Theme) -> container::Style {
    container::Style::default().background(BACKDROP)
}

pub fn live_badge(_: &Theme) -> container::Style {
    container::Style {
        background: Some(DANGER.into()),
        border: border::rounded(4),
        ..Default::default()
    }
}

pub fn tooltip(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE.into()),
        border: border::rounded(8),
        ..Default::default()
    }
}

pub fn field(_: &Theme, status: text_input::Status) -> text_input::Style {
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: BG.into(),
        border: Border {
            color: if focused { ACCENT } else { BORDER2 },
            width: 1.0,
            radius: 11.0.into(),
        },
        icon: FG2,
        placeholder: FG3,
        value: FG,
        selection: SOFT,
    }
}

pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    let alpha = if status == button::Status::Disabled {
        0.55
    } else {
        1.0
    };
    let (start, end) = if status == button::Status::Hovered {
        (ACCENT, ACCENT)
    } else {
        (ACCENT, ACCENT3)
    };
    let gradient = gradient::Linear::new(Radians(std::f32::consts::FRAC_PI_2 + 0.78))
        .add_stop(0.0, Color { a: alpha, ..start })
        .add_stop(1.0, Color { a: alpha, ..end });
    button::Style {
        background: Some(Background::Gradient(Gradient::Linear(gradient))),
        text_color: Color { a: alpha, ..FG },
        border: border::rounded(12),
        ..Default::default()
    }
}

/// Botão sem fundo: link da tela de login, item de menu, botão de ícone.
pub fn ghost(_: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(HOVER_STRONG.into()),
        _ => None,
    };
    let text_color = if status == button::Status::Disabled {
        FG3
    } else {
        FG2
    };
    button::Style {
        background,
        text_color,
        border: border::rounded(9),
        ..Default::default()
    }
}

pub fn outline(_: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(HOVER.into()),
        _ => None,
    };
    button::Style {
        background,
        text_color: FG,
        border: Border {
            color: BORDER2,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

pub fn danger(_: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => Some(DANGER_SOFT.into()),
        _ => None,
    };
    button::Style {
        background,
        text_color: DANGER,
        border: Border {
            color: DANGER,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

pub fn nav_item(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let background = match (active, status) {
            (true, _) => Some(SOFT.into()),
            (false, button::Status::Hovered | button::Status::Pressed) => Some(HOVER.into()),
            _ => None,
        };
        button::Style {
            background,
            text_color: if active { ACCENT2 } else { FG2 },
            border: border::rounded(9),
            ..Default::default()
        }
    }
}

/// Barra fina, sem trilho, pra não pesar na sidebar.
pub fn thin_scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let mut style = scrollable::default(theme, status);
    let rail = scrollable::Rail {
        background: None,
        border: border::rounded(4),
        scroller: scrollable::Scroller {
            background: HOVER_STRONG.into(),
            border: border::rounded(4),
        },
    };
    style.vertical_rail = rail;
    style.horizontal_rail = rail;
    style
}

/// Nome do tile: escuro translúcido, como o `--pill` da web.
pub fn pill(_: &Theme) -> container::Style {
    container::Style {
        background: Some(BACKDROP.into()),
        border: border::rounded(8),
        ..Default::default()
    }
}

/// Barra de controles da call: pílula de raio 18 com sombra funda.
pub fn bar(_: &Theme) -> container::Style {
    container::Style {
        background: Some(BG2.into()),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 18.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.4),
            offset: Vector::new(0.0, 12.0),
            blur_radius: 30.0,
        },
        ..Default::default()
    }
}

/// Quadrado de 46 px da barra. `off` é mic mudo ou fone desligado: vermelho suave.
pub fn bar_button(off: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = match (off, hot) {
            (true, false) => DANGER_SOFT,
            (true, true) => Color { a: 0.3, ..DANGER },
            (false, false) => BG3,
            (false, true) => SURFACE,
        };
        button::Style {
            background: Some(background.into()),
            text_color: if off { DANGER } else { FG2 },
            border: border::rounded(13),
            ..Default::default()
        }
    }
}

/// "Sair" da barra: o único cheio de vermelho.
pub fn leave(_: &Theme, status: button::Status) -> button::Style {
    let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(
            Color {
                a: if hot { 0.85 } else { 1.0 },
                ..DANGER
            }
            .into(),
        ),
        text_color: FG,
        border: border::rounded(13),
        ..Default::default()
    }
}

pub fn pick(_: &Theme, status: pick_list::Status) -> pick_list::Style {
    let open = matches!(status, pick_list::Status::Opened { .. });
    pick_list::Style {
        text_color: FG,
        placeholder_color: FG3,
        handle_color: FG2,
        background: BG.into(),
        border: Border {
            color: if open { ACCENT } else { BORDER2 },
            width: 1.0,
            radius: 11.0.into(),
        },
    }
}

pub fn pick_menu(_: &Theme) -> menu::Style {
    menu::Style {
        background: BG3.into(),
        border: Border {
            color: BORDER2,
            width: 1.0,
            radius: 11.0.into(),
        },
        text_color: FG2,
        selected_text_color: ACCENT2,
        selected_background: SOFT.into(),
        shadow: Shadow::default(),
    }
}
