//! Ícone na bandeja, com "Abrir Concord", o mic e a saída da call quando tem call, "Não
//! incomodar" e "Sair". Clique esquerdo abre. `@` não visto põe um ponto no ícone. No Linux
//! pelo StatusNotifierItem (`ksni`); no Windows pelo `tray-icon`, numa thread com o laço de
//! mensagens do Win32.

use std::sync::LazyLock;

use chrono::{DateTime, Local};
use futures_util::{SinkExt, Stream};
use tokio::sync::{mpsc::UnboundedSender, watch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    /// A bandeja está na tela: fechar a janela pode deixar o app nela.
    Shown,
    /// Sem bandeja, ou ela sumiu (GNOME sem a extensão AppIndicator, ou o painel ainda não
    /// subiu no boot): fechar a janela tem de fechar o app. Volta com [`TrayEvent::Shown`].
    Missing,
    Open,
    Quit,
    /// Liga ou desliga o mic da call, como o botão.
    ToggleMic,
    /// Sai da call.
    Leave,
    /// Liga o não incomodar por uma hora, ou desliga antes da hora.
    Quiet,
}

/// O que o menu e o ícone mostram, vindo da interface por [`set`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct State {
    /// Em call: se o mic está desligado. Fora de call, `None`, e o menu fica sem os itens dela.
    pub mic_muted: Option<bool>,
    /// Chegou `@` com a janela sem foco: o ícone ganha o ponto.
    pub unseen: bool,
    /// Não incomodar até essa hora.
    pub quiet: Option<DateTime<Local>>,
}

/// Último estado pedido pela interface. A bandeja lê daqui quando muda.
static STATE: LazyLock<watch::Sender<State>> =
    LazyLock::new(|| watch::Sender::new(State::default()));

/// Atualiza o menu e o ícone. Só avisa a bandeja quando algo mudou.
pub fn set(state: State) {
    STATE.send_if_modified(|s| {
        let changed = *s != state;
        *s = state;
        changed
    });
}

const ABRIR: &str = "Abrir Concord";
const SAIR: &str = "Sair";

/// Itens do menu, de cima pra baixo. `None` é um separador.
fn items(state: &State) -> Vec<Option<(String, TrayEvent)>> {
    let mut items = vec![Some((ABRIR.into(), TrayEvent::Open)), None];
    if let Some(muted) = state.mic_muted {
        let mic = if muted {
            "Ligar microfone"
        } else {
            "Desligar microfone"
        };
        items.extend([
            Some((mic.into(), TrayEvent::ToggleMic)),
            Some(("Sair da call".into(), TrayEvent::Leave)),
            None,
        ]);
    }
    let quiet = match state.quiet {
        Some(until) => format!("Não incomodar até {}", until.format("%H:%M")),
        None => "Não incomodar por 1 h".into(),
    };
    items.extend([
        Some((quiet, TrayEvent::Quiet)),
        None,
        Some((SAIR.into(), TrayEvent::Quit)),
    ]);
    items
}

/// A bandeja enquanto o stream existir, pra uma `Subscription::run`. O primeiro evento é
/// `Shown` ou `Missing`.
pub fn run() -> impl Stream<Item = TrayEvent> {
    iced::stream::channel(8, async |mut out| {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut state = STATE.subscribe();
        let first_state = *state.borrow_and_update();
        let Some(tray) = platform::start(tx, first_state).await else {
            let _ = out.send(TrayEvent::Missing).await;
            return;
        };
        // Sem o painel, o `Missing` já está na fila quando o `start` volta.
        let first = rx.try_recv().unwrap_or(TrayEvent::Shown);
        if out.send(first).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Some(event) if out.send(event).await.is_ok() => {}
                    _ => return,
                },
                Ok(()) = state.changed() => {
                    let now = *state.borrow_and_update();
                    tray.show(now).await;
                }
            }
        }
    })
}

/// Ícone do app em `size` x `size`, RGBA, com o ponto do `@` se `dot`.
fn icon_rgba(size: u32, dot: bool) -> Option<Vec<u8>> {
    let icon = image::load_from_memory(crate::icon::PNG).ok()?;
    let icon = icon.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
    let mut rgba = icon.into_rgba8().into_raw();
    if dot {
        paint_dot(&mut rgba, size);
    }
    Some(rgba)
}

/// Ponto vermelho no canto de baixo à direita, com a borda suavizada.
fn paint_dot(rgba: &mut [u8], size: u32) {
    const RED: [f32; 3] = [242.0, 63.0, 67.0];
    let r = size as f32 * 0.22;
    let c = size as f32 - r;
    for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % size, i as u32 / size);
        let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
        let a = (r - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
        if a == 0.0 {
            continue;
        }
        // O ponto por cima do ícone, em alfa direto.
        let under = px[3] as f32 / 255.0 * (1.0 - a);
        let out = a + under;
        for (ch, red) in px.iter_mut().zip(RED) {
            *ch = ((red * a + *ch as f32 * under) / out).round() as u8;
        }
        px[3] = (out * 255.0).round() as u8;
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use ksni::{TrayMethods, menu::StandardItem};

    use super::*;

    struct Bandeja {
        tx: UnboundedSender<TrayEvent>,
        icon: Vec<ksni::Icon>,
        /// O ícone com o ponto do `@`.
        dot: Vec<ksni::Icon>,
        state: State,
    }

    impl ksni::Tray for Bandeja {
        fn id(&self) -> String {
            crate::update::APP_ID.into()
        }

        fn title(&self) -> String {
            "Concord".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            if self.state.unseen {
                self.dot.clone()
            } else {
                self.icon.clone()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = self.tx.send(TrayEvent::Open);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            items(&self.state)
                .into_iter()
                .map(|item| match item {
                    None => ksni::MenuItem::Separator,
                    Some((label, event)) => StandardItem {
                        label,
                        activate: Box::new(move |b: &mut Self| {
                            let _ = b.tx.send(event);
                        }),
                        ..Default::default()
                    }
                    .into(),
                })
                .collect()
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

    impl Guard {
        /// O ksni compara as propriedades e avisa o painel do que mudou.
        pub async fn show(&self, state: State) {
            self.0.update(|b| b.state = state).await;
        }
    }

    /// `None` só sem D-Bus. Sem o painel, o serviço sobe assim mesmo (ele pode aparecer
    /// depois, como no boot) e avisa `Missing`.
    pub async fn start(tx: UnboundedSender<TrayEvent>, state: State) -> Option<Guard> {
        let icon = |dot| {
            [32, 64]
                .into_iter()
                .filter_map(|size| Some(argb(size as i32, icon_rgba(size, dot)?)))
                .collect()
        };
        let (dot, icon) = (icon(true), icon(false));
        match (Bandeja {
            tx,
            icon,
            dot,
            state,
        })
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
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    };
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        System::Threading::GetCurrentThreadId,
        UI::WindowsAndMessaging::{
            DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, TranslateMessage, WM_APP,
            WM_QUIT,
        },
    };

    use super::*;

    /// O que o menu pode mandar. O id de cada item é o nome do evento.
    const MENU: [TrayEvent; 5] = [
        TrayEvent::Open,
        TrayEvent::ToggleMic,
        TrayEvent::Leave,
        TrayEvent::Quiet,
        TrayEvent::Quit,
    ];

    /// Encerra o laço de mensagens da thread da bandeja, e o ícone sai junto.
    pub struct Guard(u32);

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: só posta uma mensagem; thread que já acabou dá erro, ignorado.
            let _ = unsafe { PostThreadMessageW(self.0, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    }

    impl Guard {
        /// O ícone e o menu só mudam na thread deles: acorda ela, que lê o [`STATE`].
        pub async fn show(&self, _: State) {
            // SAFETY: só posta uma mensagem; thread que já acabou dá erro, ignorado.
            let _ = unsafe { PostThreadMessageW(self.0, WM_APP, WPARAM(0), LPARAM(0)) };
        }
    }

    pub async fn start(tx: UnboundedSender<TrayEvent>, state: State) -> Option<Guard> {
        let (ready, started) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("concord-bandeja".into())
            .spawn(move || run(tx, state, ready))
            .ok()?;
        started.await.ok().flatten().map(Guard)
    }

    fn menu(state: &State) -> Result<Menu, String> {
        let menu = Menu::new();
        for item in items(state) {
            match item {
                None => menu.append(&PredefinedMenuItem::separator()),
                Some((label, event)) => {
                    menu.append(&MenuItem::with_id(format!("{event:?}"), label, true, None))
                }
            }
            .map_err(|e| e.to_string())?;
        }
        Ok(menu)
    }

    fn icon(dot: bool) -> Result<Icon, String> {
        let rgba = icon_rgba(32, dot).ok_or("ícone")?;
        Icon::from_rgba(rgba, 32, 32).map_err(|e| e.to_string())
    }

    fn apply(tray: &TrayIcon, state: &State) {
        let done = menu(state)
            .map(|menu| tray.set_menu(Some(Box::new(menu))))
            .and_then(|()| icon(state.unseen))
            .and_then(|icon| tray.set_icon(Some(icon)).map_err(|e| e.to_string()));
        if let Err(e) = done {
            log!("bandeja: não atualizou: {e}");
        }
    }

    /// O ícone vive nesta thread: a janela escondida dele recebe os cliques pelo laço daqui.
    fn run(
        tx: UnboundedSender<TrayEvent>,
        mut shown: State,
        ready: tokio::sync::oneshot::Sender<Option<u32>>,
    ) {
        let built = menu(&shown).and_then(|menu| {
            TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Concord")
                .with_icon(icon(shown.unseen)?)
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
            if let Some(event) = MENU.into_iter().find(|ev| e.id == format!("{ev:?}")) {
                let _ = tx.send(event);
            }
        }));
        // SAFETY: id da thread atual, sem ponteiro.
        let _ = ready.send(Some(unsafe { GetCurrentThreadId() }));
        let mut msg = MSG::default();
        // SAFETY: laço de mensagens padrão, com o `MSG` desta pilha. -1 é erro, 0 é o
        // `WM_QUIT` do `Guard`.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
            // SAFETY: a mensagem que o `GetMessageW` acabou de pôr no `MSG`.
            unsafe {
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
            // O `WM_APP` do `Guard::show` só acorda. Mensagem de thread se perde com o menu
            // aberto (laço modal), então confere a cada mensagem, não só nela.
            let now = *STATE.borrow();
            if now != shown {
                apply(&tray, &now);
                shown = now;
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
        let rgba = icon_rgba(32, false).unwrap();
        assert_eq!(rgba.len(), 32 * 32 * 4);
        assert_eq!(rgba[3], 0, "canto transparente");
    }

    #[test]
    fn ponto_so_no_canto_de_baixo() {
        let (plain, dot) = (icon_rgba(32, false).unwrap(), icon_rgba(32, true).unwrap());
        let px = |rgba: &[u8], x: usize, y: usize| rgba[(y * 32 + x) * 4..][..4].to_vec();
        assert_eq!(px(&dot, 25, 25), [242, 63, 67, 255], "meio do ponto");
        assert_eq!(px(&dot, 31, 31), px(&plain, 31, 31), "o canto fica de fora");
        assert_eq!(
            dot[..16 * 32 * 4],
            plain[..16 * 32 * 4],
            "metade de cima igual"
        );
    }

    #[test]
    fn menu_muda_com_a_call_e_o_nao_incomodar() {
        let labels = |state: &State| -> Vec<String> {
            items(state)
                .into_iter()
                .map(|i| i.map_or("-".into(), |(label, _)| label))
                .collect()
        };
        let fora = State::default();
        assert_eq!(
            labels(&fora),
            ["Abrir Concord", "-", "Não incomodar por 1 h", "-", "Sair"]
        );
        let mut em_call = State {
            mic_muted: Some(false),
            ..fora
        };
        assert_eq!(
            labels(&em_call)[2..4],
            ["Desligar microfone", "Sair da call"]
        );
        em_call.mic_muted = Some(true);
        assert_eq!(labels(&em_call)[2], "Ligar microfone");
        let ate = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(14, 35, 0)
            .unwrap()
            .and_local_timezone(Local)
            .unwrap();
        em_call.quiet = Some(ate);
        let menu = items(&em_call);
        assert_eq!(
            menu[5],
            Some(("Não incomodar até 14:35".into(), TrayEvent::Quiet))
        );
        assert_eq!(menu.last(), Some(&Some((SAIR.into(), TrayEvent::Quit))));
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

        // Em call e com `@` não visto: o painel tem de ver o menu e o ícone novos. O stream é
        // quem leva o estado pro ksni, então fica sendo lido um pouco.
        set(State {
            mic_muted: Some(false),
            unseen: true,
            quiet: None,
        });
        let _ = tokio::time::timeout(Duration::from_millis(500), eventos.next()).await;
        type Layout = (
            u32,
            (
                i32,
                std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
                Vec<zbus::zvariant::OwnedValue>,
            ),
        );
        let (_, (_, _, filhos)): Layout = quem_clica
            .call_method(
                Some(item.as_str()),
                "/MenuBar",
                Some("com.canonical.dbusmenu"),
                "GetLayout",
                &(0i32, -1i32, Vec::<String>::new()),
            )
            .await
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        let menu = format!("{filhos:?}");
        assert!(
            menu.contains("Desligar microfone") && menu.contains("Sair da call"),
            "{menu}"
        );
        let icone: Vec<(i32, i32, Vec<u8>)> = zbus::Proxy::new(
            &quem_clica,
            item.as_str(),
            "/StatusNotifierItem",
            "org.kde.StatusNotifierItem",
        )
        .await
        .unwrap()
        .get_property("IconPixmap")
        .await
        .unwrap();
        let (_, _, px) = icone.iter().find(|(w, ..)| *w == 32).expect("ícone de 32");
        assert_eq!(
            px[(25 * 32 + 25) * 4..][..4],
            [255, 242, 63, 67],
            "ponto em ARGB"
        );
    }
}
