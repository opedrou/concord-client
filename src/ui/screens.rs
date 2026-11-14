//! Telas fora da sidebar: endereço, login, offline, home e configurações.
use iced::widget::{
    Column, Row, button, center, column, container, mouse_area, opaque, pick_list, row, scrollable,
    slider, space, stack, text, text_input, toggler,
};
use iced::{Alignment, Color, Element, Length, Theme};

use super::sidebar::{CLOSE, icon, mark};
use super::theme::{self, BG, DANGER, FG, FG2, FG3, GREEN, IDLE};
use super::voice::Probe;
use super::{ID_SERVIDOR, ID_USUARIO, LoginForm, Main, Msg, ServerForm, Tab, avatar};
use crate::api::Secret;
use crate::audio::{MANUAL_MAX_DB, MANUAL_MIN_DB, NoiseLevel, Sensitivity};
use crate::config::Config;
use crate::update::Check;

/// Primeira opção das listas de dispositivo: `None` na config.
const PADRAO: &str = "Padrão do sistema";

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

pub fn home<'a>(username: &'a str, notice: Option<&'a str>) -> Element<'a, Msg> {
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
        .push(notice.map(|n| text(n).size(14).color(IDLE)))
        .spacing(12)
        .padding(40)
        .align_x(Alignment::Center),
    )
}

pub fn settings<'a>(m: &'a Main, config: &'a Config, tab: Tab) -> Element<'a, Msg> {
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
            item("Voz", Tab::Voice),
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
        Tab::Voice => voice_panel(m, config),
        Tab::About => about_panel(m),
    };
    let close = button(container(icon(CLOSE, 16.0, FG2)).center(32))
        .padding(0)
        .style(theme::ghost)
        .on_press(Msg::CloseSettings);
    let body = column![
        row![space::horizontal(), close],
        // Rola quando a janela é pequena demais pro painel.
        scrollable(container(panel).padding([0, 14]))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::thin_scroll)
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
    .width(680)
    .height(520)
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
    msg: impl Into<Option<Msg>>,
    style: fn(&Theme, button::Status) -> button::Style,
) -> Element<'a, Msg> {
    button(text(label).size(14).font(theme::SEMIBOLD))
        .padding([9, 16])
        .style(style)
        .on_press_maybe(msg.into())
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

/// Opções de um dispositivo: "Padrão do sistema" e depois a lista do sistema.
fn device_field<'a>(
    label: &'a str,
    names: &[String],
    selected: &Option<String>,
    msg: fn(Option<String>) -> Msg,
) -> Element<'a, Msg> {
    let options: Vec<String> = [PADRAO.to_owned()]
        .into_iter()
        .chain(names.iter().cloned())
        .collect();
    let current = selected.clone().unwrap_or_else(|| PADRAO.to_owned());
    let list = pick_list(options, Some(current), move |name| {
        msg((name != PADRAO).then_some(name))
    })
    .width(Length::Fill)
    .padding([9, 12])
    .text_size(14.4)
    .style(theme::pick)
    .menu_style(theme::pick_menu);
    labeled(label, list)
}

#[derive(Clone, Copy, PartialEq)]
struct Noise(NoiseLevel);

impl std::fmt::Display for Noise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self.0 {
            NoiseLevel::Off => "Desligada",
            NoiseLevel::Apm => "Navegador",
            NoiseLevel::Rnnoise => "RNNoise",
        })
    }
}

fn voice_panel<'a>(m: &'a Main, config: &'a Config) -> Element<'a, Msg> {
    let devices = m.devices.clone().unwrap_or_default();
    let noise = pick_list(
        [NoiseLevel::Off, NoiseLevel::Apm, NoiseLevel::Rnnoise].map(Noise),
        Some(Noise(config.noise)),
        |n| Msg::Noise(n.0),
    )
    .width(Length::Fill)
    .padding([9, 12])
    .text_size(14.4)
    .style(theme::pick)
    .menu_style(theme::pick_menu);
    let master = slider(0..=100u8, config.master_volume, Msg::Master)
        .on_release(Msg::VolumeDone)
        .width(Length::Fill);
    column![
        title("Voz"),
        device_field("Microfone", &devices.inputs, &config.input, Msg::Input),
        device_field(
            "Saída de áudio",
            &devices.outputs,
            &config.output,
            Msg::Output
        ),
        labeled("Redução de ruído", noise),
        sensitivity(m, config),
        labeled("Volume geral", master),
    ]
    .spacing(14)
    .into()
}

/// Altura da faixa do medidor com o slider do limiar por cima.
const FAIXA: f32 = 22.0;
const BARRA: f32 = 8.0;
/// Resolução das fatias do medidor.
const FATIAS: f32 = 1000.0;

/// Posição de um nível na faixa, de 0 a 1.
fn fraction(db: f32) -> f32 {
    ((db - MANUAL_MIN_DB) / (MANUAL_MAX_DB - MANUAL_MIN_DB)).clamp(0.0, 1.0)
}

/// Barra horizontal: cada trecho em sua cor, o resto no trilho. `parts` em frações da faixa.
fn meter_bar<'a>(parts: &[(Color, f32)]) -> Element<'a, Msg> {
    let mut bar = Row::new();
    let (mut used, mut total) = (0, 0.0);
    for &(color, share) in parts {
        total += share;
        let end = (total * FATIAS).round() as u16;
        let width = end.saturating_sub(used);
        used += width;
        if width > 0 {
            bar = bar.push(
                container(space())
                    .width(Length::FillPortion(width))
                    .height(Length::Fill)
                    .style(theme::fill(color)),
            );
        }
    }
    let rest = FATIAS as u16 - used;
    if rest > 0 {
        bar = bar.push(space().width(Length::FillPortion(rest)));
    }
    // A folga dos lados alinha a barra com o centro do marcador nas pontas.
    container(
        container(bar)
            .width(Length::Fill)
            .height(BARRA)
            .style(theme::meter_track),
    )
    .padding([(FAIXA - BARRA) / 2.0, f32::from(theme::MARKER) / 2.0])
    .into()
}

fn sensitivity<'a>(m: &'a Main, config: &'a Config) -> Element<'a, Msg> {
    let auto = config.sensitivity == Sensitivity::Auto;
    let toggle = toggler(auto)
        .label("Determinar automaticamente")
        .on_toggle(Msg::AutoSensitivity)
        .size(20)
        .text_size(14.4);
    let failed = m.call.is_none() && matches!(m.probe, Probe::Failed);
    let body: Element<'a, Msg> = if failed {
        text("Não foi possível abrir o microfone.")
            .size(13)
            .color(DANGER)
            .into()
    } else {
        let level = fraction(m.level);
        match config.sensitivity {
            Sensitivity::Auto => {
                let color = if m.open { GREEN } else { FG3 };
                column![
                    meter_bar(&[(color, level)]),
                    text("O Concord detecta quando você está falando.")
                        .size(13)
                        .color(FG2),
                ]
                .spacing(2)
                .into()
            }
            Sensitivity::Manual(db) => {
                let limit = fraction(db);
                let bar = meter_bar(&[(IDLE, level.min(limit)), (GREEN, (level - limit).max(0.0))]);
                let marker = slider(MANUAL_MIN_DB..=MANUAL_MAX_DB, db, Msg::Threshold)
                    .step(1.0f32)
                    .on_release(Msg::VolumeDone)
                    .height(FAIXA)
                    .style(theme::threshold);
                column![
                    stack![bar, marker],
                    row![
                        text("O microfone abre quando o nível passa da marca.")
                            .size(13)
                            .color(FG2),
                        space::horizontal(),
                        text(format!("{db:.0} dB")).size(13).font(theme::MEDIUM),
                    ],
                ]
                .spacing(2)
                .into()
            }
        }
    };
    labeled("Sensibilidade de entrada", column![toggle, body].spacing(8))
}

fn about_panel(m: &Main) -> Element<'_, Msg> {
    let status = m.check.as_ref().map(|check| {
        let (line, color) = match check {
            Check::Searching | Check::Busy => ("Procurando…".to_owned(), FG2),
            Check::Latest => ("Esta já é a versão mais nova.".to_owned(), FG2),
            Check::Downloading(v) => (format!("Baixando a versão {v}…"), FG2),
            Check::Ready(v) => (
                format!("Versão {v} pronta. Clique na seta verde pra reiniciar."),
                GREEN,
            ),
            Check::Failed(e) => (e.clone(), DANGER),
        };
        text(line).size(13).color(color)
    });
    let search = (!m.checking()).then_some(Msg::CheckUpdate);
    column![
        title("Sobre"),
        text(concat!("Concord ", env!("CARGO_PKG_VERSION"))).size(15),
        text("Cliente desktop nativo, em Rust.").size(14).color(FG2),
        column![action("Procurar atualização", search, theme::outline)]
            .push(status)
            .spacing(8),
    ]
    .spacing(12)
    .into()
}
