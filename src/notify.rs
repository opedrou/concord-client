//! Notificação nativa do sistema e o tempo real que diz o que notificar: chamado pra call e
//! `@`. O som é do próprio app (`audio::play_once`), então a notificação vai muda.
// Ligado na interface na próxima etapa; aí este `allow` sai.
#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::Stream;
use notify_rust::{Notification, NotificationResponse};

use crate::api::{Client, NotifyEvent};
use crate::sse::Live;

/// AUMID do Windows: o mesmo do atalho do Menu Iniciar, senão o toast sai sem o nome e o
/// ícone do Concord.
#[cfg(windows)]
pub const APP_ID: &str = "Concord.Desktop";

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

/// Mostra a notificação e espera ela fechar. `true`: a pessoa clicou nela. Sem servidor de
/// notificação no sistema, `false` e uma linha no log.
pub async fn show(title: String, body: String) -> bool {
    tokio::task::spawn_blocking(move || show_blocking(&title, &body))
        .await
        .unwrap_or(false)
}

fn show_blocking(title: &str, body: &str) -> bool {
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
    // Sem `sound_name`, o toast sai com `<audio silent="true"/>`.
    #[cfg(windows)]
    n.app_id(APP_ID);
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
    let _ = handle.wait_for_response(|r: &NotificationResponse| {
        clicked = matches!(
            r,
            NotificationResponse::Default | NotificationResponse::Action(_)
        );
    });
    clicked
}

/// Solta a vaga do [`ESPERANDO`] mesmo se a espera entrar em pânico.
struct Esperando;

impl Drop for Esperando {
    fn drop(&mut self) {
        ESPERANDO.fetch_sub(1, Ordering::Relaxed);
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

        assert!(!show("Ana te chamou".into(), "call-tete".into()).await);

        for (resposta, clicou) in [(Some("default"), true), (None, false)] {
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
                show("Ana te chamou".into(), "call-tete".into()),
            )
            .await
            .expect("a espera não acabou");
            assert_eq!(resultado, clicou, "{resposta:?}");
            let (app, acoes, dicas) = recebidos.recv().await.unwrap();
            assert_eq!(app, "Concord");
            assert_eq!(acoes, ["default", "Abrir"]);
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
