//! Bancada do chat contra o site local, sem janela: o cliente pela API, com a mesma linha do
//! tempo e o mesmo SSE da tela, e o Chrome pelo site, nos dois sentidos. No meio, um proxy
//! entre o cliente e o site derruba as conexões sem derrubar o site. Roda por
//! `src/chat/bancada.sh`.
use std::{pin::Pin, process::Command, time::Instant};

use futures_util::{Stream, StreamExt};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    sync::watch,
    time::sleep,
};

use super::*;
use crate::api::{ChannelKind, Secret};

const ESPERA: Duration = Duration::from_secs(90);
/// Mensagens mandadas com o cliente fora do ar.
const DURANTE: usize = 5;

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("faltou {name}"))
}

/// Proxy TCP do cliente pro site. Fora do ar, solta as conexões abertas e recusa as novas.
struct Proxy {
    base: String,
    down: watch::Sender<bool>,
}

impl Proxy {
    async fn start(target: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = target.trim_start_matches("http://").to_owned();
        let (down, rx) = watch::channel(false);
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    return;
                };
                if *rx.borrow() {
                    continue;
                }
                let (mut rx, target) = (rx.clone(), target.clone());
                tokio::spawn(async move {
                    let Ok(mut outbound) = TcpStream::connect(&target).await else {
                        return;
                    };
                    tokio::select! {
                        _ = copy_bidirectional(&mut inbound, &mut outbound) => {}
                        _ = rx.wait_for(|down| *down) => {}
                    }
                });
            }
        });
        Self {
            base: format!("http://127.0.0.1:{port}"),
            down,
        }
    }

    fn set_down(&self, down: bool) {
        self.down.send_replace(down);
    }
}

/// O que a interface faz com o canal aberto, sem a interface.
struct Lado {
    client: Client,
    channel: i64,
    tela: Timeline,
    live: Pin<Box<dyn Stream<Item = Live> + Send>>,
    conexoes: usize,
    quedas: usize,
}

impl Lado {
    /// Processa o SSE até `pronto` valer.
    async fn ate(&mut self, o_que: &str, pronto: impl Fn(&Self) -> bool) {
        let fim = tokio::time::Instant::now() + ESPERA;
        while !pronto(self) {
            let item = tokio::time::timeout_at(fim, self.live.next())
                .await
                .unwrap_or_else(|_| panic!("não chegou: {o_que}"))
                .expect("o SSE acabou");
            match item {
                Live::Connected => {
                    self.conexoes += 1;
                    self.recuperar().await;
                }
                Live::Event(event) => self.tela.apply(event),
                Live::Lost => self.quedas += 1,
                Live::Unauthorized => panic!("sessão recusada"),
            }
        }
        println!("CLIENTE: {o_que}");
    }

    /// Como a tela: a busca que falha tenta de novo.
    async fn recuperar(&mut self) {
        loop {
            match recover(&self.client, self.channel, self.tela.newest()).await {
                Ok(window) => return self.tela.recovered(window),
                Err(e) => {
                    println!("CLIENTE: busca falhou ({e:?}), de novo");
                    sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    fn tem(&self, content: &str) -> usize {
        self.tela
            .messages
            .iter()
            .filter(|m| m.content == content)
            .count()
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bancada_chat() {
    let base = var("CONCORD_URL");
    let cookie = Client::new(&base, None)
        .login(&var("CONCORD_USER"), &Secret(var("CONCORD_PASS")))
        .await
        .expect("login");
    // O outro lado da API, sem proxy: manda enquanto o cliente está fora.
    let direto = Client::new(&base, Some(cookie.clone()));
    let slug = var("CONCORD_CANAL");
    let channel = direto
        .channels()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.slug == slug && c.kind == ChannelKind::Text)
        .expect("canal de texto")
        .id;
    let proxy = Proxy::start(&base).await;
    let client = Client::new(&proxy.base, Some(cookie));
    let run = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let out = std::env::var("OUT").unwrap_or_else(|_| std::env::temp_dir().display().to_string());

    let mut web = Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/chat/conversa.mjs"
        ))
        .env("CONCORD_CANAL", &slug)
        .env("RUN", &run)
        .env("OUT", &out)
        .spawn()
        .expect("node");

    // Abrir o chat: primeira carga e SSE juntos, como a tela.
    let mut lado = Lado {
        tela: Timeline::default(),
        live: Box::pin(live(client.clone(), channel)),
        client,
        channel,
        conexoes: 0,
        quedas: 0,
    };
    lado.recuperar().await;
    lado.ate("conectou", |l| l.conexoes == 1).await;

    let web1 = format!("web-1 {run}");
    lado.ate("viu a mensagem do Chrome", |l| l.tem(&web1) == 1)
        .await;
    let enviada = lado
        .client
        .send_message(channel, &format!("cliente-1 {run}"))
        .await
        .expect("envio");
    lado.tela.insert(enviada);

    // Derruba o caminho do cliente e manda com ele fora.
    proxy.set_down(true);
    lado.ate("o SSE caiu", |l| l.quedas == 1).await;
    let antes = Instant::now();
    for k in 1..=DURANTE {
        direto
            .send_message(channel, &format!("durante-{k} {run}"))
            .await
            .expect("envio durante a queda");
    }
    sleep(Duration::from_secs(4)).await;
    proxy.set_down(false);
    let todas = |l: &Lado| (1..=DURANTE).all(|k| l.tem(&format!("durante-{k} {run}")) >= 1);
    lado.ate("recuperou o que perdeu", |l| l.conexoes >= 2 && todas(l))
        .await;
    println!(
        "CLIENTE: fora do ar por {:.1} s, {} conexões",
        antes.elapsed().as_secs_f32(),
        lado.conexoes
    );

    for k in 1..=DURANTE {
        assert_eq!(lado.tem(&format!("durante-{k} {run}")), 1, "durante-{k}");
    }
    assert_eq!(lado.tem(&web1), 1);
    // A tela bate com o servidor: sem buraco, sem repetida, sem fantasma.
    let servidor: Vec<i64> = direto
        .messages(channel, None, PAGINA)
        .await
        .unwrap()
        .messages
        .iter()
        .map(|m| m.id)
        .collect();
    let tela: Vec<i64> = lado.tela.messages.iter().map(|m| m.id).collect();
    assert_eq!(
        tela[tela.len().saturating_sub(servidor.len())..],
        servidor[..]
    );

    let status = tokio::task::spawn_blocking(move || web.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "Chrome: {status}");
    println!("BANCADA OK");
}
