//! Soundboard da call: painel sobre o palco com os sons do servidor e o "Parar" pra todos, e o
//! aviso de quem tocou. Subir, cortar e apagar sons fica no site.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::widget::text::Wrapping;
use iced::widget::{Column, Row, button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Task};

use super::sidebar::{CLOSE, icon, tipped};
use super::stage::floating;
use super::theme::{self, BORDER, DANGER, FG, FG2, IDLE};
use super::voice::ActiveCall;
use super::{Main, Msg as App};
use crate::api::{Client, Sound};
use crate::audio;
use crate::config::Config;
use crate::soundboard::{self, Cooldown};

/// Quanto o "fulano tocou" fica na tela, como no site.
pub(super) const NOTICE_FOR: Duration = Duration::from_millis(2500);
const WIDTH: f32 = 340.0;
const COLUMNS: usize = 3;
/// Altura da grade a partir da qual ela rola: quatro fileiras.
const GRID: f32 = 196.0;
/// Do primeiro botão ao fim da fileira: o botão (8 + texto de 2 linhas + 8) e o espaço.
const ROW: f32 = 49.0;

#[derive(Clone, Debug)]
pub enum Msg {
    /// Botão da barra: abre o painel ou fecha, se já está aberto.
    Open,
    /// Lista do servidor, pela id da call que pediu.
    Listed(u64, Result<Vec<Sound>, String>),
    /// "Tentar de novo" da lista que não veio.
    Reload,
    /// Clique num som: toca pra todos e pra si.
    Play(Sound),
    /// "Parar": corta os sons de todo mundo.
    Stop,
    /// Som tocado por alguém da sala, pelo nome, que é a chave do volume.
    Heard(u64, String, Sound),
    /// Alguém apertou o "Parar".
    Stopped(u64),
    /// Som baixado e decodificado, pela chave do cache, com o ganho que ele vai tocar.
    Ready(u64, String, Result<Arc<[i16]>, String>, f32),
    /// Passou o tempo do aviso. O número separa este aviso de um mais novo.
    Expire(u64, u64),
}

#[derive(Debug, Default)]
pub struct Board {
    /// Painel aberto.
    pub open: bool,
    /// `None` enquanto carrega; a lista é buscada de novo a cada abertura.
    list: Option<Result<Vec<Sound>, String>>,
    /// Sons já baixados e cortados. Só na memória, e somem ao sair da call, como no site.
    pcm: HashMap<String, Arc<[i16]>>,
    cooldown: Cooldown,
    /// "Fulano tocou tal som", e o número do aviso.
    pub notice: Option<String>,
    seq: u64,
}

impl Board {
    fn notify(&mut self, line: String) -> u64 {
        self.notice = Some(line);
        self.seq += 1;
        self.seq
    }
}

/// O som pode ter o corte trocado no site sem mudar de arquivo: o corte entra na chave.
pub(super) fn key(sound: &Sound) -> String {
    format!("{}#{}-{:?}", sound.url, sound.trim_start, sound.trim_end)
}

/// Ganho final, como o site: o volume da soundboard de quem tocou vezes o geral, até 2. O
/// próprio clique só usa o geral. Surdo dá zero, e aí o som nem é baixado.
pub fn gain(config: &Config, by: Option<&str>) -> f32 {
    if config.deafened {
        return 0.0;
    }
    let person = by.map_or(1.0, |b| {
        audio::gain(config.volume_of(&Config::soundboard_key(b)))
    });
    (person * audio::gain(config.master_volume)).min(2.0)
}

pub fn action(msg: &Msg) -> Option<String> {
    match msg {
        Msg::Open => Some("clicou na soundboard".into()),
        Msg::Play(_) => Some("tocou um som da soundboard".into()),
        Msg::Stop => Some("parou os sons da soundboard".into()),
        _ => None,
    }
}

pub fn update(m: &mut Main, config: &Config, msg: Msg) -> Task<App> {
    let me = m.user.username.clone();
    let client = m.client.clone();
    let Some(call) = m.call.as_mut() else {
        return Task::none();
    };
    let id = call.id;
    match msg {
        Msg::Open => {
            if std::mem::take(&mut call.board.open) {
                return Task::none();
            }
            // Um painel por vez.
            (call.volume_panel, call.share_panel, call.share_menu) = (None, false, false);
            call.people = None;
            call.board.open = true;
            list(call, client)
        }
        Msg::Reload => list(call, client),
        Msg::Listed(from, result) => {
            if from == id {
                call.board.list = Some(result);
            }
            Task::none()
        }
        Msg::Play(sound) => {
            // O mesmo cooldown dos outros: ninguém ouve um som que os outros descartaram.
            if !call.board.cooldown.allow(&me, Instant::now()) {
                return Task::none();
            }
            if let Some(c) = &call.call {
                c.play_sound(&sound);
            }
            let seq = call.board.notify(format!("Você tocou {}", sound.name));
            Task::batch([
                play(call, client, &sound, gain(config, None)),
                expire(id, seq),
            ])
        }
        Msg::Heard(from, by, sound) => {
            if from != id || !call.board.cooldown.allow(&by, Instant::now()) {
                return Task::none();
            }
            // O aviso aparece mesmo com o volume dela em zero, como no site.
            let seq = call.board.notify(format!("{by} tocou {}", sound.name));
            Task::batch([
                play(call, client, &sound, gain(config, Some(&by))),
                expire(id, seq),
            ])
        }
        // Para aqui na hora, mesmo sem sala, como no site.
        Msg::Stop => {
            if let Some(c) = &call.call {
                c.stop_sounds();
            }
            silence(call);
            Task::none()
        }
        Msg::Stopped(from) => {
            if from == id {
                silence(call);
            }
            Task::none()
        }
        Msg::Ready(from, key, result, gain) => {
            match result {
                Ok(pcm) if from == id => {
                    if let Some(engine) = &call.engine {
                        engine.sound(pcm.clone(), gain);
                    }
                    call.board.pcm.insert(key, pcm);
                }
                Ok(_) => {}
                Err(e) => log!("soundboard: som não tocou: {e}"),
            }
            Task::none()
        }
        Msg::Expire(from, seq) => {
            if from == id && call.board.seq == seq {
                call.board.notice = None;
            }
            Task::none()
        }
    }
}

/// Corta os sons da soundboard daqui e tira o aviso.
fn silence(call: &mut ActiveCall) {
    if let Some(engine) = &call.engine {
        engine.stop_sounds();
    }
    call.board.notice = None;
}

/// Busca a lista de novo: quem subiu um som no site vê ele na próxima abertura.
fn list(call: &mut ActiveCall, client: Client) -> Task<App> {
    call.board.list = None;
    let id = call.id;
    Task::future(async move {
        let result = client.sounds().await.map_err(|e| format!("{e:?}"));
        App::Soundboard(Msg::Listed(id, result))
    })
}

/// Toca já baixado, ou baixa e decodifica fora da thread da interface. Ganho zero não toca
/// nem baixa.
fn play(call: &mut ActiveCall, client: Client, sound: &Sound, gain: f32) -> Task<App> {
    if gain <= 0.0 {
        return Task::none();
    }
    let key = key(sound);
    if let Some(pcm) = call.board.pcm.get(&key) {
        if let Some(engine) = &call.engine {
            engine.sound(pcm.clone(), gain);
        }
        return Task::none();
    }
    let (id, sound) = (call.id, sound.clone());
    Task::future(async move {
        let pcm = fetch(&client, sound).await;
        App::Soundboard(Msg::Ready(id, key, pcm, gain))
    })
}

/// O arquivo vem de outra pessoa: decodificar roda numa thread à parte, e um pânico do
/// decodificador vira erro em vez de derrubar o app.
async fn fetch(client: &Client, sound: Sound) -> Result<Arc<[i16]>, String> {
    let bytes = client
        .sound(&sound.url)
        .await
        .map_err(|e| format!("download: {e:?}"))?;
    tokio::task::spawn_blocking(move || soundboard::decode(bytes, sound.trim_start, sound.trim_end))
        .await
        .map_err(|e| format!("decodificador: {e}"))?
}

fn expire(id: u64, seq: u64) -> Task<App> {
    Task::future(async move {
        tokio::time::sleep(NOTICE_FOR).await;
        App::Soundboard(Msg::Expire(id, seq))
    })
}

/// "Fulano tocou tal som", acima da barra, com o painel fechado.
pub fn notice(call: &ActiveCall) -> Option<Element<'_, App>> {
    let line = call.board.notice.as_deref().filter(|_| !call.board.open)?;
    Some(
        container(text(line).size(13.5).font(theme::SEMIBOLD).color(IDLE))
            .padding([7, 14])
            .style(theme::tooltip)
            .into(),
    )
}

/// Painel aberto, ou nada.
pub fn card(call: &ActiveCall) -> Option<Element<'_, App>> {
    let board = &call.board;
    if !board.open {
        return None;
    }
    let stop = button(text("Parar").size(13).font(theme::SEMIBOLD))
        .padding([5, 12])
        .style(theme::danger)
        .on_press(App::Soundboard(Msg::Stop));
    let stop = tipped(stop.into(), "Parar o som que está tocando, pra todo mundo");
    let close = button(container(icon(CLOSE, 16.0, FG2)).center(28))
        .padding(0)
        .style(theme::ghost)
        .on_press(App::CloseVolume);
    let head = row![
        text("Soundboard")
            .size(14.4)
            .font(theme::SEMIBOLD)
            .width(Length::Fill),
        stop,
        close
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    let hint = |line: &'static str| text(line).size(13).color(FG2).into();
    let body: Element<'_, App> = match &board.list {
        None => hint("Carregando sons…"),
        Some(Err(_)) => {
            let retry = button(text("Tentar de novo").size(13).font(theme::SEMIBOLD))
                .padding([5, 12])
                .style(theme::outline)
                .on_press(App::Soundboard(Msg::Reload));
            row![
                text("Não foi possível carregar os sons.")
                    .size(13)
                    .color(DANGER)
                    .width(Length::Fill),
                retry
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into()
        }
        Some(Ok(list)) if list.is_empty() => {
            hint("Nenhum som ainda. Os sons são enviados pelo site, em Configurações › Soundboard.")
        }
        Some(Ok(list)) => grid(list),
    };
    let foot: Element<'_, App> = match &board.notice {
        Some(line) => text(line).size(12.5).color(IDLE).into(),
        None => space().into(),
    };

    Some(
        container(column![head, body, foot].spacing(10))
            .width(WIDTH)
            .padding(14)
            .style(floating(BORDER))
            .into(),
    )
}

/// Botões com o nome do som, em [`COLUMNS`] colunas, rolando depois de quatro fileiras.
fn grid(list: &[Sound]) -> Element<'_, App> {
    let mut rows = Column::new().spacing(6);
    for chunk in list.chunks(COLUMNS) {
        let mut line = Row::new().spacing(6);
        for sound in chunk {
            let name = text(&sound.name)
                .size(13)
                .color(FG)
                .center()
                .wrapping(Wrapping::WordOrGlyph);
            line = line.push(
                button(container(name).center_x(Length::Fill))
                    .padding([8, 6])
                    .width(Length::FillPortion(1))
                    .style(theme::outline)
                    .on_press(App::Soundboard(Msg::Play(sound.clone()))),
            );
        }
        for _ in chunk.len()..COLUMNS {
            line = line.push(space().width(Length::FillPortion(1)));
        }
        rows = rows.push(line);
    }
    let height = (list.len().div_ceil(COLUMNS) as f32 * ROW).min(GRID);
    scrollable(rows)
        .height(height)
        .style(theme::thin_scroll)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ganho_e_o_volume_da_pessoa_vezes_o_geral_e_surdo_zera() {
        let close = |a: f32, b: f32| (a - b).abs() < 0.001;
        let mut c = Config {
            master_volume: 50,
            ..Config::default()
        };
        assert!(close(gain(&c, None), 0.5), "o próprio clique: só o geral");
        c.volumes.insert(Config::soundboard_key("ana"), 100);
        assert!(close(gain(&c, Some("ana")), 2.0 * 0.5));
        c.master_volume = 100;
        assert!(close(gain(&c, Some("ana")), 2.0), "teto em 2");
        c.volumes.insert(Config::soundboard_key("bia"), 0);
        assert_eq!(gain(&c, Some("bia")), 0.0);
        c.volumes.insert("caio".into(), 0);
        assert!(
            gain(&c, Some("caio")) > 0.0,
            "voz em zero não cala a soundboard"
        );
        c.deafened = true;
        assert_eq!(gain(&c, Some("ana")), 0.0);
    }

    #[test]
    fn corte_diferente_e_outro_som_no_cache() {
        let s = Sound {
            id: 1,
            name: "a".into(),
            url: "/api/sounds/1?v=a.mp3".into(),
            trim_start: 0.0,
            trim_end: None,
        };
        let cortado = Sound {
            trim_end: Some(1.0),
            ..s.clone()
        };
        assert_ne!(key(&s), key(&cortado));
    }
}
