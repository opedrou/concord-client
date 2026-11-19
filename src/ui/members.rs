//! Lista de membros na lateral direita: aberta com fotos, ou só o trilho do botão.
use iced::widget::{Column, button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Padding};

use super::sidebar::{USERS, icon, one_line, tipped};
use super::theme::{self, ACCENT2, BG2, BORDER, FG, FG2, FG3};
use super::{Main, Msg, avatar};

const WIDTH: f32 = 240.0;
const RAIL: f32 = 52.0;

pub fn view(m: &Main) -> Element<'_, Msg> {
    let toggle = button(
        container(icon(
            USERS,
            20.0,
            if m.members_open { ACCENT2 } else { FG2 },
        ))
        .center(34),
    )
    .padding(0)
    .style(theme::nav_item(m.members_open))
    .on_press(Msg::ToggleMembers);
    let toggle = tipped(
        toggle.into(),
        if m.members_open {
            "Esconder a lista de membros"
        } else {
            "Mostrar a lista de membros"
        },
    );
    let line = container(space())
        .width(1)
        .height(Length::Fill)
        .style(theme::fill(BORDER));

    if !m.members_open {
        let rail = container(toggle)
            .padding([15, 9])
            .width(RAIL)
            .height(Length::Fill)
            .style(theme::fill(BG2));
        return row![line, rail].into();
    }

    let (online, offline) = groups(m);
    let count = online.len() + offline.len();
    let header = container(
        row![
            text(format!("Membros — {count}"))
                .size(14)
                .font(theme::SEMIBOLD)
                .color(FG)
                .width(Length::Fill),
            toggle,
        ]
        .align_y(Alignment::Center),
    )
    .padding(Padding {
        top: 15.0,
        right: 9.0,
        bottom: 15.0,
        left: 16.0,
    })
    .width(Length::Fill);
    let divider = container(space())
        .width(Length::Fill)
        .height(1)
        .style(theme::fill(BORDER));
    let rows = [("Online", online, true), ("Offline", offline, false)]
        .into_iter()
        .filter(|(_, names, _)| !names.is_empty())
        .flat_map(|(title, names, on)| {
            let title = container(
                text(format!("{title} — {}", names.len()))
                    .size(11)
                    .font(theme::BOLD)
                    .color(FG3),
            )
            .padding([10, 8]);
            [title.into()]
                .into_iter()
                .chain(names.into_iter().map(move |name| member(m, name, on)))
        });
    let list = scrollable(Column::with_children(rows).spacing(2).padding(8))
        .height(Length::Fill)
        .spacing(4)
        .style(theme::thin_scroll);
    let panel = container(column![header, divider, list])
        .width(WIDTH)
        .height(Length::Fill)
        .style(theme::fill(BG2));
    row![line, panel].into()
}

/// Canal de voz de quem está nele.
fn voice_channel<'a>(m: &'a Main, name: &str) -> Option<&'a str> {
    let (slug, _) = m
        .presence
        .iter()
        .find(|(_, list)| list.iter().any(|p| p.display_name() == name))?;
    Some(
        m.channels
            .iter()
            .find(|c| c.slug == *slug)
            .map_or(slug.as_str(), |c| c.name.as_str()),
    )
}

/// Online e offline, cada um em ordem sem diferenciar maiúscula. Está online quem o servidor
/// lista, quem está em voz e o próprio usuário (que está com o app aberto).
pub fn groups(m: &Main) -> (Vec<&str>, Vec<&str>) {
    let mut names: Vec<&str> = m.members.iter().map(|p| p.username.as_str()).collect();
    if !names.contains(&m.user.username.as_str()) {
        names.push(&m.user.username);
    }
    names.sort_by_cached_key(|n| n.to_lowercase());
    names.into_iter().partition(|n| {
        *n == m.user.username || m.online.contains(*n) || voice_channel(m, n).is_some()
    })
}

/// Foto e nome; quem está num canal de voz leva o nome do canal embaixo.
fn member<'a>(m: &'a Main, name: &'a str, online: bool) -> Element<'a, Msg> {
    let color = if online { FG2 } else { FG3 };
    let label = column![container(one_line(name, 13.6).color(color)).clip(true)]
        .push(voice_channel(m, name).map(|c| container(one_line(c, 11.5).color(FG3)).clip(true)));
    let face = avatar::faded(name, m.avatar(name), 28.0, if online { 1.0 } else { 0.45 });
    let line = row![face, label.width(Length::Fill)]
        .spacing(10)
        .align_y(Alignment::Center);
    container(line).padding([5, 8]).width(Length::Fill).into()
}
