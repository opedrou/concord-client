//! Tempo real do servidor por `text/event-stream`: o laço que reconecta e o leitor dos
//! eventos. O chat do canal e as notificações usam os dois.
use std::time::{Duration, Instant};

use futures_util::{SinkExt, Stream};
use reqwest::Response;
use serde::de::DeserializeOwned;

use crate::api::Error;

const PRIMEIRA_ESPERA: Duration = Duration::from_secs(1);
const MAX_ESPERA: Duration = Duration::from_secs(30);
/// O servidor manda ping a cada 25 s: sem nada nesse tempo, a conexão morreu calada.
const SILENCIO: Duration = Duration::from_secs(60);
/// Evento maior que isso não é do Concord.
const MAX_EVENTO: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum Live<E> {
    /// Conectou ou voltou: hora de buscar o que perdeu.
    Connected,
    Event(E),
    /// Caiu. Tenta de novo sozinho, esperando cada vez mais.
    Lost,
    Unauthorized,
}

/// SSE enquanto o stream existir. `open` abre a conexão a cada volta; dos eventos, só os
/// de nome `event` que decodificam em `E` saem, e o resto é pulado sem derrubar nada.
/// Reconecta sozinho, com espera de 1 s dobrando até 30 s, e volta pra 1 s depois de uma
/// conexão que durou. `what` abre a linha do log.
pub fn live<E, F, Fut>(
    what: &'static str,
    event: &'static str,
    open: F,
) -> impl Stream<Item = Live<E>>
where
    E: DeserializeOwned + Send + 'static,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = Result<Response, Error>> + Send,
{
    iced::stream::channel(64, async move |mut out| {
        let mut wait = PRIMEIRA_ESPERA;
        loop {
            match open().await {
                Ok(mut resp) => {
                    let since = Instant::now();
                    if out.send(Live::Connected).await.is_err() {
                        return;
                    }
                    let mut sse = Sse::new(event);
                    while let Ok(Ok(Some(chunk))) =
                        tokio::time::timeout(SILENCIO, resp.chunk()).await
                    {
                        let Some(events) = sse.push(&chunk) else {
                            break;
                        };
                        for e in events.iter().filter_map(|d| serde_json::from_str(d).ok()) {
                            if out.send(Live::Event(e)).await.is_err() {
                                return;
                            }
                        }
                    }
                    if since.elapsed() > MAX_ESPERA {
                        wait = PRIMEIRA_ESPERA;
                    }
                    log!("{what}: o tempo real caiu, volta em {} s", wait.as_secs());
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
struct Sse {
    buf: Vec<u8>,
    /// O único nome de evento que sai.
    event: &'static str,
}

impl Sse {
    fn new(event: &'static str) -> Self {
        Sse {
            buf: Vec::new(),
            event,
        }
    }

    /// O `data` de cada evento completo com o nome pedido. `None`: evento grande demais.
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
            if event == self.event && !data.is_empty() {
                out.push(data.join("\n"));
            }
        }
        (self.buf.len() <= MAX_EVENTO).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ChatEvent, NotifyEvent};

    fn decode<E: DeserializeOwned>(sse: &mut Sse, chunks: &[&str]) -> Vec<E> {
        let mut dados = Vec::new();
        for c in chunks {
            dados.extend(sse.push(c.as_bytes()).unwrap());
        }
        dados
            .iter()
            .filter_map(|d| serde_json::from_str(d).ok())
            .collect()
    }

    #[test]
    fn eventos_do_chat_em_pedacos() {
        let mut sse = Sse::new("message");
        let corpo = ": connected\n\nevent: message\ndata: {\"type\":\"deleted\",\"messageId\":7,\"channelId\":2}\n\n: ping\n\nevent: message\r\ndata: {\"type\":\"created\",\"message\":{\"id\":10,\"channelId\":2,\"authorId\":3,\"authorName\":\"ana\",\"content\":\"a\\nb\",\"createdAt\":1,\"attachment\":null}}\r\n\r\nevent: mess";
        // Em pedaços que cortam no meio da linha.
        let (a, b) = corpo.split_at(70);
        let eventos: Vec<ChatEvent> = decode(&mut sse, &[a, b]);
        assert_eq!(eventos[0], ChatEvent::Deleted { message_id: 7 });
        let ChatEvent::Created { message } = &eventos[1] else {
            panic!("{eventos:?}");
        };
        assert_eq!((message.id, message.content.as_str()), (10, "a\nb"));
        assert_eq!(sse.buf, b"event: mess");
        assert!(sse.push(&vec![b'x'; MAX_EVENTO]).is_none());
    }

    #[test]
    fn notificacao_le_os_dois_tipos_e_pula_o_resto() {
        let mut sse = Sse::new("notification");
        let corpo = ": connected\n\n\
            event: notification\ndata: {\"type\":\"call\",\"caller\":\"ana\",\"channel\":\"call-tete\",\"at\":\"2026-11-18T13:00:00.000Z\"}\n\n\
            event: message\ndata: {\"type\":\"call\",\"caller\":\"intruso\",\"channel\":\"x\",\"at\":\"\"}\n\n\
            event: notification\ndata: {\"type\":\"futuro\",\"qualquer\":1}\n\n\
            event: notification\ndata: {\"type\":\"call\",\"caller\":\"sem canal\"}\n\n\
            event: notification\ndata: {\"type\":\"mention\",\"author\":\"bia\",\"channelId\":4,\"channelName\":\"geral\",\"excerpt\":\"oi @ana\",\"at\":\"2026-11-18T13:01:00.000Z\"}\n\n";
        let (a, b) = corpo.split_at(120);
        let eventos: Vec<NotifyEvent> = decode(&mut sse, &[a, b]);
        assert_eq!(
            eventos,
            [
                NotifyEvent::Call {
                    caller: "ana".into(),
                    channel: "call-tete".into(),
                    at: "2026-11-18T13:00:00.000Z".into(),
                },
                NotifyEvent::Mention {
                    author: "bia".into(),
                    channel_id: 4,
                    channel_name: "geral".into(),
                    excerpt: "oi @ana".into(),
                    at: "2026-11-18T13:01:00.000Z".into(),
                },
            ]
        );
    }
}
