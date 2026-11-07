//! Palco da call: grade de tiles, barra de controles e painel de volume.
use iced::widget::text::Wrapping;
use iced::widget::{
    Column, Row, button, center, checkbox, column, container, image, mouse_area, opaque, pick_list,
    pin, responsive, row, scrollable, slider, space, stack, text, toggler,
};
use iced::{Alignment, Color, ContentFit, Element, Length, Padding, Size, Theme, border};

use super::sidebar::{
    CLOSE, HEADPHONES, HEADPHONES_OFF, MIC, MIC_OFF, SCREEN, VOLUME, VOLUME_X, icon, live_badge,
    tipped,
};
use super::theme::{self, ACCENT, ACCENT2, BG, BG2, BORDER, BORDER2, DANGER, FG, FG2, GREEN, IDLE};
use super::voice::{self, ActiveCall, GAP, Status};
use super::{Main, Msg, avatar};
use crate::call::{Live, MAX_ATTEMPTS};
use crate::config::{Config, SCREEN as SCREEN_KEY};
use crate::share::{Choice, Quality, Source, State, View};

/// Largura fixa e altura aproximada do painel de volume, pra centrar no tile.
const PANEL: Size = Size::new(280.0, 104.0);
/// Largura do painel de qualidade.
const SHARE_PANEL: f32 = 340.0;
/// Seletor do Windows: três miniaturas por linha, com rolagem a partir da terceira.
const PICKER: f32 = 620.0;
const PICKER_COLUMNS: usize = 3;
const PICKER_GRID: f32 = 300.0;
const THUMB_HEIGHT: f32 = 104.0;
/// Largura do nome embaixo da miniatura: (620 - 2×14 de margem - 2×8 entre elas) / 3 - 2×6
/// do botão. A barra de rolagem flutua por cima, não come largura.
const THUMB_NAME: f32 = 180.0;
const THUMB_NAME_SIZE: f32 = 12.0;
/// Barra de controles com a margem dela, pra os avisos abrirem logo acima.
const ABOVE_BAR: f32 = 94.0;
/// Faixa do pé do palco onde o mouse faz a barra aparecer.
const BAR_ZONE: f32 = 140.0;
/// Coluna das pessoas ao lado da transmissão grande.
const SIDE: Size = Size::new(240.0, 135.0);

pub fn view<'a>(m: &'a Main, call: &'a ActiveCall, config: &'a Config) -> Element<'a, Msg> {
    let base = with_bar(call, config, move |area| match &call.live {
        Some(live) if !live.is_empty() => {
            if call.watching.is_some() && call.view == voice::View::Focus {
                focus(m, call, config, live, area)
            } else {
                grid(m, call, config, live, area)
            }
        }
        _ => center(text("Conectando…").size(15).color(FG2)).into(),
    });

    let mut layers = vec![container(base).style(theme::fill(theme::STAGE)).into()];
    if let Status::Reconnecting(n) = call.status {
        layers.push(banner(n));
    }
    layers.extend(share_overlay(call, config));
    stack(layers).into()
}

/// Palco com a barra de controles por cima, só com o mouse perto do pé.
fn with_bar<'a>(
    call: &'a ActiveCall,
    config: &'a Config,
    body: impl Fn(Size) -> Element<'a, Msg> + 'a,
) -> Element<'a, Msg> {
    responsive(move |area| {
        let mut layers = stack![
            container(body(area))
                .width(Length::Fill)
                .height(Length::Fill)
        ];
        if call.bar_visible() {
            layers = layers.push(
                container(bar(call, config))
                    .center_x(Length::Fill)
                    .align_bottom(Length::Fill)
                    .padding([12, 20]),
            );
        }
        // A posição serve também pro painel do botão direito abrir onde foi o clique.
        mouse_area(layers)
            .on_move(move |p| Msg::StageMouse(p, p.y > area.height - BAR_ZONE))
            .on_exit(Msg::BarNear(false))
            .into()
    })
    .into()
}

/// Transmissão assistida grande, com a outra transmissão e as pessoas numa coluna do lado.
fn focus<'a>(
    m: &'a Main,
    call: &'a ActiveCall,
    config: &'a Config,
    live: &'a [Live],
    area: Size,
) -> Element<'a, Msg> {
    let watched = live
        .iter()
        .find(|p| Some(&p.identity) == call.watching.as_ref());
    let big: Element<'_, Msg> = match watched {
        Some(p) => {
            let body = stack![screen_image(call, 15.0), screen_tag(p, area.width)];
            mouse_area(
                container(body)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .clip(true)
                    .style(rounded_stage),
            )
            .on_press(Msg::FocusScreen)
            .on_right_press(Msg::OpenVolume(Config::screen_key(voice::name_of(p))))
            .into()
        }
        None => space().into(),
    };
    let others = live
        .iter()
        .filter(|p| p.screen_share && Some(&p.identity) != call.watching.as_ref())
        .map(|p| screen_tile(call, p, SIDE))
        .chain(live.iter().map(|p| tile(m, call, p, SIDE)));
    let side = scrollable(Column::with_children(others).spacing(GAP))
        .width(SIDE.width)
        .style(theme::thin_scroll);
    let layout = container(row![big, side].spacing(GAP)).padding(GAP);
    let panel = call.volume_panel.as_deref().map(|name| {
        let at = voice::panel_origin(area, call.volume_at, panel_size(name));
        pin(volume_card(call, config, name)).x(at.x).y(at.y)
    });
    match panel {
        Some(panel) => stack![layout, panel].into(),
        None => layout.into(),
    }
}

/// Tamanho do painel de volume: o da transmissão tem a linha de botões a mais.
fn panel_size(name: &str) -> Size {
    if name.ends_with(SCREEN_KEY) {
        Size::new(PANEL.width, PANEL.height + 46.0)
    } else {
        PANEL
    }
}

fn rounded_stage(_: &Theme) -> container::Style {
    container::Style {
        background: Some(theme::STAGE.into()),
        border: border::rounded(18),
        ..Default::default()
    }
}

/// O quadro mais novo da transmissão assistida, inteiro na área.
fn screen_image(call: &ActiveCall, loading: f32) -> Element<'_, Msg> {
    match &call.screen {
        Some(a) => image(a.handle().clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(ContentFit::Contain)
            .into(),
        None => center(text("Carregando a transmissão…").size(loading).color(FG2)).into(),
    }
}

/// "Nome · tela" com o selo, no canto de baixo.
fn screen_tag(p: &Live, width: f32) -> Element<'_, Msg> {
    let label = text(Config::screen_key(voice::name_of(p)))
        .size(13)
        .font(theme::SEMIBOLD)
        .wrapping(Wrapping::None);
    let tag = row![
        live_badge(),
        container(label)
            .max_width((width - 110.0).max(40.0))
            .clip(true)
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    container(container(tag).padding([5, 10]).style(theme::pill))
        .align_left(Length::Fill)
        .align_bottom(Length::Fill)
        .padding(10)
        .into()
}

fn grid<'a>(
    m: &'a Main,
    call: &'a ActiveCall,
    config: &'a Config,
    live: &'a [Live],
    area: Size,
) -> Element<'a, Msg> {
    // Cada transmissão vira um tile próprio, antes das pessoas, como no site.
    let screens: Vec<&Live> = live.iter().filter(|p| p.screen_share).collect();
    let cells: Vec<(&Live, bool)> = screens
        .iter()
        .map(|p| (*p, true))
        .chain(live.iter().map(|p| (p, false)))
        .collect();
    let count = cells.len();
    let size = voice::tile_size(area, count);
    let (cols, _) = voice::grid(count);
    let rows = cells.chunks(cols).map(|chunk| {
        Row::with_children(chunk.iter().map(|&(p, screen)| {
            if screen {
                screen_tile(call, p, size)
            } else {
                tile(m, call, p, size)
            }
        }))
        .spacing(GAP)
        .into()
    });
    let tiles = center(Column::with_children(rows).spacing(GAP));
    let panel = call.volume_panel.as_deref().map(|name| {
        let at = voice::panel_origin(area, call.volume_at, panel_size(name));
        pin(volume_card(call, config, name)).x(at.x).y(at.y)
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

/// Tile da transmissão de alguém. Sem assistir: a prévia borrada e o botão. Assistindo: o
/// vídeo, com tela cheia e parar no canto.
fn screen_tile<'a>(call: &'a ActiveCall, p: &'a Live, size: Size) -> Element<'a, Msg> {
    if p.is_local {
        return own_tile(call, p, size);
    }
    let watching = call.watching.as_ref() == Some(&p.identity);
    let mut body = stack![];
    if watching {
        body = body.push(screen_image(call, 14.0));
        let stop = button(text("Parar").size(12.5).font(theme::SEMIBOLD))
            .padding([5, 10])
            .style(theme::outline)
            .on_press(Msg::Watch(None));
        body = body.push(
            container(stop)
                .align_right(Length::Fill)
                .align_top(Length::Fill)
                .padding(10),
        );
    } else {
        if let Some(preview) = call.previews.get(&p.identity) {
            body = body.push(
                image(preview.clone())
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .content_fit(ContentFit::Cover),
            );
        }
        let watch = button(text("Assistir transmissão").size(14).font(theme::BOLD))
            .padding([9, 18])
            .style(theme::primary)
            .on_press(Msg::Watch(Some(p.identity.clone())));
        body = body.push(
            container(center(watch))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::fill(Color::from_rgba8(0, 0, 0, 0.45))),
        );
    }
    body = body.push(screen_tag(p, size.width));
    let tile = container(body)
        .width(size.width)
        .height(size.height)
        .clip(true)
        .style(rounded_stage);
    // Clique na assistida aumenta; botão direito abre o volume dela e a tela cheia.
    let mut area =
        mouse_area(tile).on_right_press(Msg::OpenVolume(Config::screen_key(voice::name_of(p))));
    if watching {
        area = area.on_press(Msg::FocusScreen);
    }
    area.into()
}

/// Tile da própria transmissão: a prévia do que sai, com Trocar no canto. Sem foco a
/// prévia para no último quadro, com o aviso.
fn own_tile<'a>(call: &'a ActiveCall, p: &'a Live, size: Size) -> Element<'a, Msg> {
    let preview: Element<'_, Msg> = match &call.own {
        Some(a) => image(a.handle().clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(ContentFit::Contain)
            .into(),
        None => center(text("Carregando a prévia…").size(13).color(FG2)).into(),
    };
    let swap = button(text("Trocar").size(12.5).font(theme::SEMIBOLD))
        .padding([5, 10])
        .style(theme::outline)
        .on_press_maybe((call.share == State::Live).then_some(Msg::ShareSwap));
    let corner = |e: Element<'a, Msg>, right: bool| {
        let c = container(e).align_top(Length::Fill).padding(10);
        if right {
            c.align_right(Length::Fill)
        } else {
            c.align_left(Length::Fill)
        }
    };
    let paused = call.preview_paused.then(|| {
        let note = text("Prévia pausada").size(12).color(FG2);
        corner(
            container(note).padding([4, 9]).style(theme::pill).into(),
            false,
        )
    });
    let body = stack![preview, corner(swap.into(), true)]
        .push(paused)
        .push(screen_tag(p, size.width));
    // Fundo de tile, não de palco: carregando, o tile não some no escuro.
    container(body)
        .width(size.width)
        .height(size.height)
        .clip(true)
        .style(|_| container::Style {
            background: Some(BG2.into()),
            border: border::rounded(18),
            ..Default::default()
        })
        .into()
}

/// Transmissão assistida em tela cheia, sem a barra lateral. A barra de controles aparece
/// com o mouse perto do pé, com "Fechar transmissão" no lugar de sair; Esc volta.
pub fn full<'a>(call: &'a ActiveCall, config: &'a Config) -> Element<'a, Msg> {
    let base = with_bar(call, config, move |_| screen_image(call, 15.0));
    container(base).style(theme::fill(theme::STAGE)).into()
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
    // Com a transmissão grande ou em tela cheia, o botão vermelho fecha ela, não a call.
    let closing = call.watching.is_some() && call.view != voice::View::Tile;
    let (label, msg) = if closing {
        ("Fechar transmissão", Msg::Watch(None))
    } else {
        ("Sair", Msg::Leave)
    };
    let leave = button(
        text(label)
            .size(15)
            .font(theme::BOLD)
            .center()
            .height(Length::Fill),
    )
    .padding([0, 20])
    .height(46)
    .style(theme::leave)
    .on_press(msg);

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
fn share_overlay<'a>(call: &'a ActiveCall, config: &'a Config) -> Option<Element<'a, Msg>> {
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
        // O seletor do Windows aberto pelo Trocar.
        State::Live if call.share_panel && cfg!(windows) => picker(call, config),
        State::Live => match &call.share_audio {
            Some(view) if call.audio_list => audio_card(view),
            Some(view) => audio_line(view),
            None => return None,
        },
        _ if call.share_panel && cfg!(windows) => picker(call, config),
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
    // No Windows o alvo já veio do nosso seletor: só falta o primeiro quadro.
    let label = if cfg!(windows) {
        "Abrindo a captura…"
    } else {
        "Escolha a tela na janela do sistema…"
    };
    let line = row![text(label).size(13.5).font(theme::SEMIBOLD), cancel,]
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
    // Etapa 4: o áudio por app do Windows ainda não existe.
    let audio: Option<Element<'_, Msg>> = cfg!(target_os = "linux").then(|| {
        toggler(config.share_audio)
            .label("Transmitir áudio")
            .on_toggle(Msg::ShareAudio)
            .size(20)
            .text_size(14.4)
            .into()
    });

    let body = column![head, Column::with_children(options).spacing(2)]
        .push(audio)
        .push(start)
        .spacing(10);
    container(body)
        .width(SHARE_PANEL)
        .padding(12)
        .style(floating(BORDER))
        .into()
}

/// Seletor do Windows: abas de aplicativos e telas, miniatura com o nome embaixo, áudio e
/// qualidade. No Linux quem escolhe é o portal, e vale o [`share_card`].
pub(super) fn picker<'a>(call: &'a ActiveCall, config: &'a Config) -> Element<'a, Msg> {
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

    let tab = |label, windows: bool| {
        button(text(label).size(14).font(theme::SEMIBOLD).center())
            .padding([7, 16])
            .style(theme::nav_item(call.share_windows == windows))
            .on_press(Msg::ShareTab(windows))
    };
    let tabs = row![tab("Aplicativos", true), tab("Telas", false)].spacing(6);

    let grid: Element<'_, Msg> = match &call.share_sources {
        None => center(text("Lendo telas e janelas…").size(13.5).color(FG2))
            .height(PICKER_GRID)
            .into(),
        Some(list) => {
            let tiles: Vec<Element<'_, Msg>> = list
                .iter()
                .filter(|(s, _)| s.window == call.share_windows)
                .map(|(s, thumb)| {
                    source_tile(s, thumb.as_ref(), call.share_source.as_ref() == Some(s))
                })
                .collect();
            if tiles.is_empty() {
                center(text("Nenhuma janela aberta.").size(13.5).color(FG2))
                    .height(PICKER_GRID)
                    .into()
            } else {
                let mut rows = Column::new().spacing(8);
                let mut tiles = tiles.into_iter().peekable();
                while tiles.peek().is_some() {
                    let mut line = Row::new().spacing(8);
                    for _ in 0..PICKER_COLUMNS {
                        line = line.push(match tiles.next() {
                            Some(t) => t,
                            None => space().width(Length::FillPortion(1)).into(),
                        });
                    }
                    rows = rows.push(line);
                }
                scrollable(rows)
                    .height(PICKER_GRID)
                    .style(theme::thin_scroll)
                    .into()
            }
        }
    };

    let quality = pick_list(Quality::ALL, Some(config.share_quality), Msg::ShareQuality)
        .padding([8, 12])
        .style(theme::pick);
    let audio = checkbox(config.share_audio)
        .label(if call.share_windows {
            "Áudio do aplicativo"
        } else {
            "Áudio do computador"
        })
        .on_toggle(Msg::ShareAudio)
        .size(18)
        .text_size(14);
    // Pelo Trocar, a qualidade e o áudio seguem os da transmissão no ar.
    let swapping = call.share == State::Live;
    let label = if swapping { "Trocar" } else { "Transmitir" };
    let start = button(text(label).size(14.5).font(theme::BOLD).center())
        .padding([10, 22])
        .style(theme::primary)
        .on_press_maybe(call.share_source.is_some().then_some(Msg::ShareStart));
    let foot = row![]
        .push((!swapping).then_some(quality))
        .push(space().width(Length::Fill))
        .push((!swapping).then_some(audio))
        .push(start)
        .spacing(14)
        .align_y(Alignment::Center);

    container(column![head, tabs, grid, foot].spacing(12))
        .width(PICKER)
        .padding(14)
        .style(floating(BORDER))
        .into()
}

/// Miniatura em 16:9 com o nome pequeno embaixo.
fn source_tile<'a>(
    source: &'a Source,
    thumb: Option<&'a image::Handle>,
    chosen: bool,
) -> Element<'a, Msg> {
    let preview: Element<'_, Msg> = match thumb {
        Some(h) => image(h.clone())
            .width(Length::Fill)
            .height(THUMB_HEIGHT)
            .content_fit(ContentFit::Contain)
            .into(),
        None => container(space())
            .width(Length::Fill)
            .height(THUMB_HEIGHT)
            .style(theme::fill(BG))
            .into(),
    };
    let name = text(ellipsize(&source.to_string(), THUMB_NAME_SIZE, THUMB_NAME))
        .size(THUMB_NAME_SIZE)
        .color(if chosen { FG } else { FG2 })
        .wrapping(Wrapping::None);
    button(column![preview, name].spacing(6))
        .padding(6)
        .width(Length::FillPortion(1))
        .style(theme::nav_item(chosen))
        .on_press(Msg::ShareSource(source.clone()))
        .into()
}

/// Corta numa linha com reticências pra caber em `width`. O `text` do iced 0.14 não corta
/// sozinho: sem quebra, vaza pro vizinho.
fn ellipsize(content: &str, size: f32, width: f32) -> String {
    let fits = |s: &str| text_width(s, size) <= width;
    if fits(content) {
        return content.to_owned();
    }
    // Maior prefixo que cabe junto com a reticência.
    let ends: Vec<usize> = content.char_indices().map(|(i, _)| i).collect();
    let n = ends.partition_point(|&end| fits(&format!("{}…", &content[..end])));
    format!("{}…", content[..ends[n.saturating_sub(1)]].trim_end())
}

/// Largura de uma linha na fonte do corpo, medida como o `text` desenha.
fn text_width(content: &str, size: f32) -> f32 {
    use iced::advanced::graphics::text::Paragraph;
    use iced::advanced::text::{Paragraph as _, Text};
    use iced::widget::text::{Alignment, LineHeight, Shaping};
    Paragraph::with_text(Text {
        content,
        bounds: Size::INFINITE,
        size: size.into(),
        line_height: LineHeight::default(),
        font: theme::BODY,
        align_x: Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: Shaping::default(),
        wrapping: Wrapping::None,
    })
    .min_bounds()
    .width
}

fn audio_label(choice: &Choice) -> String {
    match choice {
        Choice::App(app) => format!("Áudio: {app}"),
        Choice::All => "Áudio: computador todo".into(),
        Choice::Silent => "Sem áudio".into(),
    }
}

/// O que vai de áudio com a transmissão no ar, com o botão de trocar.
fn audio_line(view: &View) -> Element<'_, Msg> {
    let silent = view.choice == Choice::Silent;
    let change = button(text("Trocar").size(13).font(theme::SEMIBOLD))
        .padding([5, 12])
        .style(theme::outline)
        .on_press(Msg::AudioList);
    let line = row![
        icon(if silent { VOLUME_X } else { VOLUME }, 16.0, FG2),
        text(audio_label(&view.choice))
            .size(13.5)
            .font(theme::SEMIBOLD),
        change,
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    container(line)
        .padding([7, 14])
        .style(theme::tooltip)
        .into()
}

/// Lista de áudio: os apps tocando agora, o computador todo ou nada. Atualiza sozinha.
fn audio_card(view: &View) -> Element<'_, Msg> {
    let close = button(container(icon(CLOSE, 16.0, FG2)).center(28))
        .padding(0)
        .style(theme::ghost)
        .on_press(Msg::AudioList);
    let head = row![
        text("Áudio da transmissão")
            .size(14.4)
            .font(theme::SEMIBOLD)
            .width(Length::Fill),
        close
    ]
    .align_y(Alignment::Center);

    let option = |label: String, choice: Choice| -> Element<'_, Msg> {
        button(text(label).size(14).font(theme::MEDIUM))
            .padding([8, 12])
            .width(Length::Fill)
            .style(theme::nav_item(choice == view.choice))
            .on_press(Msg::AudioChoice(choice))
            .into()
    };
    let mut list: Vec<_> = view
        .apps
        .iter()
        .map(|app| option(app.clone(), Choice::App(app.clone())))
        .collect();
    if view.apps.is_empty() {
        list.push(
            container(text("Nenhum app tocando agora.").size(13).color(FG2))
                .padding([6, 12])
                .into(),
        );
    }
    // O escolhido continua na lista quando para de tocar, senão some a marcação.
    if let Choice::App(app) = &view.choice
        && !view.apps.contains(app)
    {
        list.push(option(
            format!("{app} (sem som agora)"),
            view.choice.clone(),
        ));
    }
    list.push(option("Computador todo".into(), Choice::All));
    list.push(option("Sem áudio".into(), Choice::Silent));

    container(column![head, Column::with_children(list).spacing(2)].spacing(10))
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
    let screen = name.ends_with(SCREEN_KEY);
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
        text(if screen { "Transmissão" } else { "Voz" })
            .size(13)
            .color(FG2),
        level,
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    // Da transmissão assistida: tela cheia completa e parar, só por aqui.
    let watched = call
        .live
        .iter()
        .flatten()
        .find(|l| Some(&l.identity) == call.watching.as_ref())
        .is_some_and(|l| Config::screen_key(voice::name_of(l)) == name);
    let actions = watched.then(|| {
        let small = |label, msg| {
            button(text(label).size(13).font(theme::SEMIBOLD).center())
                .padding([6, 12])
                .width(Length::Fill)
                .style(theme::outline)
                .on_press(msg)
        };
        row![
            small("Tela cheia", Msg::Fullscreen),
            small("Parar de assistir", Msg::Watch(None))
        ]
        .spacing(8)
    });

    let ring = if call.speaking(name) { ACCENT } else { BORDER };
    container(column![head, line].push(actions).spacing(10))
        .width(PANEL.width)
        .padding(12)
        .style(floating(ring))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nome_comprido_corta_com_reticencias_na_largura() {
        assert_eq!(ellipsize("Spotify", 12.0, 180.0), "Spotify");
        let longo = "Concord - chamada de voz self-hosted - Google Chrome ".repeat(3);
        let curto = ellipsize(&longo, 12.0, 180.0);
        assert!(curto.ends_with('…'), "{curto}");
        assert!(text_width(&curto, 12.0) <= 180.0, "{curto}");
        assert!(longo.starts_with(curto.trim_end_matches('…')), "{curto}");
        assert!(
            curto.chars().count() > 10,
            "corta perto da borda, não no começo: {curto}"
        );
    }
}
