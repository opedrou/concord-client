//! Ícone na bandeja, com "Abrir Concord" e "Sair". Clique esquerdo abre. No Linux pelo
//! StatusNotifierItem (`ksni`); no Windows pelo `tray-icon`, numa thread com o laço de
//! mensagens do Win32.
// Ligado na interface na próxima etapa; aí este `allow` sai.
#![allow(dead_code)]

use futures_util::{SinkExt, Stream};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// A bandeja está na tela: fechar a janela pode deixar o app nela.
    Shown,
    /// Sem bandeja, ou ela sumiu (GNOME sem a extensão AppIndicator, ou o painel ainda não
    /// subiu no boot): fechar a janela tem de fechar o app. Volta com [`TrayEvent::Shown`].
    Missing,
    Open,
    Quit,
}

const ABRIR: &str = "Abrir Concord";
const SAIR: &str = "Sair";

/// A bandeja enquanto o stream existir, pra uma `Subscription::run`. O primeiro evento é
/// `Shown` ou `Missing`.
pub fn run() -> impl Stream<Item = TrayEvent> {
    iced::stream::channel(8, async |mut out| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let Some(_tray) = platform::start(tx).await else {
            let _ = out.send(TrayEvent::Missing).await;
            return;
        };
        // Sem o painel, o `Missing` já está na fila quando o `start` volta.
        let first = rx.try_recv().unwrap_or(TrayEvent::Shown);
        if out.send(first).await.is_err() {
            return;
        }
        while let Some(event) = rx.recv().await {
            if out.send(event).await.is_err() {
                return;
            }
        }
    })
}

/// Ícone do app em `size` x `size`, RGBA.
fn icon_rgba(size: u32) -> Option<Vec<u8>> {
    let icon = image::load_from_memory(crate::icon::PNG).ok()?;
    let icon = icon.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
    Some(icon.into_rgba8().into_raw())
}

#[cfg(target_os = "linux")]
mod platform {
    use ksni::{TrayMethods, menu::StandardItem};

    use super::*;

    struct Bandeja {
        tx: UnboundedSender<TrayEvent>,
        icon: Vec<ksni::Icon>,
    }

    impl ksni::Tray for Bandeja {
        fn id(&self) -> String {
            crate::update::APP_ID.into()
        }

        fn title(&self) -> String {
            "Concord".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            self.icon.clone()
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.tx.send(TrayEvent::Open);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            let item = |label: &str, event: TrayEvent| {
                StandardItem {
                    label: label.into(),
                    activate: Box::new(move |b: &mut Self| {
                        let _ = b.tx.send(event);
                    }),
                    ..Default::default()
                }
                .into()
            };
            vec![item(ABRIR, TrayEvent::Open), item(SAIR, TrayEvent::Quit)]
        }

        fn watcher_online(&self) {
            log!("bandeja: painel voltou");
            let _ = self.tx.send(TrayEvent::Shown);
        }

        /// Fica de pé esperando o painel voltar.
        fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
            log!("bandeja: sem painel ({reason:?})");
            let _ = self.tx.send(TrayEvent::Missing);
            true
        }
    }

    /// Para o serviço da bandeja quando o stream cai.
    pub struct Guard(ksni::Handle<Bandeja>);

    impl Drop for Guard {
        fn drop(&mut self) {
            // O pedido sai na chamada; esperar o fim não muda nada aqui.
            drop(self.0.shutdown());
        }
    }

    /// `None` só sem D-Bus. Sem o painel, o serviço sobe assim mesmo (ele pode aparecer
    /// depois, como no boot) e avisa `Missing`.
    pub async fn start(tx: UnboundedSender<TrayEvent>) -> Option<Guard> {
        let icon = [32, 64]
            .into_iter()
            .filter_map(|size| Some(argb(size as i32, icon_rgba(size)?)))
            .collect();
        match (Bandeja { tx, icon })
            .assume_sni_available(true)
            .spawn()
            .await
        {
            Ok(handle) => Some(Guard(handle)),
            Err(e) => {
                log!("bandeja: não abriu: {e}");
                None
            }
        }
    }

    /// O StatusNotifierItem quer ARGB32 em ordem de rede.
    pub(super) fn argb(size: i32, mut rgba: Vec<u8>) -> ksni::Icon {
        for px in rgba.chunks_exact_mut(4) {
            px.rotate_right(1);
        }
        ksni::Icon {
            width: size,
            height: size,
            data: rgba,
        }
    }
}

#[cfg(windows)]
mod platform {
    use tray_icon::{
        MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem},
    };
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        System::Threading::GetCurrentThreadId,
        UI::WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, TranslateMessage, WM_QUIT,
        },
    };

    use super::*;

    /// Encerra o laço de mensagens da thread da bandeja, e o ícone sai junto.
    pub struct Guard(u32);

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: só posta uma mensagem; thread que já acabou dá erro, ignorado.
            let _ = unsafe { PostThreadMessageW(self.0, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    }

    pub async fn start(tx: UnboundedSender<TrayEvent>) -> Option<Guard> {
        let (ready, started) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("concord-bandeja".into())
            .spawn(move || run(tx, ready))
            .ok()?;
        started.await.ok().flatten().map(Guard)
    }

    /// O ícone vive nesta thread: a janela escondida dele recebe os cliques pelo laço daqui.
    fn run(tx: UnboundedSender<TrayEvent>, ready: tokio::sync::oneshot::Sender<Option<u32>>) {
        let open = MenuItem::with_id("abrir", ABRIR, true, None);
        let quit = MenuItem::with_id("sair", SAIR, true, None);
        let built = Menu::with_items(&[&open, &quit])
            .map_err(|e| e.to_string())
            .and_then(|menu| {
                let icon = icon_rgba(32).ok_or("ícone")?;
                let icon = tray_icon::Icon::from_rgba(icon, 32, 32).map_err(|e| e.to_string())?;
                TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_menu_on_left_click(false)
                    .with_tooltip("Concord")
                    .with_icon(icon)
                    .build()
                    .map_err(|e| e.to_string())
            });
        let tray = match built {
            Ok(tray) => tray,
            Err(e) => {
                log!("bandeja: não abriu: {e}");
                let _ = ready.send(None);
                return;
            }
        };
        let clicks = tx.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                let _ = clicks.send(TrayEvent::Open);
            }
        }));
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let event = if e.id == "sair" {
                TrayEvent::Quit
            } else {
                TrayEvent::Open
            };
            let _ = tx.send(event);
        }));
        // SAFETY: id da thread atual, sem ponteiro.
        let _ = ready.send(Some(unsafe { GetCurrentThreadId() }));
        let mut msg = MSG::default();
        // SAFETY: laço de mensagens padrão, com o `MSG` desta pilha.
        unsafe {
            // -1 é erro, 0 é o `WM_QUIT` do `Guard`.
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        }
        TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
        MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
        drop(tray);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icone_sai_no_tamanho_e_em_rgba() {
        let rgba = icon_rgba(32).unwrap();
        assert_eq!(rgba.len(), 32 * 32 * 4);
        assert_eq!(rgba[3], 0, "canto transparente");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pixmap_do_linux_em_argb() {
        let icon = platform::argb(1, vec![1, 2, 3, 4]);
        assert_eq!(
            (icon.width, icon.height, icon.data),
            (1, 1, vec![4, 1, 2, 3])
        );
    }

    /// Painel falso: só guarda quem se registrou.
    #[cfg(target_os = "linux")]
    struct Painel(tokio::sync::mpsc::UnboundedSender<String>);

    #[cfg(target_os = "linux")]
    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Painel {
        fn register_status_notifier_item(&self, service: &str) {
            let _ = self.0.send(service.to_owned());
        }
    }

    /// Sem painel, como o GNOME sem a extensão, e o painel que aparece depois, como no boot.
    /// Num D-Bus próprio, pra nunca pôr ícone na bandeja de verdade:
    /// `dbus-run-session -- cargo test -- --ignored bandeja_no_dbus_proprio`
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "precisa de um D-Bus próprio: dbus-run-session"]
    async fn bandeja_no_dbus_proprio() {
        use futures_util::StreamExt;
        use std::time::Duration;
        let bus = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
        assert!(!bus.contains("/run/user/"), "barramento da sessão: {bus}");
        let proximo = async |s: &mut std::pin::Pin<Box<dyn Stream<Item = TrayEvent> + Send>>| {
            tokio::time::timeout(Duration::from_secs(5), s.next())
                .await
                .expect("sem evento")
        };
        let mut eventos: std::pin::Pin<Box<dyn Stream<Item = TrayEvent> + Send>> = Box::pin(run());
        assert_eq!(proximo(&mut eventos).await, Some(TrayEvent::Missing));

        let (tx, mut registrados) = tokio::sync::mpsc::unbounded_channel();
        let _painel = zbus::connection::Builder::session()
            .unwrap()
            .name("org.kde.StatusNotifierWatcher")
            .unwrap()
            .serve_at("/StatusNotifierWatcher", Painel(tx))
            .unwrap()
            .build()
            .await
            .unwrap();
        assert_eq!(proximo(&mut eventos).await, Some(TrayEvent::Shown));
        let item = tokio::time::timeout(Duration::from_secs(5), registrados.recv())
            .await
            .unwrap()
            .unwrap();
        // Clique esquerdo no ícone.
        let quem_clica = zbus::Connection::session().await.unwrap();
        quem_clica
            .call_method(
                Some(item.as_str()),
                "/StatusNotifierItem",
                Some("org.kde.StatusNotifierItem"),
                "Activate",
                &(0i32, 0i32),
            )
            .await
            .unwrap();
        assert_eq!(proximo(&mut eventos).await, Some(TrayEvent::Open));
    }
}
