//! Rotas do servidor Concord. Resposta do servidor é entrada não confiável:
//! tudo tem limite de tamanho e nada segue redirect.
use std::{collections::HashMap, fmt, time::Duration};

use reqwest::{RequestBuilder, Response, StatusCode, Url, header, redirect};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;

const MAX_JSON: usize = 2 * 1024 * 1024;
/// Mesmo teto do upload no servidor (`MAX_AVATAR_BYTES`).
const MAX_AVATAR: usize = 5 * 1024 * 1024;
/// Mesmo teto do upload de som no servidor (`MAX_SOUND_BYTES`).
const MAX_SOUND: usize = 1024 * 1024;

/// Senha ou cookie. O `Debug` não mostra o valor, pra nunca cair em log.
#[derive(Clone, PartialEq)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Error {
    /// Rede fora ou tempo esgotado.
    Offline,
    /// Respondeu, mas não como um Concord responde.
    NotConcord,
    /// Sessão ausente ou vencida.
    Unauthorized,
    /// Usuário ou senha errados.
    BadCredentials,
    /// Login bloqueado por tentativas; segundos até liberar.
    TooManyAttempts(u64),
    /// O servidor recusou o formato do arquivo (`unsupported_format`).
    Unsupported,
    Server(u16),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AddressError {
    Empty,
    Invalid,
    NotHttps,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Text,
    Voice,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Channel {
    pub id: i64,
    pub name: String,
    pub slug: String,
    #[serde(rename = "type")]
    pub kind: ChannelKind,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Participant {
    pub identity: String,
    pub name: String,
    pub muted: bool,
    pub camera: bool,
    pub screen_share: bool,
}

impl Participant {
    pub fn display_name(&self) -> &str {
        if self.name.is_empty() {
            &self.identity
        } else {
            &self.name
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub id: i64,
    pub username: String,
    pub avatar_url: Option<String>,
}

/// Mensagem de canal de texto. `created_at` em ms desde 1970.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: i64,
    pub author_id: Option<i64>,
    pub author_name: String,
    pub content: String,
    pub created_at: i64,
    pub attachment: Option<Attachment>,
}

/// Nome, tipo e tamanho vêm do corpo de quem mandou, sem conferência no servidor: só a
/// `url` é gerada lá.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Attachment {
    pub url: String,
    pub name: String,
    pub kind: Kind,
    /// `f64` porque qualquer número passa no servidor: negativo ou quebrado não derruba a
    /// página inteira.
    size: f64,
}

impl Attachment {
    pub fn size(&self) -> u64 {
        self.size as u64
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Video,
    Audio,
    #[serde(other)]
    File,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    /// Em ordem cronológica.
    pub messages: Vec<Message>,
    pub has_more: bool,
}

/// Som da soundboard, na lista do servidor. O corte é em segundos, e `trim_end` `None` vai
/// até o fim do arquivo.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sound {
    pub id: i64,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub trim_start: f64,
    pub trim_end: Option<f64>,
    /// Id de quem subiu. `None` em som de conta apagada, e no que chega pela sala.
    #[serde(default)]
    pub uploaded_by: Option<i64>,
}

/// Recibo do upload. Volta inteiro no POST da mensagem.
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq)]
pub struct Uploaded {
    pub path: String,
    pub name: String,
    pub mime: String,
    pub kind: String,
    pub size: u64,
}

/// Evento do SSE do canal.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ChatEvent {
    Created {
        message: Message,
    },
    #[serde(rename_all = "camelCase")]
    Deleted {
        message_id: i64,
    },
}

/// Evento do SSE de notificações. Tipo que este cliente não conhece não decodifica, e o
/// leitor pula.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NotifyEvent {
    /// Alguém chamou pra call do canal `channel` (slug). `at` em ISO 8601.
    Call {
        caller: String,
        channel: String,
        at: String,
    },
    /// Alguém marcou com `@` no canal de texto `channel_id`.
    #[serde(rename_all = "camelCase")]
    Mention {
        author: String,
        channel_id: i64,
        channel_name: String,
        excerpt: String,
        at: String,
    },
}

/// Mesmo teto do servidor (`MAX_ATTACHMENT_BYTES`), abaixo dos 100 MB do Cloudflare.
pub const MAX_ANEXO: u64 = 95 * 1024 * 1024;
/// Upload de 95 MiB numa conexão lenta.
const ENVIO: Duration = Duration::from_secs(30 * 60);
/// O SSE fica aberto: quem vê se morreu é o leitor, pelo ping de 25 s.
const ABERTO: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDetails {
    pub server_url: String,
    pub participant_token: String,
}

impl fmt::Debug for ConnectionDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionDetails")
            .field("server_url", &self.server_url)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct PresenceBody {
    channels: Vec<PresenceChannel>,
    /// Servidor velho não manda: todo mundo fora de voz fica offline.
    #[serde(default)]
    online: Vec<String>,
}

#[derive(Deserialize)]
struct PresenceChannel {
    slug: String,
    participants: Vec<Participant>,
}

impl PresenceBody {
    fn split(self) -> (Presence, Vec<String>) {
        let channels = self
            .channels
            .into_iter()
            .map(|c| (c.slug, c.participants))
            .collect();
        (channels, self.online)
    }
}

/// Slug do canal -> quem está nele.
pub type Presence = HashMap<String, Vec<Participant>>;

/// Sessão fantasma não vira nome repetido, e a lista não pula de lugar.
pub fn occupants(list: &[Participant]) -> Vec<&Participant> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<_> = list
        .iter()
        .filter(|p| seen.insert(p.display_name()))
        .collect();
    out.sort_by_cached_key(|p| p.display_name().to_lowercase());
    out
}

/// Reduz o que a pessoa digitou à origem do servidor. Exige `https://`, exceto
/// em `localhost`, porque senha e cookie passam por esse endereço.
pub fn normalize_server(input: &str) -> Result<String, AddressError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(AddressError::Empty);
    }
    let invalid = |_| AddressError::Invalid;
    let url = if input.contains("://") {
        Url::parse(input).map_err(invalid)?
    } else {
        let https = Url::parse(&format!("https://{input}")).map_err(invalid)?;
        if https.host_str().is_some_and(is_loopback) {
            Url::parse(&format!("http://{input}")).map_err(invalid)?
        } else {
            https
        }
    };
    let host = url.host_str().ok_or(AddressError::Invalid)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AddressError::Invalid);
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback(host) => {}
        _ => return Err(AddressError::NotHttps),
    }
    Ok(url.origin().ascii_serialization())
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    cookie: Option<Secret>,
}

impl Client {
    /// `base` vem de [`normalize_server`].
    pub fn new(base: &str, cookie: Option<Secret>) -> Self {
        let http = reqwest::Client::builder()
            .redirect(redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("concord-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("cliente HTTP");
        Self {
            http,
            base: base.to_owned(),
            cookie,
        }
    }

    fn authed(&self, req: RequestBuilder) -> RequestBuilder {
        match &self.cookie {
            Some(c) => req.header(header::COOKIE, &c.0),
            None => req,
        }
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.authed(self.http.get(format!("{}{path}", self.base)))
    }

    /// Valida endereço e sessão. Servidor Concord sem sessão responde 401
    /// `not_authenticated`, que é o que separa ele de um site qualquer.
    pub async fn me(&self) -> Result<User, Error> {
        let resp = self
            .get("/api/auth/me")
            .send()
            .await
            .map_err(|_| Error::Offline)?;
        let status = resp.status();
        classify_me(status, &read_limited(resp, MAX_JSON).await?)
    }

    /// Devolve o cookie de sessão. A senha sai daqui direto pro servidor.
    pub async fn login(&self, username: &str, password: &Secret) -> Result<Secret, Error> {
        let resp = self
            .http
            .post(format!("{}/api/auth/login", self.base))
            .json(&json!({ "username": username, "password": password.0 }))
            .send()
            .await
            .map_err(|_| Error::Offline)?;
        match resp.status() {
            StatusCode::OK => session_cookie(resp.headers()).ok_or(Error::NotConcord),
            StatusCode::UNAUTHORIZED => Err(Error::BadCredentials),
            StatusCode::TOO_MANY_REQUESTS => {
                Err(Error::TooManyAttempts(retry_after(resp.headers())))
            }
            s => Err(Error::Server(s.as_u16())),
        }
    }

    pub async fn logout(&self) -> Result<(), Error> {
        let url = format!("{}/api/auth/logout", self.base);
        self.authed(self.http.post(url))
            .send()
            .await
            .map_err(|_| Error::Offline)?;
        Ok(())
    }

    pub async fn channels(&self) -> Result<Vec<Channel>, Error> {
        self.json("/api/channels").await
    }

    /// Token da sala, válido por 5 min. `tab` vira o sufixo da identity: fixo por
    /// instalação, pra sessão antiga cair sozinha ao reconectar.
    pub async fn connection_details(
        &self,
        room: &str,
        tab: &str,
    ) -> Result<ConnectionDetails, Error> {
        let req = self
            .get("/api/connection-details")
            .query(&[("roomName", room), ("tabSessionId", tab)]);
        let resp = req.send().await.map_err(|_| Error::Offline)?;
        let bytes = read_limited(check(resp)?, MAX_JSON).await?;
        serde_json::from_slice(&bytes).map_err(|_| Error::NotConcord)
    }

    /// Quem está em cada canal de voz e os usernames online.
    pub async fn presence(&self) -> Result<(Presence, Vec<String>), Error> {
        let body: PresenceBody = self.json("/api/channels/presence").await?;
        Ok(body.split())
    }

    pub async fn members(&self) -> Result<Vec<Member>, Error> {
        self.json("/api/members").await
    }

    /// `path` vem do servidor: só caminho de avatar passa, senão o cookie
    /// poderia ser mandado pra outro lugar.
    pub async fn avatar(&self, path: &str) -> Result<Vec<u8>, Error> {
        if !is_avatar_path(path) {
            return Err(Error::NotConcord);
        }
        let resp = self.get(path).send().await.map_err(|_| Error::Offline)?;
        read_limited(check(resp)?, MAX_AVATAR).await
    }

    async fn json<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        let resp = self.get(path).send().await.map_err(|_| Error::Offline)?;
        parse(resp).await
    }

    /// Página do histórico, em ordem cronológica. `before` é o id da mais antiga já na tela.
    pub async fn messages(
        &self,
        channel: i64,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Page, Error> {
        let mut req = self
            .get(&format!("/api/channels/{channel}/messages"))
            .query(&[("limit", limit)]);
        if let Some(before) = before {
            req = req.query(&[("before", before)]);
        }
        parse(req.send().await.map_err(|_| Error::Offline)?).await
    }

    pub async fn send_message(
        &self,
        channel: i64,
        content: &str,
        attachment: Option<&Uploaded>,
    ) -> Result<Message, Error> {
        let url = format!("{}/api/channels/{channel}/messages", self.base);
        let req = self
            .authed(self.http.post(url))
            .json(&json!({ "content": content, "attachment": attachment }));
        parse(req.send().await.map_err(|_| Error::Offline)?).await
    }

    /// 403 quando não é a própria mensagem nem admin.
    pub async fn delete_message(&self, channel: i64, id: i64) -> Result<(), Error> {
        let url = format!("{}/api/channels/{channel}/messages/{id}", self.base);
        let resp = self.authed(self.http.delete(url)).send().await;
        check(resp.map_err(|_| Error::Offline)?).map(drop)
    }

    /// Corpo bruto, como a web: o servidor grava em stream. `sent` recebe quantos bytes já
    /// foram entregues pra rede, e o total. 413 é arquivo grande demais.
    pub async fn upload(
        &self,
        name: &str,
        bytes: Vec<u8>,
        mut sent: impl FnMut(u64, u64) + Send + Sync + 'static,
    ) -> Result<Uploaded, Error> {
        const PEDACO: usize = 256 * 1024;
        let total = bytes.len();
        let chunks = futures_util::stream::iter((0..total).step_by(PEDACO).map(move |at| {
            let end = (at + PEDACO).min(total);
            sent(end as u64, total as u64);
            Ok::<_, std::io::Error>(bytes[at..end].to_vec())
        }));
        let url = format!("{}/api/attachments", self.base);
        let req = self
            .authed(self.http.post(url))
            .query(&[("name", name)])
            .header(header::CONTENT_LENGTH, total)
            .timeout(ENVIO)
            .body(reqwest::Body::wrap_stream(chunks));
        parse(req.send().await.map_err(|_| Error::Offline)?).await
    }

    /// Bytes de um anexo, até `max`. `url` vem do servidor: só caminho de anexo passa.
    pub async fn attachment(&self, url: &str, max: usize) -> Result<Vec<u8>, Error> {
        if !is_server_path(url, "/api/attachments/") {
            return Err(Error::NotConcord);
        }
        let resp = self.get(url).timeout(ENVIO).send().await;
        read_limited(check(resp.map_err(|_| Error::Offline)?)?, max).await
    }

    /// Sons da soundboard, em ordem de nome.
    pub async fn sounds(&self) -> Result<Vec<Sound>, Error> {
        self.json("/api/sounds").await
    }

    /// Bytes de um som. `url` vem de outro participante, pela sala: só caminho de som passa,
    /// senão o cookie iria pra onde ele quisesse.
    pub async fn sound(&self, url: &str) -> Result<Vec<u8>, Error> {
        if !is_server_path(url, "/api/sounds/") {
            return Err(Error::NotConcord);
        }
        let resp = self.get(url).send().await.map_err(|_| Error::Offline)?;
        read_limited(check(resp)?, MAX_SOUND).await
    }

    /// Sobe um som com o corpo cru, como o site. `name` é o nome do botão: o servidor limpa
    /// e corta em 40 caracteres.
    pub async fn upload_sound(&self, name: &str, bytes: Vec<u8>) -> Result<Sound, Error> {
        let resp = self.sound_upload(name, bytes).send().await;
        let resp = resp.map_err(|_| Error::Offline)?;
        let status = resp.status();
        classify_sound(status, &read_limited(resp, MAX_JSON).await?)
    }

    fn sound_upload(&self, name: &str, bytes: Vec<u8>) -> RequestBuilder {
        let url = format!("{}/api/sounds", self.base);
        self.authed(self.http.post(url))
            .query(&[("name", name)])
            .header(header::CONTENT_LENGTH, bytes.len())
            .body(bytes)
    }

    /// 403 quando não foi quem subiu nem admin.
    pub async fn delete_sound(&self, id: i64) -> Result<(), Error> {
        let url = format!("{}/api/sounds/{id}", self.base);
        let resp = self.authed(self.http.delete(url)).send().await;
        check(resp.map_err(|_| Error::Offline)?).map(drop)
    }

    /// SSE do canal, já com a resposta 200: a partir daqui o servidor entrega os eventos.
    pub async fn message_stream(&self, channel: i64) -> Result<Response, Error> {
        self.event_stream(&format!("/api/channels/{channel}/messages/stream"))
            .await
    }

    /// SSE das notificações de quem está logado: chamado pra call e `@`.
    pub async fn notification_stream(&self) -> Result<Response, Error> {
        self.event_stream("/api/notifications/stream").await
    }

    /// Chama pessoas pra call do canal: o servidor avisa cada uma no app dela. Devolve
    /// quantas foram chamadas e quantas estavam com o app conectado. 429 é o limite de
    /// chamadas, com o tempo de espera; 400 é lista ou canal que não existe mais.
    pub async fn call_people(
        &self,
        user_ids: &[i64],
        channel_slug: &str,
    ) -> Result<(u32, u32), Error> {
        let url = format!("{}/api/call-people", self.base);
        let req = self
            .authed(self.http.post(url))
            .json(&json!({ "userIds": user_ids, "channelSlug": channel_slug }));
        let resp = req.send().await.map_err(|_| Error::Offline)?;
        let (status, wait) = (resp.status(), retry_after(resp.headers()));
        classify_call(status, wait, &read_limited(resp, MAX_JSON).await?)
    }

    async fn event_stream(&self, path: &str) -> Result<Response, Error> {
        let req = self
            .get(path)
            .header(header::ACCEPT, "text/event-stream")
            .timeout(ABERTO);
        let resp = tokio::time::timeout(Duration::from_secs(15), req.send()).await;
        check(
            resp.map_err(|_| Error::Offline)?
                .map_err(|_| Error::Offline)?,
        )
    }
}

async fn parse<T: DeserializeOwned>(resp: Response) -> Result<T, Error> {
    let bytes = read_limited(check(resp)?, MAX_JSON).await?;
    serde_json::from_slice(&bytes).map_err(|_| Error::NotConcord)
}

fn is_avatar_path(path: &str) -> bool {
    is_server_path(path, "/api/avatars/")
}

/// Caminho que vem do servidor e leva o cookie: tem que ficar nele.
fn is_server_path(path: &str, prefix: &str) -> bool {
    path.starts_with(prefix) && !path.contains("..")
}

fn check(resp: Response) -> Result<Response, Error> {
    match resp.status() {
        s if s.is_success() => Ok(resp),
        StatusCode::UNAUTHORIZED => Err(Error::Unauthorized),
        s => Err(Error::Server(s.as_u16())),
    }
}

fn classify_me(status: StatusCode, body: &[u8]) -> Result<User, Error> {
    match status {
        StatusCode::OK => serde_json::from_slice(body).map_err(|_| Error::NotConcord),
        StatusCode::UNAUTHORIZED => {
            #[derive(Deserialize)]
            struct Body {
                error: String,
            }
            match serde_json::from_slice::<Body>(body) {
                Ok(b) if b.error == "not_authenticated" => Err(Error::Unauthorized),
                _ => Err(Error::NotConcord),
            }
        }
        s if s.is_server_error() => Err(Error::Server(s.as_u16())),
        _ => Err(Error::NotConcord),
    }
}

/// `call-people`: 200 traz quantos foram chamados e quantos tinham o app conectado; 429
/// traz a espera do `Retry-After`. 400 (`invalid_body`, `unknown_user`, `unknown_channel`)
/// fica `Server(400)`: a lista da tela está velha, e buscar de novo resolve.
fn classify_call(status: StatusCode, wait: u64, body: &[u8]) -> Result<(u32, u32), Error> {
    #[derive(Deserialize)]
    struct Body {
        called: u32,
        delivered: u32,
    }
    match status {
        StatusCode::OK => serde_json::from_slice::<Body>(body)
            .map(|b| (b.called, b.delivered))
            .map_err(|_| Error::NotConcord),
        StatusCode::TOO_MANY_REQUESTS => Err(Error::TooManyAttempts(wait)),
        StatusCode::UNAUTHORIZED => Err(Error::Unauthorized),
        s => Err(Error::Server(s.as_u16())),
    }
}

/// Upload de som: 201 traz o som; 413 é arquivo grande demais, e 400 separa formato recusado
/// (`unsupported_format`) de corpo vazio (`invalid_body`).
fn classify_sound(status: StatusCode, body: &[u8]) -> Result<Sound, Error> {
    #[derive(Deserialize)]
    struct Body {
        error: String,
    }
    match status {
        s if s.is_success() => serde_json::from_slice(body).map_err(|_| Error::NotConcord),
        StatusCode::BAD_REQUEST => match serde_json::from_slice::<Body>(body) {
            Ok(b) if b.error == "unsupported_format" => Err(Error::Unsupported),
            _ => Err(Error::Server(400)),
        },
        StatusCode::UNAUTHORIZED => Err(Error::Unauthorized),
        s => Err(Error::Server(s.as_u16())),
    }
}

fn session_cookie(headers: &header::HeaderMap) -> Option<Secret> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok()?.split(';').next())
        .map(str::trim)
        .find(|c| c.starts_with("session="))
        .map(|c| Secret(c.to_owned()))
}

fn retry_after(headers: &header::HeaderMap) -> u64 {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok()?.parse().ok())
        .unwrap_or(60)
}

async fn read_limited(mut resp: Response, max: usize) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|_| Error::Offline)? {
        if out.len() + chunk.len() > max {
            return Err(Error::NotConcord);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn presenca_le_online_e_servidor_velho_sem_o_campo() {
        let com = r#"{"channels":[{"slug":"call","participants":[]}],"online":["Ana","Zé"]}"#;
        let (canais, online) = serde_json::from_str::<super::PresenceBody>(com)
            .unwrap()
            .split();
        assert!(canais.contains_key("call"));
        assert_eq!(online, ["Ana", "Zé"]);
        let velho = r#"{"channels":[]}"#;
        let (_, online) = serde_json::from_str::<super::PresenceBody>(velho)
            .unwrap()
            .split();
        assert!(online.is_empty());
    }

    use super::*;

    #[test]
    fn endereco_normaliza_pra_origem() {
        for (entrada, esperado) in [
            ("https://call.exemplo.com", "https://call.exemplo.com"),
            ("  call.exemplo.com/  ", "https://call.exemplo.com"),
            (
                "https://call.exemplo.com/rooms/call?x=1#y",
                "https://call.exemplo.com",
            ),
            (
                "https://CALL.exemplo.com:8443",
                "https://call.exemplo.com:8443",
            ),
            ("localhost:3001", "http://localhost:3001"),
            ("http://localhost:3001/", "http://localhost:3001"),
            ("127.0.0.1:3000", "http://127.0.0.1:3000"),
            ("http://[::1]:3000", "http://[::1]:3000"),
        ] {
            assert_eq!(
                normalize_server(entrada).as_deref(),
                Ok(esperado),
                "{entrada}"
            );
        }
    }

    #[test]
    fn endereco_sem_https_e_recusado() {
        for entrada in [
            "http://call.exemplo.com",
            "http://localhost.exemplo.com",
            "http://127.0.0.1.exemplo.com",
            "ftp://call.exemplo.com",
        ] {
            assert_eq!(
                normalize_server(entrada),
                Err(AddressError::NotHttps),
                "{entrada}"
            );
        }
        assert_eq!(normalize_server("   "), Err(AddressError::Empty));
        assert_eq!(
            normalize_server("https://user:senha@a.com"),
            Err(AddressError::Invalid)
        );
        assert_eq!(normalize_server("https://"), Err(AddressError::Invalid));
    }

    #[test]
    fn me_separa_concord_de_site_qualquer() {
        let ok = br#"{"id":1,"username":"a","isAdmin":false,"avatarUrl":null,"avatarColor":null}"#;
        assert_eq!(classify_me(StatusCode::OK, ok).unwrap().username, "a");
        assert_eq!(
            classify_me(
                StatusCode::UNAUTHORIZED,
                br#"{"error":"not_authenticated"}"#
            ),
            Err(Error::Unauthorized)
        );
        assert_eq!(
            classify_me(StatusCode::UNAUTHORIZED, b"<html>"),
            Err(Error::NotConcord)
        );
        assert_eq!(
            classify_me(StatusCode::OK, b"<html>"),
            Err(Error::NotConcord)
        );
        assert_eq!(
            classify_me(StatusCode::NOT_FOUND, b""),
            Err(Error::NotConcord)
        );
        assert_eq!(
            classify_me(StatusCode::MOVED_PERMANENTLY, b""),
            Err(Error::NotConcord)
        );
        assert_eq!(
            classify_me(StatusCode::BAD_GATEWAY, b""),
            Err(Error::Server(502))
        );
    }

    #[test]
    fn cookie_de_sessao_sai_do_set_cookie() {
        let mut h = header::HeaderMap::new();
        h.append(header::SET_COOKIE, "outro=1; Path=/".parse().unwrap());
        h.append(
            header::SET_COOKIE,
            "session=abc.def; HttpOnly; Path=/".parse().unwrap(),
        );
        assert_eq!(session_cookie(&h), Some(Secret("session=abc.def".into())));
        assert_eq!(session_cookie(&header::HeaderMap::new()), None);
    }

    #[test]
    fn avatar_nunca_sai_do_servidor() {
        assert!(is_avatar_path("/api/avatars/3?v=abc.png"));
        assert!(!is_avatar_path("https://evil.com/api/avatars/3"));
        assert!(!is_avatar_path("//evil.com/api/avatars/3"));
        assert!(!is_avatar_path("/api/avatars/../auth/me"));
        assert!(is_server_path(
            "/api/attachments/8?v=a.png",
            "/api/attachments/"
        ));
        assert!(!is_server_path(
            "https://evil.com/api/attachments/8",
            "/api/attachments/"
        ));
        assert!(!is_server_path(
            "/api/attachments/../auth/me",
            "/api/attachments/"
        ));
    }

    #[test]
    fn som_le_a_lista_do_site_e_nunca_sai_do_servidor() {
        let lista: Vec<Sound> = serde_json::from_str(
            r#"[{"id":3,"name":"buzina","url":"/api/sounds/3?v=ab.mp3","size":9000,
                "uploadedBy":null,"createdAt":"2026-11-20","trimStart":0.5,"trimEnd":null},
               {"id":4,"name":"x","url":"/api/sounds/4?v=c.ogg","size":1,"uploadedBy":2,
                "createdAt":"2026-11-21","trimStart":0,"trimEnd":1.25}]"#,
        )
        .unwrap();
        assert_eq!((lista[0].trim_start, lista[0].trim_end), (0.5, None));
        assert_eq!(lista[1].trim_end, Some(1.25));
        assert_eq!(
            (lista[0].uploaded_by, lista[1].uploaded_by),
            (None, Some(2))
        );
        let pasta = "/api/sounds/";
        assert!(is_server_path("/api/sounds/3?v=ab.mp3", pasta));
        assert!(!is_server_path("https://evil.com/api/sounds/3", pasta));
        assert!(!is_server_path("//evil.com/api/sounds/3", pasta));
        assert!(!is_server_path("/api/avatars/3?v=a.png", pasta));
        assert!(!is_server_path("/api/sounds/../auth/me", pasta));
    }

    #[test]
    fn subir_som_le_as_respostas_do_site() {
        let ok = br#"{"id":9,"name":"buzina","url":"/api/sounds/9?v=a.mp3","size":10,
            "uploadedBy":5,"createdAt":1,"trimStart":0,"trimEnd":null}"#;
        let som = classify_sound(StatusCode::CREATED, ok).unwrap();
        assert_eq!((som.id, som.uploaded_by), (9, Some(5)));
        let erro = |status, body: &[u8]| classify_sound(status, body).unwrap_err();
        assert_eq!(
            erro(
                StatusCode::PAYLOAD_TOO_LARGE,
                br#"{"error":"file_too_large"}"#
            ),
            Error::Server(413)
        );
        assert_eq!(
            erro(
                StatusCode::BAD_REQUEST,
                br#"{"error":"unsupported_format"}"#
            ),
            Error::Unsupported
        );
        assert_eq!(
            erro(StatusCode::BAD_REQUEST, br#"{"error":"invalid_body"}"#),
            Error::Server(400)
        );
        assert_eq!(erro(StatusCode::UNAUTHORIZED, b""), Error::Unauthorized);
        assert_eq!(erro(StatusCode::CREATED, b"<html>"), Error::NotConcord);
    }

    #[test]
    fn subir_som_manda_o_nome_na_query_e_o_corpo_cru() {
        let c = Client::new("http://localhost:3000", None);
        let req = c
            .sound_upload("Buzina alta & cia", vec![1, 2, 3])
            .build()
            .unwrap();
        assert_eq!(req.url().path(), "/api/sounds");
        let nome: Vec<_> = req.url().query_pairs().collect();
        assert_eq!(nome, [("name".into(), "Buzina alta & cia".into())]);
        assert_eq!(
            req.body().and_then(|b| b.as_bytes()),
            Some(&[1u8, 2, 3][..])
        );
    }

    #[test]
    fn chamar_pessoas_le_entregues_e_espera() {
        let ok = br#"{"ok":true,"called":3,"delivered":1}"#;
        assert_eq!(classify_call(StatusCode::OK, 60, ok), Ok((3, 1)));
        assert_eq!(
            classify_call(StatusCode::OK, 60, br#"{"ok":true,"called":3}"#),
            Err(Error::NotConcord)
        );
        let limite = br#"{"error":"rate_limited"}"#;
        assert_eq!(
            classify_call(StatusCode::TOO_MANY_REQUESTS, 1800, limite),
            Err(Error::TooManyAttempts(1800))
        );
        assert_eq!(
            classify_call(StatusCode::BAD_REQUEST, 60, br#"{"error":"unknown_user"}"#),
            Err(Error::Server(400))
        );
        assert_eq!(
            classify_call(StatusCode::UNAUTHORIZED, 60, b""),
            Err(Error::Unauthorized)
        );
        let mut h = header::HeaderMap::new();
        assert_eq!(retry_after(&h), 60, "sem Retry-After");
        h.insert(header::RETRY_AFTER, "3540".parse().unwrap());
        assert_eq!(retry_after(&h), 3540);
    }

    #[test]
    fn membro_traz_o_id() {
        let m: Vec<Member> = serde_json::from_str(
            r##"[{"id":5,"username":"ana","avatarUrl":null,"avatarColor":"#9d4edd"}]"##,
        )
        .unwrap();
        assert_eq!((m[0].id, m[0].username.as_str()), (5, "ana"));
    }

    #[test]
    fn segredo_nao_aparece_no_debug() {
        assert_eq!(format!("{:?}", Secret("session=abc".into())), "Secret(***)");
    }

    fn p(identity: &str, name: &str) -> Participant {
        Participant {
            identity: identity.into(),
            name: name.into(),
            muted: false,
            camera: false,
            screen_share: false,
        }
    }

    #[test]
    fn ocupantes_sem_nome_repetido_e_em_ordem() {
        let lista = [
            p("b__1", "bia"),
            p("b__2", "bia"),
            p("A__3", "Ana"),
            p("zeca__4", ""),
        ];
        let nomes: Vec<_> = occupants(&lista).iter().map(|p| p.display_name()).collect();
        assert_eq!(nomes, ["Ana", "bia", "zeca__4"]);
    }

    /// Contra o servidor de verdade:
    /// `CONCORD_URL=... CONCORD_USER=... CONCORD_PASS=... cargo test -- --ignored servidor`
    #[tokio::test]
    #[ignore]
    async fn servidor_real_responde_o_contrato() {
        let var = |n| std::env::var(n).unwrap();
        let base = normalize_server(&var("CONCORD_URL")).unwrap();

        let anonimo = Client::new(&base, None);
        assert_eq!(anonimo.me().await, Err(Error::Unauthorized));
        assert_eq!(
            anonimo
                .login(&var("CONCORD_USER"), &Secret("senha-errada".into()))
                .await,
            Err(Error::BadCredentials)
        );

        let cookie = anonimo
            .login(&var("CONCORD_USER"), &Secret(var("CONCORD_PASS")))
            .await
            .unwrap();
        let logado = Client::new(&base, Some(cookie));
        assert_eq!(logado.me().await.unwrap().username, var("CONCORD_USER"));
        assert!(!logado.channels().await.unwrap().is_empty());
        logado.presence().await.unwrap();
        assert!(!logado.members().await.unwrap().is_empty());
        logado.logout().await.unwrap();
    }
}
