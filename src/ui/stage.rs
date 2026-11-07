//! Palco da call: grade de tiles, barra de controles e painel de volume.
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, center, column, container, mouse_area, opaque, pin, responsive, row,
    slider, space, stack, text,
};
use iced::{Alignment, Color, Element, Length, Padding, Size, Theme, border};

use super::sidebar::{
    CLOSE, HEADPHONES, HEADPHONES_OFF, MIC, MIC_OFF, SCREEN, VOLUME, VOLUME_X, icon, live_badge,
    tipped,
};
use super::theme::{self, ACCENT, ACCENT2, BG2, BORDER, BORDER2, DANGER, FG, FG2, GREEN, IDLE};
use super::voice::{self, ActiveCall, GAP, Status};
use super::{Main, Msg, avatar};
use crate::call::{Live, MAX_ATTEMPTS};
use crate::config::Config;
use crate::share::{Quality, State};

/// Largura fixa e altura aproximada do painel de volume, pra centrar no tile.
const PANEL: Size = Size::new(280.0, 104.0);
/// Largura do painel de qualidade.
const SHARE_PANEL: f32 = 340.0;
/// Barra de controles com a margem dela, pra os avisos abrirem logo acima.
const ABOVE_BAR: f32 = 94.0;

pub fn view<'a>(m: &'a Main, call: &'a ActiveCall, config: &'a Config) -> Element<'a, Msg> {
    let body: Element<_> = match &call.live {
        Some(live) if !live.is_empty() => {
            responsive(move |area| grid(m, call, config, live, area)).into()
        }
        _ => center(text("Conectando…").size(15).color(FG2)).into(),
    };
    let base = column![
        container(body).height(Length::Fill).width(Length::Fill),
        container(bar(call, config))
            .center_x(Length::Fill)
            .padding([12, 20]),
    ];

    let mut layers = vec![container(base).style(theme::fill(theme::STAGE)).into()];
    if let Status::Reconnecting(n) = call.status {
        layers.push(banner(n));
    }
    layers.extend(share_overlay(call, config));
    stack(layers).into()
}

fn grid<'a>(
    m: &'a Main,
    call: &'a ActiveCall,
    config: &'a Config,
    live: &'a [Live],
    area: Size,
) -> Element<'a, Msg> {
    let size = voice::tile_size(area, live.len());
    let (cols, _) = voice::grid(live.len());
    let rows = live.chunks(cols).map(|chunk| {
        Row::with_children(chunk.iter().map(|p| tile(m, call, p, size)))
            .spacing(GAP)
            .into()
    });
    let tiles = center(Column::with_children(rows).spacing(GAP));
    let panel = call.volume_panel.as_deref().and_then(|name| {
        let index = live.iter().position(|p| voice::name_of(p) == name)?;
        let at = voice::panel_origin(area, live.len(), index, PANEL);
        Some(pin(volume_card(call, config, name)).x(at.x).y(at.y))
    });
    match panel {
        Some(panel) => stack![tiles, panel].into(),
        None => tiles.into(),
    }
}

fn tile<'a>(m: &'a Main, call: &'a ActiveCall, p: &'a Live, size: Size) -> Element<'a, Msg> {
    let name = voice::name_of(p);
    let speaking = call.speaking(name);
    let face = size.width.min(size.height) * 0.34;
    let face = avatar::view(name, m.avatar(name), face.min(96.0));

    let label = text(name)
        .size(13)
        .font(theme::SEMIBOLD)
        .wrapping(Wrapping::None);
    let tag = row![]
        .push(p.screen_share.then(live_badge))
        .push(p.muted.then(|| icon(MIC_OFF, 14.0, DANGER)))
        .push(
            container(label)
                .max_width(size.width - if p.screen_share { 110.0 } else { 70.0 })
                .clip(true),
        )
        .spacing(6)
        .align_y(Alignment::Center);
    let tag = container(tag).padding([5, 10]).style(theme::pill);

    let body = stack![
        center(face),
        container(tag)
            .align_left(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(10),
    ];
    let ring = if speaking {
        GREEN
    } else {
        iced::Color::TRANSPARENT
    };
    let tile = container(body)
        .width(size.width)
        .height(size.height)
        .clip(true)
        .style(move |_| container::Style {
            background: Some(BG2.into()),
            border: border::rounded(18).color(ring).width(2),
            ..Default::default()
        });
    if p.is_local {
        tile.into()
    } else {
        mouse_area(tile)
            .on_right_press(Msg::OpenVolume(name.to_owned()))
            .into()
    }
}

fn bar<'a>(call: &'a ActiveCall, config: &'a Config) -> Element<'a, Msg> {
    let square = |glyph, off, msg, tip| {
        let color = if off { DANGER } else { FG2 };
        let b = button(container(icon(glyph, 20.0, color)).center(46))
            .padding(0)
            .style(theme::bar_button(off))
            .on_press(msg);
        tipped(b.into(), tip)
    };
    let mic = square(
        if config.mic_muted { MIC_OFF } else { MIC },
        config.mic_muted,
        Msg::ToggleMic,
        if config.mic_muted {
            "Ligar o microfone"
        } else {
            "Desligar o microfone"
        },
    );
    let ears = square(
        if config.deafened {
            HEADPHONES_OFF
        } else {
            HEADPHONES
        },
        config.deafened,
        Msg::ToggleDeafen,
        if config.deafened {
            "Voltar a ouvir"
        } else {
            "Parar de ouvir"
        },
    );
    let on_air = matches!(call.share, State::Choosing | State::Live);
    let screen =
        button(container(icon(SCREEN, 20.0, if on_air { ACCENT2 } else { FG2 })).center(46))
            .padding(0)
            .style(theme::bar_toggle(on_air))
            .on_press_maybe(call.call.is_some().then_some(Msg::ShareButton));
    let screen = tipped(
        screen.into(),
        match call.share {
            State::Live => "Parar de transmitir",
            State::Choosing => "Cancelar",
            _ => "Transmitir a tela",
        },
    );
    let separator = container(space())
        .width(1)
        .height(28)
        .style(theme::fill(BORDER2));
    let leave = button(
        text("Sair")
            .size(15)
            .font(theme::BOLD)
            .center()
            .height(Length::Fill),
    )
    .padding([0, 20])
    .height(46)
    .style(theme::leave)
    .on_press(Msg::Leave);

    container(
        row![mic, ears, screen, separator, leave]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([8, 10])
    .style(theme::bar)
    .into()
}

fn banner<'a>(attempt: u32) -> Element<'a, Msg> {
    let label = match attempt {
        0 => "Reconectando…".to_owned(),
        n => format!("Reconectando ({n} de {MAX_ATTEMPTS})"),
    };
    let label = text(label).size(13.5).font(theme::SEMIBOLD).color(IDLE);
    container(container(label).padding([7, 14]).style(theme::tooltip))
        .center_x(Length::Fill)
        .padding(14)
        .into()
}

/// O que a transmissão mostra logo acima da barra: nunca mais de um por vez.
fn share_overlay<'a>(call: &ActiveCall, config: &'a Config) -> Option<Element<'a, Msg>> {
    let content = match call.share {
        State::Choosing => choosing(),
        State::Failed => {
            let label = text("Não foi possível transmitir a tela.")
                .size(13.5)
                .font(theme::SEMIBOLD)
                .color(IDLE);
            container(label)
                .padding([7, 14])
                .style(theme::tooltip)
                .into()
        }
        _ if call.share_panel => share_card(config),
        _ => return None,
    };
    Some(
        container(opaque(content))
            .center_x(Length::Fill)
            .align_bottom(Length::Fill)
            .padding(Padding::ZERO.bottom(ABOVE_BAR))
            .into(),
    )
}

fn choosing<'a>() -> Element<'a, Msg> {
    let cancel = button(text("Cancelar").size(13).font(theme::SEMIBOLD))
        .padding([5, 12])
        .style(theme::outline)
        .on_press(Msg::ShareStop);
    let line = row![
        text("Escolha a tela na janela do sistema…")
            .size(13.5)
            .font(theme::SEMIBOLD),
        cancel,
    ]
    .spacing(14)
    .align_y(Alignment::Center);
    container(line)
        .padding([7, 14])
        .style(theme::tooltip)
        .into()
}

/// Qualidade da próxima transmissão. Sem fundo clicável, como o painel de volume.
fn share_card(config: &Config) -> Element<'_, Msg> {
    let close = button(container(icon(CLOSE, 16.0, FG2)).center(28))
        .padding(0)
        .style(theme::ghost)
        .on_press(Msg::ShareButton);
    let head = row![
        text("Transmitir a tela")
            .size(14.4)
            .font(theme::SEMIBOLD)
            .width(Length::Fill),
        close
    ]
    .align_y(Alignment::Center);

    let options = Quality::ALL.map(|quality| {
        let label = row![
            text(quality.label())
                .size(14)
                .font(theme::MEDIUM)
                .width(Length::Fill),
            text(quality.detail()).size(12.5),
        ]
        .align_y(Alignment::Center);
        button(label)
            .padding([8, 12])
            .width(Length::Fill)
            .style(theme::nav_item(quality == config.share_quality))
            .on_press(Msg::ShareQuality(quality))
            .into()
    });
    let start = button(
        text("Transmitir")
            .size(14.5)
            .font(theme::BOLD)
            .center()
            .width(Length::Fill),
    )
    .padding(10)
    .width(Length::Fill)
    .style(theme::primary)
    .on_press(Msg::ShareStart);

    container(column![head, Column::with_children(options).spacing(2), start].spacing(10))
        .width(SHARE_PANEL)
        .padding(12)
        .style(floating(BORDER))
        .into()
}

/// Painel solto sobre o palco.
fn floating(ring: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(BG2.into()),
        text_color: Some(FG),
        border: border::rounded(14).color(ring).width(1),
        shadow: iced::Shadow {
            color: iced::Color::from_rgba8(0, 0, 0, 0.45),
            offset: iced::Vector::new(0.0, 12.0),
            blur_radius: 32.0,
        },
        ..Default::default()
    }
}

/// Volume de uma pessoa. Sem fundo clicável: fecha no X ou no Escape, e a call segue.
fn volume_card<'a>(call: &'a ActiveCall, config: &'a Config, name: &'a str) -> Element<'a, Msg> {
    let volume = config.volume_of(name);
    let close = button(container(icon(CLOSE, 16.0, FG2)).center(28))
        .padding(0)
        .style(theme::ghost)
        .on_press(Msg::CloseVolume);
    let head = row![
        container(
            text(name)
                .size(14.4)
                .font(theme::SEMIBOLD)
                .wrapping(Wrapping::None)
        )
        .width(Length::Fill)
        .clip(true),
        close
    ]
    .align_y(Alignment::Center);

    let owned = name.to_owned();
    let level = slider(0..=100u8, volume, move |v| Msg::Volume(owned.clone(), v))
        .on_release(Msg::VolumeDone)
        .width(Length::Fill);
    let line = row![
        icon(
            if volume == 0 { VOLUME_X } else { VOLUME },
            18.0,
            if volume == 0 { DANGER } else { FG2 }
        ),
        text("Voz").size(13).color(FG2),
        level,
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let ring = if call.speaking(name) { ACCENT } else { BORDER };
    container(column![head, line].spacing(10))
        .width(PANEL.width)
        .padding(12)
        .style(floating(ring))
        .into()
}
