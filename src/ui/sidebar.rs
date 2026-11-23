//! Coluna de canais: marca, listas, quem está em cada canal e rodapé.
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, button, column, container, hover, mouse_area, row, scrollable, space, stack, svg, text,
    tooltip,
};
use iced::{Alignment, Color, Element, Length, Padding, border, mouse};

use super::theme::{self, ACCENT, ACCENT2, BG2, BG3, BORDER, DANGER, FG, FG2, FG3, GREEN, IDLE};
use super::voice::{ActiveCall, Status};
use super::{FALHAS_ATE_AVISAR, Main, Msg, Tab, avatar, chat, stage, voice};
use crate::api::{self, Channel, ChannelKind};
use crate::config::Config;

macro_rules! icon {
    ($name:literal) => {
        include_bytes!(concat!("../../assets/icons/", $name, ".svg")).as_slice()
    };
}

pub const HASH: &[u8] = icon!("hash");
const SPEAKER: &[u8] = icon!("speaker");
pub const MIC: &[u8] = icon!("mic");
pub const MIC_OFF: &[u8] = icon!("mic-off");
pub const HEADPHONES: &[u8] = icon!("headphones");
pub const HEADPHONES_OFF: &[u8] = icon!("headphones-off");
pub const VOLUME: &[u8] = icon!("volume");
pub const VOLUME_X: &[u8] = icon!("volume-x");
const SETTINGS: &[u8] = icon!("settings");
pub const PHONE_OFF: &[u8] = icon!("phone-off");
const VIDEO: &[u8] = icon!("video");
pub const SCREEN: &[u8] = icon!("screen-share");
pub const CLOSE: &[u8] = icon!("close");
pub const CHEVRON_UP: &[u8] = icon!("chevron-up");
pub const MARK: &[u8] = icon!("mark");
pub const DOWNLOAD: &[u8] = icon!("download");
pub const USER_PLUS: &[u8] = icon!("user-plus");
pub const USERS: &[u8] = icon!("users");

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
    // Versão nova já instalada: seta verde discreta, que abre a nova fora de call.
    let update = m.update.as_deref().map(|version| {
        let tip = if m.call.is_some() {
            format!("Versão {version} baixada. Clique pra reiniciar agora: tu sai da call.")
        } else {
            format!("Versão {version} baixada. Clique pra reiniciar agora.")
        };
        let arrow = button(container(icon(DOWNLOAD, 18.0, GREEN)).center(28))
            .padding(0)
            .style(theme::ghost)
            .on_press(Msg::RestartForUpdate);
        tooltip(
            arrow,
            container(text(tip).size(12.5))
                .padding([6, 10])
                .style(theme::tooltip),
            tooltip::Position::Bottom,
        )
    });
    let brand = container(
        row![mark(34.0), text("Concord").size(17).font(theme::DISPLAY)]
            .push(space().width(Length::Fill))
            .push(update)
            .spacing(11)
            .align_y(Alignment::Center),
    )
    .padding([15, 16])
    .width(Length::Fill);

    let of_kind = |kind| m.channels.iter().filter(move |c: &&Channel| c.kind == kind);
    let text_rows = of_kind(ChannelKind::Text).map(|c| {
        let open = m.chat.as_ref().is_some_and(|chat| chat.channel == c.id);
        let go = Some((Msg::Chat(c.id, chat::Msg::Open), open));
        let unread = m.mentions.get(&c.id).copied().unwrap_or(0);
        channel_row(HASH, &c.name, go, unread, config.sidebar_width)
    });
    let voice_rows = of_kind(ChannelKind::Voice).map(|c| {
        let active = m.call.as_ref().filter(|call| call.slug == c.slug);
        let go = Some((Msg::JoinChannel(c.slug.clone()), active.is_some()));
        let people = match active.and_then(|call| call.live.as_ref()) {
            // Na call ativa a lista vem da sala, não do polling.
            Some(live) => live
                .iter()
                .map(|l| {
                    let name = voice::name_of(l);
                    occupant(
                        (m, config),
                        name,
                        (l.muted, l.camera, l.screen_share),
                        active.is_some_and(|call| call.speaking(name)),
                        !l.is_local,
                    )
                })
                .collect(),
            None => api::occupants(m.presence.get(&c.slug).map_or(&[][..], Vec::as_slice))
                .into_iter()
                .map(|p| {
                    occupant(
                        (m, config),
                        p.display_name(),
                        (p.muted, p.camera, p.screen_share),
                        false,
                        false,
                    )
                })
                .collect(),
        };
        column![
            channel_row(SPEAKER, &c.name, go, 0, config.sidebar_width),
            occupants(people)
        ]
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

/// `go` é o clique e se o canal está aberto: o chat, ou a call nele. `unread`: menções
/// ainda não vistas, na pílula roxa à direita. `width`: a da sidebar, pra saber se o nome
/// cortou e merece o tooltip.
fn channel_row<'a>(
    glyph: &'static [u8],
    name: &'a str,
    go: Option<(Msg, bool)>,
    unread: u32,
    width: f32,
) -> Element<'a, Msg> {
    let active = go.as_ref().is_some_and(|(_, active)| *active);
    let icon_color = if active {
        ACCENT2
    } else {
        Color { a: 0.55, ..FG2 }
    };
    let label = one_line(name, 14.4).font(theme::MEDIUM);
    let line = row![icon(glyph, 18.0, icon_color), label]
        .push((unread > 0).then(|| mention_pill(unread)))
        .spacing(9)
        .align_y(Alignment::Center);
    let row: Element<'_, Msg> = match go {
        Some((msg, active)) => button(line)
            .padding([8, 9])
            .width(Length::Fill)
            .style(theme::nav_item(active))
            .on_press(msg)
            .into(),
        None => container(line.height(Length::Shrink))
            .padding([8, 9])
            .width(Length::Fill)
            .style(|_| container::Style::default().color(FG2))
            .into(),
    };
    // Seção (8 + 8), botão (9 + 9), ícone (18) e o espaço até o nome (9), mais a pílula.
    let pill = if unread > 0 { 36.0 } else { 0.0 };
    if cut(name, 14.4, width - 61.0 - pill) {
        tipped(row, name)
    } else {
        row
    }
}

/// Nome mais largo que `room` px? O iced não conta se cortou, então estima pela largura
/// média do glifo (~0,52 do tamanho da fonte).
// ponytail: estimativa; passa a medir o texto de verdade se errar muito.
fn cut(name: &str, size: f32, room: f32) -> bool {
    name.chars().count() as f32 * size * 0.52 > room
}

/// Menções não vistas num canal de texto.
fn mention_pill<'a>(count: u32) -> Element<'a, Msg> {
    let label = if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    };
    container(text(label).size(11).font(theme::BOLD).color(FG))
        .padding([1, 6])
        .style(|_| container::Style {
            background: Some(ACCENT.into()),
            border: border::rounded(9),
            ..Default::default()
        })
        .into()
}

/// Nome longo corta na borda em vez de quebrar linha ou empurrar os ícones.
pub fn one_line(content: &str, size: f32) -> text::Text<'_> {
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

/// `badges`: mudo, câmera e transmissão. Vêm do polling ou da sala. `mutable`: outra pessoa
/// da call em que estou, que ganha o botão de mudo no hover.
fn occupant<'a>(
    (m, config): (&'a Main, &'a Config),
    name: &'a str,
    badges: (bool, bool, bool),
    speaking: bool,
    mutable: bool,
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
    let line = container(line).padding([0, 6]).width(Length::Fill);
    let line: Element<'_, Msg> = if mutable {
        // Mesmo botão do tile: aparece no hover e fica fixo enquanto a pessoa está mutada.
        let silenced = config.volume_of(name) == 0;
        let corner =
            container(
                container(stage::mute_button(name, silenced)).style(|_| container::Style {
                    background: Some(BG3.into()),
                    border: border::rounded(9),
                    ..Default::default()
                }),
            )
            .align_right(Length::Fill)
            .center_y(Length::Fill);
        if silenced {
            stack![line, corner].into()
        } else {
            hover(line, corner)
        }
    } else {
        line.into()
    };
    // Largura do nome: sidebar menos seção e coluna (16 + 32), padding (12), avatar (24),
    // os dois espaços (16) e os selos.
    let selos = [screen_share, camera, muted].map(|b| if b { 1.0 } else { 0.0 });
    let room = config.sidebar_width - 100.0 - selos[0] * 38.0 - (selos[1] + selos[2]) * 18.0;
    if cut(name, 13.6, room) {
        tipped(line, name)
    } else {
        line
    }
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
                "Ligar o microfone (Ctrl+Shift+M)"
            } else {
                "Desligar o microfone (Ctrl+Shift+M)"
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
                "Voltar a ouvir (Ctrl+Shift+D)"
            } else {
                "Parar de ouvir (Ctrl+Shift+D)"
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

    let user = container(
        row![identity, actions]
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .padding([8, 10])
    .width(Length::Fill);
    let body = match &m.call {
        Some(call) => column![call_block(m, call, config.sidebar_width), divider(), user],
        None => column![user],
    };
    container(body)
        .width(Length::Fill)
        .style(theme::fill(BG3))
        .into()
}

/// "Em call", acima da linha do usuário: estado, tempo, canal e sair. Com o chat aberto a
/// barra da call some, e este é o jeito de sair sem voltar ao palco.
fn call_block<'a>(m: &'a Main, call: &'a ActiveCall, width: f32) -> Element<'a, Msg> {
    let (state, color) = match call.status {
        Status::Connected => ("Conectado", GREEN),
        Status::Connecting => ("Conectando…", IDLE),
        Status::Reconnecting(_) => ("Reconectando…", IDLE),
    };
    let channel = m
        .channels
        .iter()
        .find(|c| c.slug == call.slug)
        .map_or(call.slug.as_str(), |c| c.name.as_str());
    let status = row![
        text(state).size(12).font(theme::SEMIBOLD).color(color),
        space().width(Length::Fill),
        text(clock(call.since.elapsed().as_secs()))
            .size(12)
            .color(FG2),
    ]
    .spacing(6);
    let name = row![
        icon(SPEAKER, 14.0, FG2),
        one_line(channel, 13.6).font(theme::MEDIUM).color(FG)
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    let info = button(column![status, name].spacing(2))
        .padding([4, 8])
        .width(Length::Fill)
        .style(theme::ghost)
        .on_press(Msg::JoinChannel(call.slug.clone()));
    // Botão (8 + 8), seção do bloco (10 + 10), o botão de sair (32 + 4), ícone (14) e espaço (6).
    let info: Element<'_, Msg> = if cut(channel, 13.6, width - 92.0) {
        tipped(info.into(), channel)
    } else {
        info.into()
    };
    let leave = button(container(icon(PHONE_OFF, 16.0, DANGER)).center(32))
        .padding(0)
        .style(theme::ghost)
        .on_press(Msg::Leave);
    container(
        row![info, tipped(leave.into(), "Sair da call")]
            .spacing(4)
            .align_y(Alignment::Center),
    )
    .padding([6, 10])
    .width(Length::Fill)
    .into()
}

/// Tempo de call: `mm:ss`, e `h:mm:ss` passando de 1 h.
fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn icon_button<'a>(glyph: &'static [u8], danger: bool, msg: Msg, tip: &'a str) -> Element<'a, Msg> {
    let color = if danger { DANGER } else { FG2 };
    let target = button(container(icon(glyph, 16.0, color)).center(32))
        .padding(0)
        .style(theme::ghost)
        .on_press(msg);
    tipped(target.into(), tip)
}

pub fn tipped<'a>(
    target: Element<'a, Msg>,
    tip: impl iced::widget::text::IntoFragment<'a>,
) -> Element<'a, Msg> {
    let bubble = container(text(tip).size(12).color(FG))
        .padding([4, 8])
        .style(theme::tooltip);
    tooltip(target, bubble, tooltip::Position::Top).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_de_call() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(65), "01:05");
        assert_eq!(clock(3599), "59:59");
        assert_eq!(clock(3600), "1:00:00");
        assert_eq!(clock(3 * 3600 + 7 * 60 + 9), "3:07:09");
    }

    #[test]
    fn nome_cortado_pede_tooltip() {
        assert!(cut(
            "Bruno com nome bem comprido demais pra caber",
            13.6,
            150.0
        ));
        assert!(!cut("Ana", 13.6, 150.0));
        assert!(!cut("geral", 14.4, 200.0 - 61.0));
    }
}
