//! Lista de membros na lateral direita: aberta com fotos, ou só o trilho do botão. Clicar numa
//! pessoa abre um menu pequeno embaixo da linha: chamar pra call e mencionar no chat.
use iced::widget::{Column, button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Padding, Task, border};

use super::people::{self, State};
use super::sidebar::{USERS, icon, one_line, tipped};
use super::theme::{self, ACCENT2, BG2, BG3, BORDER, DANGER, FG, FG2, FG3, GREEN};
use super::voice::{self, ActiveCall};
use super::{Main, Msg as App, avatar};
use crate::api::Error;

const WIDTH: f32 = 240.0;
const RAIL: f32 = 52.0;

/// Menu aberto embaixo da linha de uma pessoa, e o andamento do chamado dela.
pub struct Menu {
    pub(super) name: String,
    pub(super) state: State,
}

impl Menu {
    /// Enquanto manda, e no aviso de sucesso, o botão de chamar fica parado.
    fn locked(&self) -> bool {
        matches!(self.state, State::Sending | State::Done(_))
    }
}

#[derive(Clone, Debug)]
pub enum Msg {
    /// Clique na linha: abre o menu dela, ou fecha se já era o dela.
    Open(String),
    Close,
    /// "Chamar pra call": o id de quem.
    Call(i64),
    /// Resposta do chamado, pelo nome de quem estava no menu: a de um menu que já fechou cai fora.
    Sent(String, Result<(u32, u32), Error>),
    /// Passou o tempo do aviso de sucesso.
    Finish(String),
    /// "Mencionar": põe `@nome ` no campo do chat aberto.
    Mention(String),
}

pub fn action(msg: &Msg) -> Option<String> {
    match msg {
        Msg::Call(_) => Some("chamou um membro pra call".into()),
        Msg::Mention(_) => Some("mencionou um membro".into()),
        _ => None,
    }
}

/// O que o menu de uma pessoa oferece. `call` é o id de quem chamar.
pub(super) struct Items {
    pub(super) call: Option<i64>,
    pub(super) mention: bool,
}

impl Items {
    fn any(&self) -> bool {
        self.call.is_some() || self.mention
    }
}

/// Chamar: só com call minha, pra quem tem conta e ainda não está nela. Mencionar: só com
/// um chat aberto. Em mim mesmo, nada.
pub(super) fn items(m: &Main, name: &str) -> Items {
    if name == m.user.username {
        return Items {
            call: None,
            mention: false,
        };
    }
    let inside = |c: &ActiveCall| c.live.iter().flatten().any(|l| voice::name_of(l) == name);
    let call = m
        .call
        .as_ref()
        .filter(|c| !inside(c))
        .and_then(|_| m.members.iter().find(|p| p.username == name))
        .map(|p| p.id);
    Items {
        call,
        mention: m.chat.is_some(),
    }
}

pub fn update(m: &mut Main, msg: Msg) -> Task<App> {
    match msg {
        Msg::Open(name) => {
            let same = m.member_menu.as_ref().is_some_and(|x| x.name == name);
            m.member_menu = (!same).then_some(Menu {
                name,
                state: State::Idle,
            });
            Task::none()
        }
        Msg::Close => {
            m.member_menu = None;
            Task::none()
        }
        Msg::Call(id) => {
            let Some(slug) = m.call.as_ref().map(|c| c.slug.clone()) else {
                return Task::none();
            };
            let Some(menu) = m.member_menu.as_mut().filter(|x| !x.locked()) else {
                return Task::none();
            };
            menu.state = State::Sending;
            let (client, name) = (m.client.clone(), menu.name.clone());
            Task::future(async move {
                let sent = client.call_people(&[id], &slug).await;
                App::Member(Msg::Sent(name, sent))
            })
        }
        Msg::Sent(name, result) => {
            let Some(menu) = m
                .member_menu
                .as_mut()
                .filter(|x| x.name == name && x.state == State::Sending)
            else {
                return Task::none();
            };
            menu.state = people::finished(result);
            if !matches!(menu.state, State::Done(_)) {
                return Task::none();
            }
            Task::future(async move {
                tokio::time::sleep(people::SUCCESS_FOR).await;
                App::Member(Msg::Finish(name))
            })
        }
        Msg::Finish(name) => {
            let done = m
                .member_menu
                .as_ref()
                .is_some_and(|x| x.name == name && matches!(x.state, State::Done(_)));
            if done {
                m.member_menu = None;
            }
            Task::none()
        }
        Msg::Mention(name) => {
            m.member_menu = None;
            match &mut m.chat {
                Some(chat) => chat.mention_member(&name),
                None => Task::none(),
            }
        }
    }
}

pub fn view(m: &Main) -> Element<'_, App> {
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
    .on_press(App::ToggleMembers);
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

/// Canal de voz de quem está nele: o slug e o nome.
fn voice_channel<'a>(m: &'a Main, name: &str) -> Option<(&'a str, &'a str)> {
    let (slug, _) = m
        .presence
        .iter()
        .find(|(_, list)| list.iter().any(|p| p.display_name() == name))?;
    let label = m
        .channels
        .iter()
        .find(|c| c.slug == *slug)
        .map_or(slug.as_str(), |c| c.name.as_str());
    Some((slug.as_str(), label))
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

/// Foto e nome; quem está num canal de voz leva o nome do canal embaixo, com um "Entrar" nele
/// (menos no canal em que eu já estou). Clicar na linha abre o menu, se ele tem o que oferecer.
fn member<'a>(m: &'a Main, name: &'a str, online: bool) -> Element<'a, App> {
    let color = if online { FG2 } else { FG3 };
    let channel = voice_channel(m, name).map(|(slug, label)| {
        let join = m.call.as_ref().is_none_or(|c| c.slug != slug).then(|| {
            button(text("Entrar").size(11.5).font(theme::MEDIUM))
                .padding([1, 8])
                .style(theme::nav_item(true))
                .on_press(App::JoinChannel(slug.to_owned()))
        });
        row![
            container(one_line(label, 11.5).color(FG3))
                .width(Length::Fill)
                .clip(true)
        ]
        .push(join)
        .spacing(6)
        .align_y(Alignment::Center)
    });
    let label = column![container(one_line(name, 13.6).color(color)).clip(true)].push(channel);
    let face = avatar::faded(name, m.avatar(name), 28.0, if online { 1.0 } else { 0.45 });
    let line = row![face, label.width(Length::Fill)]
        .spacing(10)
        .align_y(Alignment::Center);

    let offer = items(m, name);
    let open = m.member_menu.as_ref().filter(|x| x.name == name);
    let target = button(line)
        .padding([5, 8])
        .width(Length::Fill)
        .style(theme::nav_item(open.is_some()))
        .on_press_maybe(offer.any().then(|| App::Member(Msg::Open(name.to_owned()))));
    let menu = open
        .filter(|_| offer.any())
        .map(|open| menu_card(open, &offer, name));
    column![target].push(menu).into()
}

/// As ações, embaixo da linha da pessoa; o resultado do chamado aparece junto.
fn menu_card<'a>(menu: &'a Menu, offer: &Items, name: &'a str) -> Element<'a, App> {
    let item = |label: &'static str, msg: Option<App>| {
        button(text(label).size(13).font(theme::MEDIUM))
            .padding([6, 10])
            .width(Length::Fill)
            .style(theme::nav_item(false))
            .on_press_maybe(msg)
    };
    let status = match &menu.state {
        State::Idle => None,
        State::Sending => Some(("Chamando…", FG2)),
        State::Done(line) => Some((line.as_str(), GREEN)),
        State::Failed(line) => Some((line.as_str(), DANGER)),
    };
    let actions = column![]
        .push(offer.call.map(|id| {
            item(
                "Chamar pra call",
                (!menu.locked()).then_some(App::Member(Msg::Call(id))),
            )
        }))
        .push(offer.mention.then(|| {
            item(
                "Mencionar",
                Some(App::Member(Msg::Mention(name.to_owned()))),
            )
        }))
        .push(status.map(|(line, color)| {
            container(text(line).size(12).color(color))
                .padding([2, 10])
                .width(Length::Fill)
        }))
        .spacing(2);
    let card = container(actions)
        .padding(4)
        .width(Length::Fill)
        .style(|_| container::Style {
            background: Some(BG3.into()),
            border: border::rounded(10).color(BORDER).width(1),
            ..Default::default()
        });
    container(card)
        .padding(Padding {
            top: 2.0,
            right: 0.0,
            bottom: 4.0,
            left: 38.0,
        })
        .into()
}
