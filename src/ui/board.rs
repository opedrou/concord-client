//! Soundboard: na call, o painel sobre o palco com os sons do servidor e o "Parar" pra todos, e
//! o aviso de quem tocou; nas configurações, subir e apagar sons. Renomear e cortar ficam no
//! site.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
use crate::api::{Client, Error, Sound, User};
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
/// Os formatos que o site aceita, no seletor de arquivo.
const FORMATS: crate::chat::Filter = ("Áudio", &["*.mp3", "*.ogg", "*.wav", "*.flac", "*.m4a"]);
/// Mesmo teto do upload de som no servidor.
const MAX_BYTES: u64 = 1024 * 1024;

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

/// Aba Soundboard das configurações.
#[derive(Clone, Debug)]
pub enum Lib {
    /// Abriu a aba, ou "Tentar de novo": busca a lista.
    Load,
    Loaded(Result<Vec<Sound>, String>),
    /// "Subir som": abre o seletor de arquivo.
    Upload,
    Picked(Option<PathBuf>),
    Uploaded(Result<Sound, Error>),
    /// Primeiro clique no "Apagar": só pergunta.
    Delete(i64),
    Cancel,
    Confirm(i64),
    Deleted(Result<(), Error>),
}

/// Lista, envio e o "Apagar" que pergunta, da aba Soundboard.
#[derive(Debug, Default)]
pub struct Library {
    list: Option<Result<Vec<Sound>, String>>,
    /// Escolhendo ou subindo um arquivo.
    busy: bool,
    /// Último erro de subir ou apagar.
    error: Option<String>,
    /// Som com o "Apagar" clicado uma vez.
    confirm: Option<i64>,
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

pub fn library_action(msg: &Lib) -> Option<String> {
    match msg {
        Lib::Upload => Some("clicou em subir som".into()),
        Lib::Confirm(id) => Some(format!("apagou o som {id}")),
        _ => None,
    }
}

/// Apagar só aparece pra quem subiu o som e pra admin, como o servidor confere.
pub(super) fn can_delete(user: &User, sound: &Sound) -> bool {
    user.is_admin || sound.uploaded_by == Some(user.id)
}

/// Nome do botão: o do arquivo, sem a extensão.
pub(super) fn upload_name(path: &Path) -> String {
    path.file_stem()
        .map_or_else(|| "som".into(), |s| s.to_string_lossy().into_owned())
}

/// Nome e bytes de um arquivo de som. Passou do teto, nem lê.
fn read_sound(path: &Path) -> Result<(String, Vec<u8>), String> {
    let unreadable = |_| "Não foi possível ler o arquivo.".to_owned();
    if std::fs::metadata(path).map_err(unreadable)?.len() > MAX_BYTES {
        return Err("O som passa de 1 MB.".into());
    }
    let bytes = std::fs::read(path).map_err(unreadable)?;
    Ok((upload_name(path), bytes))
}

fn upload_error(e: Error) -> String {
    match e {
        Error::Server(413) => "O som passa de 1 MB.".into(),
        Error::Unsupported => "Formato não aceito: use mp3, ogg, wav, flac ou m4a.".into(),
        Error::Server(400) => "O arquivo está vazio.".into(),
        Error::Offline => "Sem conexão com o servidor. Tente de novo.".into(),
        _ => "Não foi possível subir o som. Tente de novo.".into(),
    }
}

pub fn library(m: &mut Main, msg: Lib) -> Task<App> {
    let client = m.client.clone();
    let lib = &mut m.library;
    let task = App::SoundLibrary;
    match msg {
        Lib::Load => {
            lib.list = None;
            lib.confirm = None;
            Task::future(async move {
                let result = client.sounds().await.map_err(|e| format!("{e:?}"));
                task(Lib::Loaded(result))
            })
        }
        Lib::Loaded(result) => {
            lib.list = Some(result);
            Task::none()
        }
        Lib::Upload => {
            if lib.busy {
                return Task::none();
            }
            (lib.busy, lib.error) = (true, None);
            Task::future(async move {
                task(Lib::Picked(
                    crate::chat::pick_file("Subir som", Some(FORMATS)).await,
                ))
            })
        }
        Lib::Picked(None) => {
            lib.busy = false;
            Task::none()
        }
        Lib::Picked(Some(path)) => {
            match read_sound(&path) {
                Ok((name, bytes)) => Task::future(async move {
                    task(Lib::Uploaded(client.upload_sound(&name, bytes).await))
                }),
                Err(e) => {
                    (lib.busy, lib.error) = (false, Some(e));
                    Task::none()
                }
            }
        }
        Lib::Uploaded(result) => {
            lib.busy = false;
            match result {
                Ok(_) => library(m, Lib::Load),
                Err(e) => {
                    lib.error = Some(upload_error(e));
                    Task::none()
                }
            }
        }
        Lib::Delete(id) => {
            (lib.confirm, lib.error) = (Some(id), None);
            Task::none()
        }
        Lib::Cancel => {
            lib.confirm = None;
            Task::none()
        }
        Lib::Confirm(id) => {
            lib.confirm = None;
            Task::future(async move { task(Lib::Deleted(client.delete_sound(id).await)) })
        }
        // Já apagado por outra pessoa também sai da lista.
        Lib::Deleted(Ok(()) | Err(Error::Server(404))) => library(m, Lib::Load),
        Lib::Deleted(Err(e)) => {
            lib.error = Some(match e {
                Error::Server(403) => "Só quem subiu o som ou um admin pode apagar.".into(),
                _ => "Não foi possível apagar o som. Tente de novo.".into(),
            });
            Task::none()
        }
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
            hint("Nenhum som ainda. Suba um em Configurações › Soundboard.")
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

/// Aba Soundboard das configurações: subir e apagar. Renomear e cortar ficam no site.
pub fn library_panel(m: &Main) -> Element<'_, App> {
    let lib = &m.library;
    let msg = App::SoundLibrary;
    let small = |label, style: fn(&iced::Theme, button::Status) -> button::Style, l| {
        button(text(label).size(13).font(theme::SEMIBOLD))
            .padding([5, 12])
            .style(style)
            .on_press(msg(l))
    };
    let upload = button(
        text(if lib.busy { "Subindo…" } else { "Subir som" })
            .size(14)
            .font(theme::SEMIBOLD),
    )
    .padding([9, 16])
    .style(theme::primary)
    .on_press_maybe((!lib.busy).then(|| msg(Lib::Upload)));
    let error = lib.error.as_deref().map(|e| text(e).size(13).color(DANGER));
    let list: Element<'_, App> = match &lib.list {
        None => text("Carregando sons…").size(14).color(FG2).into(),
        Some(Err(_)) => row![
            text("Não foi possível carregar os sons.")
                .size(14)
                .color(DANGER),
            small("Tentar de novo", theme::outline, Lib::Load),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .into(),
        Some(Ok(list)) if list.is_empty() => text("Nenhum som ainda.").size(14).color(FG2).into(),
        Some(Ok(list)) => Column::with_children(list.iter().map(|s| {
            let delete: Option<Element<'_, App>> = can_delete(&m.user, s).then(|| {
                if lib.confirm == Some(s.id) {
                    row![
                        text("Apagar pra todo mundo?").size(13).color(FG2),
                        small("Apagar", theme::danger, Lib::Confirm(s.id)),
                        small("Cancelar", theme::ghost, Lib::Cancel),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
                } else {
                    small("Apagar", theme::outline, Lib::Delete(s.id)).into()
                }
            });
            row![text(&s.name).size(14).color(FG).width(Length::Fill)]
                .push(delete)
                .spacing(8)
                .height(34)
                .align_y(Alignment::Center)
                .into()
        }))
        .spacing(4)
        .into(),
    };
    let hint = "Os sons tocam pra todo mundo na call, pelo botão da barra. Aceita mp3, ogg, wav, \
        flac ou m4a, até 1 MB. Renomear e cortar ficam no site.";
    column![
        text("Soundboard").size(20).font(theme::DISPLAY),
        text(hint).size(13).color(FG2),
        row![upload]
            .push(error)
            .spacing(12)
            .align_y(Alignment::Center),
        list,
    ]
    .spacing(16)
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
    fn apagar_so_pra_quem_subiu_ou_admin_e_nome_sem_extensao() {
        let user = |id, is_admin| User {
            id,
            username: "ana".into(),
            is_admin,
            avatar_url: None,
        };
        let de = |uploaded_by| Sound {
            id: 1,
            name: "a".into(),
            url: "/api/sounds/1?v=a.mp3".into(),
            trim_start: 0.0,
            trim_end: None,
            uploaded_by,
        };
        assert!(can_delete(&user(5, false), &de(Some(5))));
        assert!(!can_delete(&user(5, false), &de(Some(6))), "som de outro");
        assert!(!can_delete(&user(5, false), &de(None)), "conta apagada");
        assert!(
            can_delete(&user(5, true), &de(Some(6))),
            "admin apaga qualquer um"
        );
        assert_eq!(upload_name(Path::new("/x/Buzina alta.mp3")), "Buzina alta");
        assert_eq!(upload_name(Path::new("/x/sem-extensao")), "sem-extensao");
        assert_eq!(upload_name(Path::new("/x/a.b.ogg")), "a.b");
    }

    #[test]
    fn subir_recusa_arquivo_grande_antes_de_mandar() {
        let pasta = std::env::temp_dir().join(format!("concord-som-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let grande = pasta.join("grande.mp3");
        std::fs::write(&grande, vec![0u8; MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(read_sound(&grande).unwrap_err(), "O som passa de 1 MB.");
        let ok = pasta.join("sino.ogg");
        std::fs::write(&ok, [1, 2, 3]).unwrap();
        assert_eq!(read_sound(&ok).unwrap(), ("sino".to_owned(), vec![1, 2, 3]));
        std::fs::remove_dir_all(&pasta).unwrap();
        assert_eq!(upload_error(Error::Server(413)), "O som passa de 1 MB.");
        assert!(upload_error(Error::Unsupported).starts_with("Formato não aceito"));
    }

    #[test]
    fn corte_diferente_e_outro_som_no_cache() {
        let s = Sound {
            id: 1,
            name: "a".into(),
            url: "/api/sounds/1?v=a.mp3".into(),
            trim_start: 0.0,
            trim_end: None,
            uploaded_by: None,
        };
        let cortado = Sound {
            trim_end: Some(1.0),
            ..s.clone()
        };
        assert_ne!(key(&s), key(&cortado));
    }
}
