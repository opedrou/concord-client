//! Telas fora da sidebar: endereço, login, offline, home e configurações.
use iced::widget::{
    Column, button, center, column, container, mouse_area, opaque, row, space, stack, text,
    text_input,
};
use iced::{Alignment, Element, Length, Theme};

use super::sidebar::{CLOSE, icon, mark};
use super::theme::{self, BG, DANGER, FG, FG2, IDLE};
use super::{ID_SERVIDOR, ID_USUARIO, LoginForm, Main, Msg, ServerForm, Tab, avatar};
use crate::api::Secret;

const COLUNA: f32 = 400.0;

fn page<'a>(content: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    container(center(content)).style(theme::fill(BG)).into()
}

fn branded<'a>(card: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    page(
        column![
            mark(76.0),
            text("Concord").size(34).font(theme::DISPLAY_HEAVY),
            space().height(22),
            container(card).padding(26).width(COLUNA).style(theme::card),
        ]
        .align_x(Alignment::Center)
        .spacing(8)
        .width(COLUNA),
    )
}

fn input<'a>(
    placeholder: &str,
    value: &str,
    style_id: &'static str,
) -> text_input::TextInput<'a, Msg> {
    text_input(placeholder, value)
        .id(style_id)
        .size(15.2)
        .padding([11, 13])
        .style(theme::field)
}

fn labeled<'a>(label: &'a str, field: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    column![text(label).size(14).color(FG2), field.into()]
        .spacing(6)
        .into()
}

fn primary<'a>(label: &'a str, msg: Option<Msg>) -> Element<'a, Msg> {
    button(
        text(label)
            .size(15.5)
            .font(theme::BOLD)
            .center()
            .width(Length::Fill),
    )
    .padding(13)
    .width(Length::Fill)
    .style(theme::primary)
    .on_press_maybe(msg)
    .into()
}

fn link<'a>(label: &'a str, msg: Option<Msg>) -> Element<'a, Msg> {
    button(text(label).size(14).center().width(Length::Fill))
        .padding([7, 12])
        .width(Length::Fill)
        .style(theme::ghost)
        .on_press_maybe(msg)
        .into()
}

fn error_line<'a>(error: Option<&'a str>) -> Option<Element<'a, Msg>> {
    error.map(|e| text(e).size(14).color(DANGER).into())
}

pub fn connecting<'a>() -> Element<'a, Msg> {
    page(
        column![mark(76.0), text("Conectando…").size(15).color(FG2)]
            .spacing(8)
            .align_x(Alignment::Center),
    )
}

pub fn server(f: &ServerForm) -> Element<'_, Msg> {
    let mut field = input("call.exemplo.com", &f.input, ID_SERVIDOR);
    if !f.busy {
        field = field
            .on_input(Msg::ServerInput)
            .on_submit(Msg::ServerSubmit);
    }
    branded(
        column![labeled("Endereço do servidor", field)]
            .push(error_line(f.error.as_deref()))
            .push(primary(
                if f.busy { "Conectando…" } else { "Continuar" },
                (!f.busy).then_some(Msg::ServerSubmit),
            ))
            .spacing(16),
    )
}

pub fn login(f: &LoginForm) -> Element<'_, Msg> {
    let mut username = input("", &f.username, ID_USUARIO);
    let mut password = input("", &f.password.0, "campo-senha").secure(true);
    if !f.busy {
        username = username.on_input(Msg::Username).on_submit(Msg::LoginSubmit);
        password = password
            .on_input(|s| Msg::Password(Secret(s)))
            .on_submit(Msg::LoginSubmit);
    }
    branded(
        column![labeled("Usuário", username), labeled("Senha", password)]
            .push(error_line(f.error.as_deref()))
            .push(primary(
                if f.busy { "Entrando…" } else { "Entrar" },
                (!f.busy).then_some(Msg::LoginSubmit),
            ))
            .push(
                text(&f.server)
                    .size(12.5)
                    .color(FG2)
                    .center()
                    .width(Length::Fill),
            )
            .push(link(
                "Trocar servidor",
                (!f.busy).then_some(Msg::ChangeServer),
            ))
            .spacing(16),
    )
}

pub fn offline(server: &str) -> Element<'_, Msg> {
    branded(
        column![
            text("Sem conexão").size(20).font(theme::SEMIBOLD),
            text(format!("Não foi possível falar com {server}."))
                .size(14)
                .color(FG2),
            primary("Tentar de novo", Some(Msg::Retry)),
            link("Trocar servidor", Some(Msg::ChangeServer)),
        ]
        .spacing(14),
    )
}

pub fn home(username: &str) -> Element<'_, Msg> {
    page(
        column![
            mark(92.0),
            space().height(8),
            text(format!("Bem-vindo de volta, {username}."))
                .size(32)
                .font(theme::DISPLAY_HEAVY)
                .center(),
            text("Escolha um canal pra começar.").size(16).color(FG2),
        ]
        .spacing(12)
        .padding(40)
        .align_x(Alignment::Center),
    )
}

pub fn settings(m: &Main, tab: Tab) -> Element<'_, Msg> {
    let item = |label, target| {
        button(text(label).size(14.4).font(theme::MEDIUM))
            .padding([8, 12])
            .width(Length::Fill)
            .style(theme::nav_item(tab == target))
            .on_press(Msg::OpenSettings(target))
    };
    let nav = container(
        column![
            item("Servidor", Tab::Server),
            item("Conta", Tab::Account),
            item("Sobre", Tab::About)
        ]
        .spacing(4),
    )
    .padding(14)
    .width(170)
    .height(Length::Fill);

    let panel = match tab {
        Tab::Server => server_panel(m),
        Tab::Account => account_panel(m),
        Tab::About => about_panel(),
    };
    let close = button(container(icon(CLOSE, 16.0, FG2)).center(32))
        .padding(0)
        .style(theme::ghost)
        .on_press(Msg::CloseSettings);
    let body = column![
        row![space::horizontal(), close],
        container(panel).padding([0, 14])
    ]
    .padding(8);

    let window = container(row![
        nav,
        container(space())
            .width(1)
            .height(Length::Fill)
            .style(theme::fill(theme::BORDER)),
        body
    ])
    .width(640)
    .height(420)
    .style(theme::card);

    stack![
        opaque(
            mouse_area(
                container(space())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .style(theme::backdrop)
            )
            .on_press(Msg::CloseSettings)
        ),
        center(opaque(window)),
    ]
    .into()
}

fn title<'a>(content: &'a str) -> Element<'a, Msg> {
    text(content).size(20).font(theme::DISPLAY).into()
}

fn action<'a>(
    label: &'a str,
    msg: Msg,
    style: fn(&Theme, button::Status) -> button::Style,
) -> Element<'a, Msg> {
    button(text(label).size(14).font(theme::SEMIBOLD))
        .padding([9, 16])
        .style(style)
        .on_press(msg)
        .into()
}

fn server_panel(m: &Main) -> Element<'_, Msg> {
    column![
        title("Servidor"),
        labeled("Endereço", text(&m.server).size(15).color(FG)),
        action("Trocar servidor", Msg::ChangeServer, theme::outline),
    ]
    .spacing(16)
    .into()
}

fn account_panel(m: &Main) -> Element<'_, Msg> {
    let name = m.user.username.as_str();
    let identity = column![text(name).size(16).font(theme::SEMIBOLD)].push(
        m.user
            .is_admin
            .then(|| text("Administrador").size(13).color(FG2)),
    );
    let vault_warning = (!m.vault_ok).then(|| {
        text("O cofre do sistema não está disponível. Você vai precisar entrar de novo ao reabrir.")
            .size(13)
            .color(IDLE)
    });
    Column::new()
        .push(title("Conta"))
        .push(
            row![avatar::view(name, m.avatar(name), 48.0), identity]
                .spacing(12)
                .align_y(Alignment::Center),
        )
        .push(vault_warning)
        .push(action("Sair", Msg::Logout, theme::danger))
        .spacing(16)
        .into()
}

fn about_panel<'a>() -> Element<'a, Msg> {
    column![
        title("Sobre"),
        text(concat!("Concord ", env!("CARGO_PKG_VERSION"))).size(15),
        text("Cliente desktop nativo, em Rust.").size(14).color(FG2),
    ]
    .spacing(12)
    .into()
}
