//! Chat dos canais de texto, sem interface: a linha do tempo, o tempo real pelo SSE e os
//! anexos no disco. A tela fica em `ui::chat`.
use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use image::{ImageReader, Limits, imageops::FilterType};

use futures_util::{SinkExt, Stream};

use crate::api::{ChatEvent, Client, Error, Message, Page};

#[cfg(test)]
mod bancada;

/// Mensagens por página, como na web.
pub const PAGINA: u32 = 50;
/// Páginas que a volta da conexão busca atrás do que perdeu. Passou disso, a tela recomeça
/// da página mais nova, que é melhor que um buraco no meio.
const MAX_PAGINAS: usize = 20;
/// Mesmo limite do servidor, contado em UTF-16 como o `length` do JavaScript.
pub const MAX_TEXTO: usize = 4000;
/// Mesmo autor dentro disso fica no mesmo bloco, como na web.
const JUNTA_MS: i64 = 5 * 60 * 1000;
const PRIMEIRA_ESPERA: Duration = Duration::from_secs(1);
const MAX_ESPERA: Duration = Duration::from_secs(30);
/// O servidor manda ping a cada 25 s: sem nada nesse tempo, a conexão morreu calada.
const SILENCIO: Duration = Duration::from_secs(60);
/// Evento maior que isso não é do Concord.
const MAX_EVENTO: usize = 1024 * 1024;
/// Imagem guardada pra mostrar na mensagem: o dobro do tamanho na tela, pra telas HiDPI.
const MINIATURA: (u32, u32) = (800, 600);
const MAX_LADO_ORIGINAL: u32 = 8192;
const MAX_DECODIFICADO: u64 = 256 * 1024 * 1024;

/// O que está na tela, em ordem de id, que é a ordem de chegada no servidor.
#[derive(Debug, Default)]
pub struct Timeline {
    pub messages: Vec<Message>,
    /// Tem histórico antes da primeira.
    pub has_more: bool,
    /// Apagadas nesta sessão: resposta atrasada não traz de volta. O id nunca se repete
    /// (`AUTOINCREMENT` no servidor).
    deleted: HashSet<i64>,
}

/// O que a busca de [`recover`] trouxe: da mais nova pra trás, sem buraco.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    messages: Vec<Message>,
    has_more: bool,
    /// A mais nova da tela quando a busca saiu.
    asked: Option<i64>,
    /// Encostou no que já estava na tela.
    joined: bool,
}

impl Timeline {
    pub fn newest(&self) -> Option<i64> {
        self.messages.last().map(|m| m.id)
    }

    pub fn oldest(&self) -> Option<i64> {
        self.messages.first().map(|m| m.id)
    }

    /// `false` se já estava ou foi apagada.
    pub fn insert(&mut self, message: Message) -> bool {
        if self.deleted.contains(&message.id) {
            return false;
        }
        match self.messages.binary_search_by_key(&message.id, |m| m.id) {
            Ok(_) => false,
            Err(at) => {
                self.messages.insert(at, message);
                true
            }
        }
    }

    pub fn remove(&mut self, id: i64) {
        self.deleted.insert(id);
        self.messages.retain(|m| m.id != id);
    }

    pub fn apply(&mut self, event: ChatEvent) {
        match event {
            ChatEvent::Created { message } => {
                self.insert(message);
            }
            ChatEvent::Deleted { message_id } => self.remove(message_id),
        }
    }

    /// Página do "Carregar mais".
    pub fn older(&mut self, page: Page) {
        self.has_more = page.has_more;
        for message in page.messages {
            self.insert(message);
        }
    }

    /// Junta o que a volta da conexão trouxe. Entra o que faltava, sem repetir, e sai o que
    /// foi apagado enquanto a conexão estava fora, dentro do trecho que a busca cobriu.
    pub fn recovered(&mut self, window: Window) {
        if !window.joined {
            self.messages.clear();
        }
        let first = window.messages.first().map(|m| m.id);
        let last = window.messages.last().map(|m| m.id);
        // Sem mais histórico, a busca cobre do começo do canal.
        let low = if window.has_more {
            first
        } else {
            Some(i64::MIN)
        };
        // O que chegou depois da busca sair tem id maior que os dois.
        let high = last.max(window.asked);
        if let (Some(low), Some(high)) = (low, high) {
            let kept: HashSet<i64> = window.messages.iter().map(|m| m.id).collect();
            self.messages
                .retain(|m| m.id < low || m.id > high || kept.contains(&m.id));
        }
        if first.zip(self.oldest()).is_none_or(|(f, o)| f <= o) {
            self.has_more = window.has_more;
        }
        for message in window.messages {
            self.insert(message);
        }
    }

    /// Começa um bloco novo: outro autor, ou mais de 5 min depois da anterior.
    pub fn starts_group(&self, index: usize) -> bool {
        let Some(previous) = index.checked_sub(1).and_then(|i| self.messages.get(i)) else {
            return true;
        };
        let message = &self.messages[index];
        previous.author_id != message.author_id
            || previous.author_name != message.author_name
            || message.created_at - previous.created_at >= JUNTA_MS
    }
}

/// Busca da página mais nova pra trás até encostar em `asked`, a mais nova da tela. Sem
/// `asked` é a primeira carga: uma página.
pub async fn recover(client: &Client, channel: i64, asked: Option<i64>) -> Result<Window, Error> {
    let mut messages = Vec::new();
    let mut before = None;
    for _ in 0..MAX_PAGINAS {
        let page = client.messages(channel, before, PAGINA).await?;
        let first = page.messages.first().map(|m| m.id);
        messages.splice(0..0, page.messages);
        let joined = asked.zip(first).is_none_or(|(asked, first)| first <= asked);
        if joined || !page.has_more {
            return Ok(Window {
                messages,
                has_more: page.has_more,
                asked,
                joined: true,
            });
        }
        before = first;
    }
    Ok(Window {
        messages,
        has_more: true,
        asked,
        joined: false,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub enum Live {
    /// Conectou ou voltou: hora de buscar o que perdeu.
    Connected,
    Event(ChatEvent),
    /// Caiu. Tenta de novo sozinho, esperando cada vez mais.
    Lost,
    Unauthorized,
}

/// SSE do canal enquanto o stream existir. Reconecta sozinho, com espera de 1 s dobrando até
/// 30 s, e volta pra 1 s depois de uma conexão que durou.
pub fn live(client: Client, channel: i64) -> impl Stream<Item = Live> {
    iced::stream::channel(64, async move |mut out| {
        let mut wait = PRIMEIRA_ESPERA;
        loop {
            match client.message_stream(channel).await {
                Ok(mut resp) => {
                    let since = Instant::now();
                    if out.send(Live::Connected).await.is_err() {
                        return;
                    }
                    let mut sse = Sse::default();
                    while let Ok(Ok(Some(chunk))) =
                        tokio::time::timeout(SILENCIO, resp.chunk()).await
                    {
                        let Some(events) = sse.push(&chunk) else {
                            break;
                        };
                        for event in events.iter().filter_map(|d| serde_json::from_str(d).ok()) {
                            if out.send(Live::Event(event)).await.is_err() {
                                return;
                            }
                        }
                    }
                    if since.elapsed() > MAX_ESPERA {
                        wait = PRIMEIRA_ESPERA;
                    }
                    log!("chat: o tempo real caiu, volta em {} s", wait.as_secs());
                    if out.send(Live::Lost).await.is_err() {
                        return;
                    }
                }
                Err(Error::Unauthorized) => {
                    let _ = out.send(Live::Unauthorized).await;
                    return;
                }
                Err(_) => {}
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(MAX_ESPERA);
        }
    })
}

/// Leitor de `text/event-stream`, só o que o Concord usa: `event` e `data`.
#[derive(Default)]
struct Sse {
    buf: Vec<u8>,
}

impl Sse {
    /// O `data` de cada evento `message` completo. `None`: evento grande demais.
    fn push(&mut self, chunk: &[u8]) -> Option<Vec<String>> {
        self.buf.extend(chunk.iter().filter(|&&b| b != b'\r'));
        let mut out = Vec::new();
        while let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let block: Vec<u8> = self.buf.drain(..end + 2).collect();
            let block = String::from_utf8_lossy(&block);
            let (mut event, mut data) = ("message", Vec::new());
            for line in block.lines() {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => event = value,
                    "data" => data.push(value),
                    _ => {}
                }
            }
            if event == "message" && !data.is_empty() {
                out.push(data.join("\n"));
            }
        }
        (self.buf.len() <= MAX_EVENTO).then_some(out)
    }
}

/// Texto que o servidor aceita: sem espaço nas pontas e até 4000 em UTF-16.
pub fn fits(text: &str) -> bool {
    text.trim().encode_utf16().count() <= MAX_TEXTO
}

/// Os primeiros 4000 em UTF-16, sem partir caractere.
pub fn cut(text: &str) -> String {
    let mut used = 0;
    text.chars()
        .take_while(|c| {
            used += c.len_utf16();
            used <= MAX_TEXTO
        })
        .collect()
}

#[cfg(test)]
impl Window {
    /// Primeira carga, como `recover` devolve sem nada na tela.
    pub fn first(page: Page) -> Self {
        Self {
            messages: page.messages,
            has_more: page.has_more,
            asked: None,
            joined: true,
        }
    }
}

/// Imagem do anexo reduzida pra caber na mensagem, em RGBA. Os limites barram imagem pequena
/// em bytes e gigante na memória, como no avatar.
pub fn thumbnail(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_LADO_ORIGINAL);
    limits.max_image_height = Some(MAX_LADO_ORIGINAL);
    limits.max_alloc = Some(MAX_DECODIFICADO);
    reader.limits(limits);
    let mut picture = reader.decode().ok()?;
    let (w, h) = MINIATURA;
    if picture.width() > w || picture.height() > h {
        picture = picture.resize(w, h, FilterType::Triangle);
    }
    let rgba = picture.into_rgba8();
    let (w, h) = rgba.dimensions();
    Some((w, h, rgba.into_raw()))
}

/// Extensões que o servidor grava pelos bytes do arquivo (`lib/uploads.ts`). Fora delas vira
/// `bin`, pra um anexo nunca chegar no sistema como `.exe`, `.bat` ou `.desktop`.
const EXTENSOES: [&str; 14] = [
    "jpg", "png", "gif", "webp", "mp4", "mov", "webm", "m4a", "mp3", "ogg", "wav", "flac", "pdf",
    "zip",
];

/// Nome local do anexo: `<id>-<nome>.<extensão>`. O nome vem de quem mandou: perde a
/// extensão e fica só com letra, número, espaço e `-_()`, sem ponto e sem barra. A extensão
/// sai da `url`, que o servidor monta com o nome que ele mesmo deu ao arquivo.
pub fn local_name(id: i64, name: &str, url: &str) -> String {
    let ext = url
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| EXTENSOES.contains(&ext.as_str()))
        .unwrap_or_else(|| "bin".to_owned());
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    };
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " -_()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    let clean = clean.trim_matches([' ', '_']);
    let clean = if clean.is_empty() { "arquivo" } else { clean };
    format!("{id}-{clean}.{ext}")
}

/// Baixa o anexo pra pasta temporária, se ainda não estiver lá, e abre no app do sistema.
pub async fn open_attachment(
    client: &Client,
    id: i64,
    name: &str,
    url: &str,
) -> Result<(), String> {
    let path = download(client, id, name, url).await?;
    tokio::task::spawn_blocking(move || open_with_system(&path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|_| "Nenhum app abriu o anexo.".to_owned())
}

/// Anexo na pasta temporária, baixado uma vez.
pub async fn download(client: &Client, id: i64, name: &str, url: &str) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("concord-anexos");
    let path = dir.join(local_name(id, name, url));
    if !path.is_file() {
        let max = crate::api::MAX_ANEXO as usize + 1024 * 1024;
        let bytes = client
            .attachment(url, max)
            .await
            .map_err(|_| "Não foi possível baixar o anexo.".to_owned())?;
        let target = path.clone();
        tokio::task::spawn_blocking(move || save(&dir, &target, &bytes))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|_| "Não foi possível salvar o anexo.".to_owned())?;
    }
    Ok(path)
}

/// Grava por um `.part` ao lado: queda no meio não deixa arquivo pela metade com o nome final.
fn save(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let part = path.with_extension("part");
    std::fs::write(&part, bytes)?;
    std::fs::rename(part, path)
}

fn open_with_system(path: &Path) -> std::io::Result<()> {
    let opener = if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(path)
        .status()
        .map(drop)
}

/// Lê o arquivo escolhido pra anexar: nome e bytes, até 95 MiB.
pub fn read_attachment(path: &Path) -> Result<(String, Vec<u8>), String> {
    let size = std::fs::metadata(path)
        .map_err(|_| "Não foi possível ler o arquivo.".to_owned())?
        .len();
    if size > crate::api::MAX_ANEXO {
        return Err("O arquivo passa de 95 MB.".to_owned());
    }
    if size == 0 {
        return Err("O arquivo está vazio.".to_owned());
    }
    let bytes = std::fs::read(path).map_err(|_| "Não foi possível ler o arquivo.".to_owned())?;
    let name = path
        .file_name()
        .map_or_else(|| "arquivo".into(), |n| n.to_string_lossy().into_owned());
    Ok((name, bytes))
}

/// Seletor de arquivo do sistema. `None`: cancelou, ou não tem seletor.
pub async fn pick_file() -> Option<PathBuf> {
    tokio::task::spawn_blocking(pick).await.ok().flatten()
}

/// Pelo portal, que abre o seletor do GNOME ou do KDE sem puxar GTK pra dentro do processo.
#[cfg(target_os = "linux")]
fn pick() -> Option<PathBuf> {
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedValue, Value};
    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const OBJETO: &str = "/org/freedesktop/portal/desktop";

    let conn = zbus::blocking::Connection::session().ok()?;
    // Escuta a resposta antes de pedir: o caminho dela sai do nome da conexão e do token.
    let token = format!("concord{}", uuid::Uuid::new_v4().simple());
    let sender = conn
        .unique_name()?
        .trim_start_matches(':')
        .replace('.', "_");
    let request = format!("{OBJETO}/request/{sender}/{token}");
    let request = zbus::blocking::Proxy::new(
        &conn,
        PORTAL,
        request.as_str(),
        "org.freedesktop.portal.Request",
    )
    .ok()?;
    let mut answers = request.receive_signal("Response").ok()?;
    let options = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("modal", Value::from(true)),
    ]);
    conn.call_method(
        Some(PORTAL),
        OBJETO,
        Some("org.freedesktop.portal.FileChooser"),
        "OpenFile",
        &("", "Anexar arquivo", options),
    )
    .ok()?;
    let answer = answers.next()?;
    let (code, mut results): (u32, HashMap<String, OwnedValue>) =
        answer.body().deserialize().ok()?;
    if code != 0 {
        return None;
    }
    let uris = Vec::<String>::try_from(results.remove("uris")?).ok()?;
    file_path(uris.first()?)
}

/// `file:///pasta/Nome%20bom.png` -> `/pasta/Nome bom.png`.
#[cfg(target_os = "linux")]
fn file_path(uri: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let rest = uri.strip_prefix("file://").filter(|r| r.starts_with('/'))?;
    let mut bytes = Vec::with_capacity(rest.len());
    let mut it = rest.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex = [it.next()?, it.next()?];
            bytes.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            bytes.push(b);
        }
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(windows)]
fn pick() -> Option<PathBuf> {
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    // SAFETY: COM aberto e fechado nesta thread, que é só do seletor; as interfaces de
    // `ask_file` são soltas antes do `CoUninitialize`.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let path = ask_file();
        if init.is_ok() {
            CoUninitialize();
        }
        path.ok()
    }
}

#[cfg(windows)]
unsafe fn ask_file() -> windows::core::Result<PathBuf> {
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
    // SAFETY: chamadas COM numa thread com COM aberto; o texto do caminho é do COM e volta
    // pra ele depois de copiado.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.Show(None)?;
        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path: std::ffi::OsString =
            std::os::windows::ffi::OsStringExt::from_wide(name.as_wide());
        CoTaskMemFree(Some(name.0 as *const _));
        Ok(PathBuf::from(path))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn pick() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Attachment;

    fn msg(id: i64, author: &str, at_min: i64) -> Message {
        Message {
            id,
            author_id: Some(author.len() as i64),
            author_name: author.into(),
            content: format!("m{id}"),
            created_at: at_min * 60_000,
            attachment: None,
        }
    }

    fn ids(t: &Timeline) -> Vec<i64> {
        t.messages.iter().map(|m| m.id).collect()
    }

    fn window(ids: &[i64], has_more: bool, asked: Option<i64>, joined: bool) -> Window {
        Window {
            messages: ids.iter().map(|&id| msg(id, "ana", id)).collect(),
            has_more,
            asked,
            joined,
        }
    }

    #[test]
    fn contrato_das_mensagens_e_do_sse() {
        let page: Page = serde_json::from_str(
            r#"{"messages":[
                {"id":7,"channelId":2,"authorId":null,"authorName":"Usuário removido","content":"oi","createdAt":1700000000000,"attachment":null},
                {"id":8,"channelId":2,"authorId":3,"authorName":"ana","content":"","createdAt":1700000001000,
                 "attachment":{"url":"/api/attachments/8?v=ab.png","name":"foto.png","mime":"image/png","kind":"image","size":-3.5}},
                {"id":9,"channelId":2,"authorId":3,"authorName":"ana","content":"x","createdAt":1700000002000,
                 "attachment":{"url":"/api/attachments/9?v=ab.bin","name":"a","mime":"x","kind":"executavel","size":10}}
            ],"hasMore":true}"#,
        )
        .unwrap();
        assert!(page.has_more);
        assert_eq!(page.messages[0].author_id, None);
        let foto: &Attachment = page.messages[1].attachment.as_ref().unwrap();
        assert_eq!((foto.kind, foto.size()), (crate::api::Kind::Image, 0));
        let outro = page.messages[2].attachment.as_ref().unwrap();
        assert_eq!(outro.kind, crate::api::Kind::File);

        let mut sse = Sse::default();
        let corpo = ": connected\n\nevent: message\ndata: {\"type\":\"deleted\",\"messageId\":7,\"channelId\":2}\n\n: ping\n\nevent: message\r\ndata: {\"type\":\"created\",\"message\":{\"id\":10,\"channelId\":2,\"authorId\":3,\"authorName\":\"ana\",\"content\":\"a\\nb\",\"createdAt\":1,\"attachment\":null}}\r\n\r\nevent: mess";
        // Em pedaços que cortam no meio da linha.
        let (a, b) = corpo.split_at(70);
        let mut dados = sse.push(a.as_bytes()).unwrap();
        dados.extend(sse.push(b.as_bytes()).unwrap());
        let eventos: Vec<ChatEvent> = dados
            .iter()
            .map(|d| serde_json::from_str(d).unwrap())
            .collect();
        assert_eq!(eventos[0], ChatEvent::Deleted { message_id: 7 });
        let ChatEvent::Created { message } = &eventos[1] else {
            panic!("{eventos:?}");
        };
        assert_eq!((message.id, message.content.as_str()), (10, "a\nb"));
        assert_eq!(sse.buf, b"event: mess");
        assert!(sse.push(&vec![b'x'; MAX_EVENTO]).is_none());
    }

    #[test]
    fn mesmo_autor_em_5_min_fica_no_bloco() {
        let mut t = Timeline::default();
        for m in [
            msg(1, "ana", 0),
            msg(2, "ana", 4),
            msg(3, "ana", 9),
            msg(4, "bia", 9),
            msg(5, "ana", 10),
        ] {
            t.insert(m);
        }
        let blocos: Vec<bool> = (0..5).map(|i| t.starts_group(i)).collect();
        assert_eq!(blocos, [true, false, true, true, true]);
    }

    #[test]
    fn carregar_mais_poe_antes_sem_repetir() {
        let mut t = Timeline::default();
        t.recovered(window(&[51, 52, 53], true, None, true));
        assert!(t.has_more);
        t.older(Page {
            messages: (40..=51).map(|id| msg(id, "ana", id)).collect(),
            has_more: false,
        });
        assert_eq!(ids(&t), (40..=53).collect::<Vec<_>>());
        assert!(!t.has_more);
    }

    #[test]
    fn volta_da_conexao_traz_o_que_perdeu_sem_repetir_e_tira_o_apagado() {
        let mut t = Timeline::default();
        t.recovered(window(&[1, 2, 3, 4, 5], false, None, true));
        // Fora do ar: 4 apagada, 6 e 7 novas. No meio da busca chega a 8 pelo SSE.
        t.apply(ChatEvent::Created {
            message: msg(8, "ana", 8),
        });
        t.recovered(window(&[1, 2, 3, 5, 6, 7], false, Some(5), true));
        assert_eq!(ids(&t), [1, 2, 3, 5, 6, 7, 8]);
        // A mesma resposta duas vezes não muda nada.
        t.recovered(window(&[1, 2, 3, 5, 6, 7], false, Some(5), true));
        assert_eq!(ids(&t), [1, 2, 3, 5, 6, 7, 8]);
    }

    #[test]
    fn resposta_atrasada_nao_ressuscita_apagada() {
        let mut t = Timeline::default();
        t.recovered(window(&[10, 11], true, None, true));
        t.apply(ChatEvent::Deleted { message_id: 11 });
        t.recovered(window(&[10, 11, 12], true, Some(11), true));
        assert_eq!(ids(&t), [10, 12]);
        assert!(!t.insert(msg(11, "ana", 11)));
    }

    #[test]
    fn busca_parcial_so_mexe_no_trecho_que_cobriu() {
        let mut t = Timeline::default();
        t.recovered(window(&[100, 101, 102], true, None, true));
        t.older(Page {
            messages: vec![msg(50, "ana", 50), msg(60, "ana", 60)],
            has_more: true,
        });
        // A busca cobriu de 101 pra frente: a 60 fora do trecho fica, a 102 apagada sai.
        t.recovered(window(&[101, 103], true, Some(102), true));
        assert_eq!(ids(&t), [50, 60, 100, 101, 103]);
        assert!(t.has_more);
    }

    #[test]
    fn buraco_grande_recomeca_da_pagina_mais_nova() {
        let mut t = Timeline::default();
        t.recovered(window(&[1, 2], false, None, true));
        t.recovered(window(&[5000, 5001], true, Some(2), false));
        assert_eq!(ids(&t), [5000, 5001]);
        assert!(t.has_more);
    }

    #[test]
    fn canal_que_ficou_vazio_limpa_a_tela() {
        let mut t = Timeline::default();
        t.recovered(window(&[1, 2], false, None, true));
        t.recovered(window(&[], false, Some(2), true));
        assert!(t.messages.is_empty());
        assert!(!t.has_more);
    }

    #[test]
    fn texto_conta_como_o_javascript() {
        assert!(fits(&"a".repeat(4000)));
        assert!(fits(&format!("  {}  ", "a".repeat(4000))));
        assert!(!fits(&"a".repeat(4001)));
        // Emoji vale 2 em UTF-16.
        assert!(!fits(&"😀".repeat(2001)));
        assert!(fits(&"é".repeat(4000)));
        assert_eq!(cut(&"😀".repeat(2001)), "😀".repeat(2000));
        assert_eq!(
            cut(&format!("a{}", "😀".repeat(2000)))
                .encode_utf16()
                .count(),
            3999
        );
    }

    #[test]
    fn nome_de_arquivo_do_servidor_nao_vira_caminho_perigoso() {
        let url = "/api/attachments/9?v=0b7e.png";
        for (nome, url, esperado) in [
            ("foto.png", url, "9-foto.png"),
            ("Férias 2026 (1).jpeg", url, "9-Férias 2026 (1).png"),
            ("../../.bashrc", url, "9-arquivo.png"),
            ("..\\..\\Windows\\x.dll", url, "9-Windows_x.png"),
            (
                "relatorio.pdf.exe",
                "/api/attachments/9?v=a.bin",
                "9-relatorio_pdf.bin",
            ),
            ("gpj\u{202e}.exe", "/api/attachments/9?v=a.exe", "9-gpj.bin"),
            ("CON", "/api/attachments/9?v=a.PDF", "9-CON.pdf"),
            ("", "/api/attachments/9", "9-arquivo.bin"),
            ("a:b*c?d\"e<f>g|h\n", url, "9-a_b_c_d_e_f_g_h.png"),
        ] {
            assert_eq!(local_name(9, nome, url), esperado, "{nome:?}");
        }
        let longo = local_name(9, &"x".repeat(500), url);
        assert_eq!(longo.chars().count(), "9-".len() + 80 + ".png".len());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn uri_do_portal_vira_caminho() {
        assert_eq!(
            file_path("file:///home/a/Nome%20bom%C3%A9.png"),
            Some(PathBuf::from("/home/a/Nome bomé.png"))
        );
        assert_eq!(file_path("https://x/y"), None);
        assert_eq!(file_path("file://host/y"), None);
        assert_eq!(file_path("file:///y%2"), None);
    }

    #[test]
    fn anexo_grande_ou_vazio_nem_sobe() {
        let dir = std::env::temp_dir().join(format!("concord-teste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let vazio = dir.join("vazio.txt");
        std::fs::write(&vazio, b"").unwrap();
        assert!(read_attachment(&vazio).is_err());
        let grande = dir.join("grande.bin");
        let f = std::fs::File::create(&grande).unwrap();
        f.set_len(crate::api::MAX_ANEXO + 1).unwrap();
        assert_eq!(
            read_attachment(&grande).unwrap_err(),
            "O arquivo passa de 95 MB."
        );
        let ok = dir.join("nota.txt");
        std::fs::write(&ok, b"oi").unwrap();
        assert_eq!(
            read_attachment(&ok).unwrap(),
            ("nota.txt".into(), b"oi".to_vec())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn miniatura_cabe_e_imagem_gigante_nem_abre() {
        use image::{ImageFormat, RgbaImage};
        let png = |w, h| {
            let mut out = Vec::new();
            RgbaImage::new(w, h)
                .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
                .unwrap();
            out
        };
        assert_eq!(
            thumbnail(&png(1600, 300)).map(|t| (t.0, t.1)),
            Some((800, 150))
        );
        assert_eq!(thumbnail(&png(40, 30)).map(|t| (t.0, t.1)), Some((40, 30)));
        assert!(thumbnail(&png(9000, 2)).is_none());
        assert!(thumbnail(b"nada de imagem").is_none());
    }
}
