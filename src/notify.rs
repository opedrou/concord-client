//! Notificação nativa do sistema e o tempo real que diz o que notificar: chamado pra call e
//! `@`. O som é do próprio app (`audio::play_once`), então a notificação vai muda.
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::Stream;
use notify_rust::{Notification, NotificationResponse};

use crate::api::{Channel, Client, NotifyEvent};
use crate::sse::Live;

/// Notificações esperando clique ao mesmo tempo. Cada uma segura uma thread até o clique ou o
/// fechamento, e o GNOME guarda as antigas na lista de notificações sem avisar que fecharam.
/// ponytail: passou do teto, a notificação aparece mas o clique não abre nada; fechar as
/// antigas pelo id se alguém sentir falta.
const MAX_ESPERANDO: usize = 8;
static ESPERANDO: AtomicUsize = AtomicUsize::new(0);

/// Tempo real das notificações de quem está logado; ver [`crate::sse::live`].
pub fn live(client: Client) -> impl Stream<Item = Live<NotifyEvent>> {
    crate::sse::live("notificações", "notification", move || {
        let client = client.clone();
        async move { client.notification_stream().await }
    })
}

/// Por que não mostrar: a pessoa já está vendo. `call` é o slug da call em que ela está;
/// `chat`, o canal de texto aberto na janela com foco.
pub fn skip(event: &NotifyEvent, call: Option<&str>, chat: Option<i64>) -> Option<&'static str> {
    match event {
        NotifyEvent::Call { channel, .. } if call == Some(channel.as_str()) => {
            Some("já está nessa call")
        }
        NotifyEvent::Mention { channel_id, .. } if chat == Some(*channel_id) => {
            Some("o chat está aberto na tela")
        }
        _ => None,
    }
}

/// Título e corpo. O chamado traz o slug: o nome vem da lista de canais, ou fica o slug.
pub fn text(event: &NotifyEvent, channels: &[Channel]) -> (String, String) {
    match event {
        NotifyEvent::Call {
            caller, channel, ..
        } => {
            let name = channels
                .iter()
                .find(|c| &c.slug == channel)
                .map_or(channel, |c| &c.name);
            (format!("{caller} te chamou"), format!("Canal {name}"))
        }
        NotifyEvent::Mention {
            author,
            channel_name,
            excerpt,
            ..
        } => (
            format!("{author} te marcou em #{channel_name}"),
            excerpt.clone(),
        ),
    }
}

/// Botão "Agora não" do chamado: fecha sem entrar.
const AGORA_NAO: &str = "agora-nao";

/// Mostra a notificação e espera ela fechar. `true`: a pessoa clicou nela, ou em "Entrar".
/// `call` põe os botões "Entrar" e "Agora não". Sem servidor de notificação no sistema,
/// `false` e uma linha no log.
pub async fn show(title: String, body: String, call: bool) -> bool {
    // Thread solta, não `spawn_blocking`: o runtime espera as tarefas bloqueantes ao fechar,
    // e um aviso que ninguém fecha (o GNOME guarda na lista) seguraria o Sair.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("concord-aviso".into())
        .spawn(move || {
            let _ = tx.send(show_blocking(&title, &body, call));
        });
    spawned.is_ok() && rx.await.unwrap_or(false)
}

/// Clique no corpo ou num botão que não seja o "Agora não".
fn accepted(r: &NotificationResponse) -> bool {
    match r {
        NotificationResponse::Default => true,
        NotificationResponse::Action(action) => action != AGORA_NAO,
        _ => false,
    }
}

fn show_blocking(title: &str, body: &str, call: bool) -> bool {
    let mut n = Notification::new();
    n.summary(title).body(body);
    // O nome e o ícone vêm do `.desktop` que a instalação põe no menu.
    #[cfg(target_os = "linux")]
    n.appname("Concord")
        .hint(notify_rust::Hint::DesktopEntry(
            crate::update::APP_ID.into(),
        ))
        .hint(notify_rust::Hint::SuppressSound(true))
        .action("default", "Abrir");
    // Sem `sound_name`, o toast sai com `<audio silent="true"/>`. O AUMID é o do atalho do
    // Menu Iniciar, senão o toast sai sem o nome e o ícone do Concord.
    #[cfg(windows)]
    n.app_id(crate::update::win::AUMID);
    // Ações no Linux; no Windows o notify-rust vira cada uma num botão do toast.
    if call {
        n.action("entrar", "Entrar").action(AGORA_NAO, "Agora não");
    }
    let handle = match n.show() {
        Ok(handle) => handle,
        Err(e) => {
            log!("notificação: não abriu: {e}");
            return false;
        }
    };
    if ESPERANDO.fetch_add(1, Ordering::Relaxed) >= MAX_ESPERANDO {
        ESPERANDO.fetch_sub(1, Ordering::Relaxed);
        return false;
    }
    let _esperando = Esperando;
    // O `wait_for_action` do Windows dá clique no corpo como "__closed": o `wait_for_response`
    // separa os dois nos dois sistemas.
    let mut clicked = false;
    let _ = handle.wait_for_response(|r: &NotificationResponse| clicked = accepted(r));
    clicked
}

/// Solta a vaga do [`ESPERANDO`] mesmo se a espera entrar em pânico.
struct Esperando;

impl Drop for Esperando {
    fn drop(&mut self) {
        ESPERANDO.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod texto {
    use super::*;
    use crate::api::ChannelKind;

    fn chamado(channel: &str) -> NotifyEvent {
        NotifyEvent::Call {
            caller: "ana".into(),
            channel: channel.into(),
            at: String::new(),
        }
    }

    fn mencao(channel_id: i64) -> NotifyEvent {
        NotifyEvent::Mention {
            author: "bia".into(),
            channel_id,
            channel_name: "geral".into(),
            excerpt: "oi @ana, bora?".into(),
            at: String::new(),
        }
    }

    #[test]
    fn titulo_e_corpo_com_o_nome_do_canal() {
        let canais = [Channel {
            id: 2,
            name: "Call Tetê".into(),
            slug: "call-tete".into(),
            kind: ChannelKind::Voice,
        }];
        assert_eq!(
            text(&chamado("call-tete"), &canais),
            ("ana te chamou".into(), "Canal Call Tetê".into())
        );
        assert_eq!(
            text(&chamado("sumiu"), &canais).1,
            "Canal sumiu",
            "canal fora da lista fica com o slug"
        );
        assert_eq!(
            text(&mencao(4), &canais),
            ("bia te marcou em #geral".into(), "oi @ana, bora?".into())
        );
    }

    #[test]
    fn agora_nao_fecha_sem_entrar() {
        assert!(accepted(&NotificationResponse::Default), "clique no corpo");
        assert!(accepted(&NotificationResponse::Action("entrar".into())));
        assert!(!accepted(&NotificationResponse::Action(AGORA_NAO.into())));
        assert!(!accepted(&NotificationResponse::Closed(
            notify_rust::CloseReason::Dismissed
        )));
    }

    #[test]
    fn pula_so_o_que_a_pessoa_ja_esta_vendo() {
        assert!(skip(&chamado("call-tete"), Some("call-tete"), None).is_some());
        assert!(skip(&chamado("call-tete"), Some("outra"), Some(4)).is_none());
        assert!(skip(&chamado("call-tete"), None, None).is_none());
        assert!(skip(&mencao(4), Some("call-tete"), Some(4)).is_some());
        assert!(skip(&mencao(4), None, Some(5)).is_none());
        assert!(skip(&mencao(4), None, None).is_none());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use tokio::sync::mpsc::UnboundedSender;
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::OwnedValue;

    use super::*;

    /// Servidor de notificação falso: guarda o pedido e responde com `resposta` 300 ms depois,
    /// clique (`ActionInvoked`) ou fechamento (`NotificationClosed`).
    struct Servidor {
        pedidos: UnboundedSender<(String, Vec<String>, HashMap<String, OwnedValue>)>,
        resposta: Option<&'static str>,
    }

    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl Servidor {
        #[allow(clippy::too_many_arguments)]
        fn notify(
            &self,
            app_name: String,
            _replaces_id: u32,
            _app_icon: String,
            _summary: String,
            _body: String,
            actions: Vec<String>,
            hints: HashMap<String, OwnedValue>,
            _expire_timeout: i32,
            #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
        ) -> u32 {
            let _ = self.pedidos.send((app_name, actions, hints));
            let emitter = emitter.into_owned();
            let resposta = self.resposta;
            // O método roda no executor do zbus, fora do tokio.
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                let _ = zbus::block_on(async {
                    match resposta {
                        Some(acao) => Self::action_invoked(&emitter, 7, acao).await,
                        None => Self::notification_closed(&emitter, 7, 2).await,
                    }
                });
            });
            7
        }

        #[zbus(signal)]
        async fn action_invoked(e: &SignalEmitter<'_>, id: u32, action: &str) -> zbus::Result<()>;

        #[zbus(signal)]
        async fn notification_closed(
            e: &SignalEmitter<'_>,
            id: u32,
            reason: u32,
        ) -> zbus::Result<()>;
    }

    /// Sem servidor de notificação, clique e fechamento. Num D-Bus próprio, pra nunca mostrar
    /// notificação na tela de verdade:
    /// `dbus-run-session -- cargo test -- --ignored notificacao_no_dbus_proprio`
    #[tokio::test]
    #[ignore = "precisa de um D-Bus próprio: dbus-run-session"]
    async fn notificacao_no_dbus_proprio() {
        let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
        assert!(!bus.contains("/run/user/"), "barramento da sessão: {bus}");
        const NOME: &str = "org.freedesktop.Notifications";
        let conn = zbus::Connection::session().await.unwrap();
        let dbus = zbus::fdo::DBusProxy::new(&conn).await.unwrap();
        let ativaveis = dbus.list_activatable_names().await.unwrap();
        assert!(
            !ativaveis.iter().any(|n| n.as_str() == NOME),
            "o D-Bus abriria um servidor de verdade"
        );

        assert!(!show("Ana te chamou".into(), "call-tete".into(), true).await);

        // Resposta do servidor, se é chamado e se conta como clique.
        for (resposta, call, clicou) in [
            (Some("default"), true, true),
            (Some("entrar"), true, true),
            (Some(AGORA_NAO), true, false),
            (Some("default"), false, true),
            (None, false, false),
        ] {
            let (pedidos, mut recebidos) = tokio::sync::mpsc::unbounded_channel();
            let servidor = zbus::connection::Builder::session()
                .unwrap()
                .name(NOME)
                .unwrap()
                .serve_at(
                    "/org/freedesktop/Notifications",
                    Servidor { pedidos, resposta },
                )
                .unwrap()
                .build()
                .await
                .unwrap();
            let resultado = tokio::time::timeout(
                Duration::from_secs(5),
                show("Ana te chamou".into(), "call-tete".into(), call),
            )
            .await
            .expect("a espera não acabou");
            assert_eq!(resultado, clicou, "{resposta:?}");
            let (app, acoes, dicas) = recebidos.recv().await.unwrap();
            assert_eq!(app, "Concord");
            let botoes: &[&str] = if call {
                &["entrar", "Entrar", AGORA_NAO, "Agora não"]
            } else {
                &[]
            };
            assert_eq!(acoes, [&["default", "Abrir"], botoes].concat());
            assert_eq!(
                dicas
                    .get("suppress-sound")
                    .map(|v| bool::try_from(v).unwrap()),
                Some(true)
            );
            assert_eq!(
                dicas
                    .get("desktop-entry")
                    .map(|v| <&str>::try_from(v).unwrap().to_owned()),
                Some(crate::update::APP_ID.to_owned())
            );
            drop(servidor);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(ESPERANDO.load(Ordering::Relaxed), 0);
    }
}

/// Ponta a ponta contra o site local: uma conta chama e marca a outra, e o tempo real da outra
/// tem de trazer os dois avisos. Sem janela, notificação de verdade nem som. Uso:
/// `CONCORD_URL=http://localhost:3000 CONCORD_USER=.. CONCORD_PASS=.. CONCORD_ALVO=..
/// CONCORD_ALVO_PASS=.. CONCORD_VOZ=<slug> CONCORD_CANAL=<slug de texto>
/// cargo test bancada_notificacao -- --ignored --nocapture`
#[cfg(test)]
mod bancada {
    use std::time::Duration;

    use futures_util::StreamExt;

    use super::*;
    use crate::api::{ChannelKind, Secret};

    fn var(nome: &str) -> String {
        std::env::var(nome).unwrap_or_else(|_| panic!("falta {nome}"))
    }

    async fn entra(base: &str, user: &str, pass: &str) -> Client {
        let cookie = Client::new(base, None)
            .login(user, &Secret(pass.into()))
            .await
            .expect("login");
        Client::new(base, Some(cookie))
    }

    async fn proximo(
        live: &mut (impl Stream<Item = Live<NotifyEvent>> + Unpin),
    ) -> Live<NotifyEvent> {
        tokio::time::timeout(Duration::from_secs(10), live.next())
            .await
            .expect("nada chegou em 10 s")
            .expect("o tempo real acabou")
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn bancada_notificacao() {
        let base = var("CONCORD_URL");
        let (quem, alvo) = (var("CONCORD_USER"), var("CONCORD_ALVO"));
        let chama = entra(&base, &quem, &var("CONCORD_PASS")).await;
        let recebe = entra(&base, &alvo, &var("CONCORD_ALVO_PASS")).await;
        let mut live = Box::pin(live(recebe));
        assert_eq!(proximo(&mut live).await, Live::Connected);

        let id = chama
            .members()
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.username == alvo)
            .expect("alvo na lista de membros")
            .id;
        let voz = var("CONCORD_VOZ");
        assert_eq!(chama.call_people(&[id], &voz).await.unwrap(), (1, 1));
        match proximo(&mut live).await {
            Live::Event(NotifyEvent::Call {
                caller, channel, ..
            }) => assert_eq!((caller, channel), (quem.clone(), voz)),
            outro => panic!("esperava o chamado, veio {outro:?}"),
        }

        let slug = var("CONCORD_CANAL");
        let canal = chama
            .channels()
            .await
            .unwrap()
            .into_iter()
            .find(|c| c.slug == slug && c.kind == ChannelKind::Text)
            .expect("canal de texto")
            .id;
        let run = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
        chama
            .send_message(canal, &format!("oi @{alvo} {run}"), None)
            .await
            .unwrap();
        match proximo(&mut live).await {
            Live::Event(NotifyEvent::Mention {
                author,
                channel_id,
                excerpt,
                ..
            }) => {
                assert_eq!((author, channel_id), (quem, canal));
                assert!(excerpt.contains(&run), "{excerpt}");
            }
            outro => panic!("esperava a menção, veio {outro:?}"),
        }
        println!("chamado e menção chegaram pelo tempo real");
    }
}
