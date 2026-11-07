//! Coluna de canais: marca, listas, quem está em cada canal e rodapé.
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, button, column, container, mouse_area, row, scrollable, space, stack, svg, text,
    tooltip,
};
use iced::{Alignment, Color, Element, Length, Padding, border, mouse};

use super::theme::{self, ACCENT2, BG2, BG3, BORDER, DANGER, FG, FG2, FG3, GREEN, IDLE};
use super::{FALHAS_ATE_AVISAR, Main, Msg, Tab, avatar, voice};
use crate::api::{self, Channel, ChannelKind};
use crate::config::Config;

macro_rules! icon {
    ($name:literal) => {
        include_bytes!(concat!("../../assets/icons/", $name, ".svg")).as_slice()
    };
}

const HASH: &[u8] = icon!("hash");
const SPEAKER: &[u8] = icon!("speaker");
pub const MIC: &[u8] = icon!("mic");
pub const MIC_OFF: &[u8] = icon!("mic-off");
pub const HEADPHONES: &[u8] = icon!("headphones");
pub const HEADPHONES_OFF: &[u8] = icon!("headphones-off");
pub const VOLUME: &[u8] = icon!("volume");
pub const VOLUME_X: &[u8] = icon!("volume-x");
const SETTINGS: &[u8] = icon!("settings");
const VIDEO: &[u8] = icon!("video");
pub const SCREEN: &[u8] = icon!("screen-share");
pub const CLOSE: &[u8] = icon!("close");
pub const MARK: &[u8] = icon!("mark");

pub fn icon<'a>(bytes: &'static [u8], size: f32, color: Color) -> Element<'a, Msg> {
    svg(svg::Handle::from_memory(bytes))
        .width(size)
        .height(size)
        .style(move |_, _| svg::Style { color: Some(color) })
        .into()
}

pub fn live_badge<'a>() -> Element<'a, Msg> {
    let label = text("LIVE").size(9).font(theme::BOLD).color(FG);
    container(label)
        .padding([1, 5])
        .style(theme::live_badge)
        .into()
}

pub fn mark<'a>(size: f32) -> Element<'a, Msg> {
    svg(svg::Handle::from_memory(MARK))
        .width(size)
        .height(size)
        .into()
}

pub fn view<'a>(m: &'a Main, config: &'a Config) -> Element<'a, Msg> {
    let brand = container(
        row![mark(34.0), text("Concord").size(17).font(theme::DISPLAY)]
            .spacing(11)
            .align_y(Alignment::Center),
    )
    .padding([15, 16])
    .width(Length::Fill);

    let of_kind = |kind| m.channels.iter().filter(move |c: &&Channel| c.kind == kind);
    let text_rows = of_kind(ChannelKind::Text).map(|c| channel_row(HASH, &c.name, None));
    let voice_rows = of_kind(ChannelKind::Voice).map(|c| {
        let active = m.call.as_ref().filter(|call| call.slug == c.slug);
        let go = Some((c.slug.clone(), active.is_some()));
        let people = match active.and_then(|call| call.live.as_ref()) {
            // Na call ativa a lista vem da sala, não do polling.
            Some(live) => live
                .iter()
                .map(|l| {
                    let name = voice::name_of(l);
                    occupant(
                        m,
                        name,
                        (l.muted, l.camera, l.screen_share),
                        active.is_some_and(|call| call.speaking(name)),
                    )
                })
                .collect(),
            None => api::occupants(m.presence.get(&c.slug).map_or(&[][..], Vec::as_slice))
                .into_iter()
                .map(|p| {
                    occupant(
                        m,
                        p.display_name(),
                        (p.muted, p.camera, p.screen_share),
                        false,
                    )
                })
                .collect(),
        };
        column![channel_row(SPEAKER, &c.name, go), occupants(people)]
            .spacing(2)
            .into()
    });
    let lists = column![
        section("Canais de texto", text_rows),
        section("Canais de voz", voice_rows)
    ];

    let body = column![
        brand,
        divider(),
        scrollable(lists)
            .height(Length::Fill)
            .spacing(4)
            .style(theme::thin_scroll),
        footer(m, config),
    ];
    container(body)
        .width(config.sidebar_width)
        .height(Length::Fill)
        .style(theme::fill(BG2))
        .into()
}

fn divider<'a>() -> Element<'a, Msg> {
    container(space())
        .width(Length::Fill)
        .height(1)
        .style(theme::fill(BORDER))
        .into()
}

/// Faixa de arrasto entre a sidebar e o conteúdo, com a linha de 1 px da borda.
pub fn resize_handle<'a>() -> Element<'a, Msg> {
    let line = container(space())
        .width(1)
        .height(Length::Fill)
        .style(theme::fill(BORDER));
    mouse_area(container(line).width(5).height(Length::Fill))
        .on_press(Msg::DragStart)
        .interaction(mouse::Interaction::ResizingHorizontally)
        .into()
}

fn section<'a>(title: &'a str, rows: impl Iterator<Item = Element<'a, Msg>>) -> Element<'a, Msg> {
    let rows: Vec<_> = rows.collect();
    if rows.is_empty() {
        return space().into();
    }
    let label = container(
        text(title.to_uppercase())
            .size(11)
            .font(theme::BOLD)
            .color(FG3),
    )
    .padding(Padding {
        top: 14.0,
        right: 16.0,
        bottom: 5.0,
        left: 16.0,
    });
    column![label, Column::with_children(rows).spacing(4).padding(8)].into()
}

/// `go` é o slug e se a call está nele. Canal de texto não tem clique: fica pra Fase 7.
fn channel_row<'a>(
    glyph: &'static [u8],
    name: &'a str,
    go: Option<(String, bool)>,
) -> Element<'a, Msg> {
    let active = go.as_ref().is_some_and(|(_, active)| *active);
    let icon_color = if active {
        ACCENT2
    } else {
        Color { a: 0.55, ..FG2 }
    };
    let label = one_line(name, 14.4).font(theme::MEDIUM);
    let line = row![icon(glyph, 18.0, icon_color), label]
        .spacing(9)
        .align_y(Alignment::Center);
    match go {
        Some((slug, active)) => button(line)
            .padding([8, 9])
            .width(Length::Fill)
            .style(theme::nav_item(active))
            .on_press(Msg::JoinChannel(slug))
            .into(),
        None => container(line.height(Length::Shrink))
            .padding([8, 9])
            .width(Length::Fill)
            .style(|_| container::Style::default().color(FG2))
            .into(),
    }
}

/// Nome longo corta na borda em vez de quebrar linha ou empurrar os ícones.
fn one_line(content: &str, size: f32) -> text::Text<'_> {
    text(content)
        .size(size)
        .wrapping(Wrapping::None)
        .width(Length::Fill)
}

fn occupants(rows: Vec<Element<'_, Msg>>) -> Element<'_, Msg> {
    Column::with_children(rows)
        .spacing(1)
        .padding(Padding {
            top: 2.0,
            right: 6.0,
            bottom: 6.0,
            left: 26.0,
        })
        .into()
}

/// `badges`: mudo, câmera e transmissão. Vêm do polling ou da sala.
fn occupant<'a>(
    m: &'a Main,
    name: &'a str,
    badges: (bool, bool, bool),
    speaking: bool,
) -> Element<'a, Msg> {
    let (muted, camera, screen_share) = badges;
    let badges = row![]
        .push(screen_share.then(live_badge))
        .push(camera.then(|| icon(VIDEO, 14.0, FG2)))
        .push(muted.then(|| icon(MIC_OFF, 14.0, DANGER)))
        .spacing(4)
        .align_y(Alignment::Center);
    let line = row![
        avatar::ringed(avatar::view(name, m.avatar(name), 24.0), 24.0, speaking),
        container(one_line(name, 13.6).color(FG2))
            .width(Length::Fill)
            .clip(true),
        badges,
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    container(line).padding([0, 6]).width(Length::Fill).into()
}

fn footer<'a>(m: &'a Main, config: &'a Config) -> Element<'a, Msg> {
    let name = m.user.username.as_str();
    let offline = m.failures >= FALHAS_ATE_AVISAR;
    let dot_color = if offline { IDLE } else { GREEN };
    let dot = container(space())
        .width(10)
        .height(10)
        .style(move |_| container::Style {
            background: Some(dot_color.into()),
            border: border::rounded(5).color(BG3).width(2),
            ..Default::default()
        });
    let face = stack![
        avatar::view(name, m.avatar(name), 32.0),
        container(dot)
            .width(34)
            .height(34)
            .align_right(34)
            .align_bottom(34),
    ];
    let identity = button(
        row![
            face,
            column![
                container(one_line(name, 13.6).font(theme::SEMIBOLD).color(FG)).clip(true),
                text(if offline { "Sem conexão" } else { "Online" })
                    .size(11.5)
                    .color(FG2),
            ]
            .width(Length::Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding(0)
    .width(Length::Fill)
    .style(theme::ghost)
    .on_press(Msg::OpenSettings(Tab::Account));

    let actions = row![
        icon_button(
            if config.mic_muted { MIC_OFF } else { MIC },
            config.mic_muted,
            Msg::ToggleMic,
            if config.mic_muted {
                "Ligar o microfone"
            } else {
                "Desligar o microfone"
            },
        ),
        icon_button(
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
        ),
        icon_button(
            SETTINGS,
            false,
            Msg::OpenSettings(Tab::Server),
            "Configurações"
        ),
    ]
    .spacing(2);

    container(
        row![identity, actions]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([8, 10])
    .width(Length::Fill)
    .style(theme::fill(BG3))
    .into()
}

fn icon_button<'a>(glyph: &'static [u8], danger: bool, msg: Msg, tip: &'a str) -> Element<'a, Msg> {
    let color = if danger { DANGER } else { FG2 };
    let target = button(container(icon(glyph, 16.0, color)).center(32))
        .padding(0)
        .style(theme::ghost)
        .on_press(msg);
    tipped(target.into(), tip)
}

pub fn tipped<'a>(target: Element<'a, Msg>, tip: &'a str) -> Element<'a, Msg> {
    let bubble = container(text(tip).size(12).color(FG))
        .padding([4, 8])
        .style(theme::tooltip);
    tooltip(target, bubble, tooltip::Position::Top).into()
}
