//! Rotas do servidor Concord. Resposta do servidor é entrada não confiável:
//! tudo tem limite de tamanho e nada segue redirect.
use std::{collections::HashMap, fmt, time::Duration};

use reqwest::{RequestBuilder, Response, StatusCode, Url, header, redirect};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;

const MAX_JSON: usize = 2 * 1024 * 1024;
/// Mesmo teto do upload no servidor (`MAX_AVATAR_BYTES`).
const MAX_AVATAR: usize = 5 * 1024 * 1024;

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
    pub username: String,
    pub avatar_url: Option<String>,
}

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
}

#[derive(Deserialize)]
struct PresenceChannel {
    slug: String,
    participants: Vec<Participant>,
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

    pub async fn presence(&self) -> Result<Presence, Error> {
        let body: PresenceBody = self.json("/api/channels/presence").await?;
        Ok(body
            .channels
            .into_iter()
            .map(|c| (c.slug, c.participants))
            .collect())
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
        let bytes = read_limited(check(resp)?, MAX_JSON).await?;
        serde_json::from_slice(&bytes).map_err(|_| Error::NotConcord)
    }
}

fn is_avatar_path(path: &str) -> bool {
    path.starts_with("/api/avatars/") && !path.contains("..")
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
