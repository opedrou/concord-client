//! "Chamar pessoas": painel sobre o palco com os membros e uma caixa por pessoa. Quem tem
//! o Concord aberto recebe o aviso da call; o servidor limita quantas chamadas saem.
use std::time::Duration;

use iced::widget::text::Wrapping;
use iced::widget::{button, checkbox, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Task};

use super::sidebar::{CLOSE, icon};
use super::stage::floating;
use super::theme::{self, BORDER, DANGER, FG, FG2, FG3, GREEN, IDLE};
use super::voice::ActiveCall;
use super::{Main, Msg as App, avatar};
use crate::api::{Error, Member};

/// Quantas pessoas cabem num chamado, como no servidor.
pub const MAX: usize = 10;
/// Quanto o aviso de sucesso fica na tela antes de o painel fechar.
const SUCCESS_FOR: Duration = Duration::from_millis(1200);
const WIDTH: f32 = 340.0;
const FACE: f32 = 28.0;
/// Altura de uma linha da lista (rosto, margem e espaço entre elas) e da lista toda.
const ROW: f32 = 42.0;
const LIST: f32 = 252.0;

#[derive(Clone, Debug)]
pub enum Msg {
    /// Botão da barra: abre o painel ou fecha, se já está aberto.
    Open,
    Pick(i64),
    Send,
    /// Resposta do servidor, pela id da call que pediu: a de uma call que já acabou cai fora.
    Sent(u64, Result<(u32, u32), Error>),
    /// Passou o tempo do aviso de sucesso.
    Finish(u64),
}

#[derive(Clone, Debug, Default, PartialEq)]
enum State {
    #[default]
    Idle,
    Sending,
    Done(String),
    Failed(String),
}

#[derive(Debug, Default)]
pub struct Panel {
    /// Em ordem de clique.
    picked: Vec<i64>,
    state: State,
}

impl Panel {
    /// Marca ou desmarca. Com o limite cheio, só dá pra desmarcar.
    fn toggle(&mut self, id: i64) {
        if let Some(at) = self.picked.iter().position(|p| *p == id) {
            self.picked.remove(at);
        } else if self.picked.len() < MAX {
            self.picked.push(id);
        }
    }

    fn full(&self) -> bool {
        self.picked.len() >= MAX
    }

    /// Enquanto manda, e no aviso de sucesso, a lista fica parada.
    fn locked(&self) -> bool {
        matches!(self.state, State::Sending | State::Done(_))
    }
}

/// Minutos até liberar, arredondando pra cima: 61 s vira 2 min, nunca "0 min".
pub fn retry_minutes(secs: u64) -> u64 {
    secs.div_ceil(60).max(1)
}

fn finished(result: Result<(u32, u32), Error>) -> State {
    match result {
        Ok(_) => State::Done("Chamado enviado".to_owned()),
        Err(Error::TooManyAttempts(secs)) => State::Failed(format!(
            "Muitos chamados. Tenta de novo em {} min",
            retry_minutes(secs)
        )),
        Err(_) => State::Failed("Não foi possível chamar agora. Tenta de novo.".to_owned()),
    }
}

pub fn action(msg: &Msg) -> Option<String> {
    match msg {
        Msg::Open => Some("clicou em chamar pessoas".into()),
        Msg::Send => Some("mandou o chamado".into()),
        _ => None,
    }
}

pub fn update(m: &mut Main, msg: Msg) -> Task<App> {
    let Some(call) = m.call.as_mut() else {
        return Task::none();
    };
    match msg {
        Msg::Open => {
            if call.people.take().is_some() {
                return Task::none();
            }
            // Um painel por vez.
            (call.volume_panel, call.share_panel, call.share_menu) = (None, false, false);
            call.people = Some(Panel::default());
            // Quem não está na call ainda não tem a foto baixada.
            m.fetch_avatars()
        }
        Msg::Pick(id) => {
            if let Some(panel) = call.people.as_mut().filter(|p| !p.locked()) {
                panel.state = State::Idle;
                panel.toggle(id);
            }
            Task::none()
        }
        Msg::Send => {
            let (id, slug) = (call.id, call.slug.clone());
            let Some(panel) = call.people.as_mut().filter(|p| !p.locked()) else {
                return Task::none();
            };
            // Quem saiu da lista no meio não vai.
            let ids: Vec<i64> = panel
                .picked
                .iter()
                .copied()
                .filter(|p| m.members.iter().any(|member| member.id == *p))
                .collect();
            if ids.is_empty() {
                return Task::none();
            }
            panel.state = State::Sending;
            let client = m.client.clone();
            Task::future(async move {
                let sent = client.call_people(&ids, &slug).await;
                App::People(Msg::Sent(id, sent))
            })
        }
        Msg::Sent(id, result) => {
            let same = call.id == id;
            let Some(panel) = call
                .people
                .as_mut()
                .filter(|p| same && p.state == State::Sending)
            else {
                return Task::none();
            };
            panel.state = finished(result);
            if !matches!(panel.state, State::Done(_)) {
                return Task::none();
            }
            Task::future(async move {
                tokio::time::sleep(SUCCESS_FOR).await;
                App::People(Msg::Finish(id))
            })
        }
        Msg::Finish(id) => {
            if call.id == id && call.people.as_ref().is_some_and(|p| p.locked()) {
                call.people = None;
            }
            Task::none()
        }
    }
}

/// Painel aberto, ou nada.
pub fn card<'a>(m: &'a Main, call: &'a ActiveCall) -> Option<Element<'a, App>> {
    let panel = call.people.as_ref()?;
    let others: Vec<&Member> = m
        .members
        .iter()
        .filter(|p| p.username != m.user.username)
        .collect();

    let close = button(container(icon(CLOSE, 16.0, FG2)).center(28))
        .padding(0)
        .style(theme::ghost)
        .on_press(App::CloseVolume);
    let head = row![
        text("Chamar pessoas")
            .size(14.4)
            .font(theme::SEMIBOLD)
            .width(Length::Fill),
        close
    ]
    .align_y(Alignment::Center);
    let hint = text("Quem estiver com o Concord aberto recebe um aviso pra entrar nesta call.")
        .size(12.5)
        .color(FG2);

    let list: Element<'a, App> = if others.is_empty() {
        text("Não tem mais ninguém na lista.")
            .size(13.5)
            .color(FG2)
            .into()
    } else {
        let height = (others.len() as f32 * ROW).min(LIST);
        let rows = others.iter().map(|p| member_row(m, panel, p));
        scrollable(column(rows).spacing(2))
            .height(height)
            .style(theme::thin_scroll)
            .into()
    };

    let status: Element<'a, App> = match &panel.state {
        State::Done(line) => text(line).size(13).color(GREEN).width(Length::Fill).into(),
        State::Failed(line) => text(line).size(13).color(DANGER).width(Length::Fill).into(),
        _ if panel.full() => text(format!("Limite de {MAX} pessoas"))
            .size(13)
            .color(IDLE)
            .width(Length::Fill)
            .into(),
        _ if panel.picked.is_empty() => space().width(Length::Fill).into(),
        _ => text(format!("{} de {MAX}", panel.picked.len()))
            .size(13)
            .color(FG2)
            .width(Length::Fill)
            .into(),
    };
    let sending = panel.state == State::Sending;
    let ready = !panel.picked.is_empty() && !panel.locked();
    let send = button(
        text(if sending { "Chamando…" } else { "Chamar" })
            .size(14.5)
            .font(theme::BOLD)
            .center(),
    )
    .padding([9, 20])
    .style(theme::primary)
    .on_press_maybe(ready.then_some(App::People(Msg::Send)));
    let foot = row![status, send].spacing(12).align_y(Alignment::Center);

    Some(
        container(column![head, hint, list, foot].spacing(10))
            .width(WIDTH)
            .padding(14)
            .style(floating(BORDER))
            .into(),
    )
}

fn member_row<'a>(m: &'a Main, panel: &Panel, p: &'a Member) -> Element<'a, App> {
    let on = panel.picked.contains(&p.id);
    let enabled = !panel.locked() && (on || !panel.full());
    let id = p.id;
    let name = text(&p.username)
        .size(14)
        .font(theme::MEDIUM)
        .color(if enabled || on { FG } else { FG3 })
        .wrapping(Wrapping::None);
    let line = row![
        avatar::view(&p.username, m.avatar(&p.username), FACE),
        container(name).width(Length::Fill).clip(true),
        checkbox(on)
            .size(18)
            .on_toggle_maybe(enabled.then_some(move |_| App::People(Msg::Pick(id)))),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    button(line)
        .padding([6, 10])
        .width(Length::Fill)
        .style(theme::nav_item(false))
        .on_press_maybe(enabled.then_some(App::People(Msg::Pick(id))))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dez_marcados_e_o_resto_so_desmarca() {
        let mut panel = Panel::default();
        for id in 0..15 {
            panel.toggle(id);
        }
        assert_eq!(panel.picked.len(), MAX);
        assert!(panel.full());
        assert_eq!(panel.picked, (0..10).collect::<Vec<_>>(), "os 10 primeiros");
        panel.toggle(3);
        assert!(!panel.full());
        panel.toggle(12);
        assert_eq!(panel.picked.last(), Some(&12));
        assert!(!panel.picked.contains(&3));
    }

    #[test]
    fn espera_arredonda_pra_cima_e_nunca_zera() {
        assert_eq!(retry_minutes(0), 1);
        assert_eq!(retry_minutes(1), 1);
        assert_eq!(retry_minutes(60), 1);
        assert_eq!(retry_minutes(61), 2);
        assert_eq!(retry_minutes(1800), 30);
        assert_eq!(retry_minutes(1801), 31);
    }

    #[test]
    fn resposta_vira_aviso() {
        assert_eq!(finished(Ok((3, 2))), State::Done("Chamado enviado".into()));
        assert_eq!(
            finished(Err(Error::TooManyAttempts(90))),
            State::Failed("Muitos chamados. Tenta de novo em 2 min".into())
        );
        assert!(matches!(finished(Err(Error::Offline)), State::Failed(_)));
        assert!(matches!(
            finished(Err(Error::Server(400))),
            State::Failed(_)
        ));
    }
}
