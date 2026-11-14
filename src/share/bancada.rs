//! Bancada com sala real e fonte sintética, sem portal. Roda por `src/share/bancada.sh`.
use std::{
    collections::HashMap,
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use livekit::{Room, RoomEvent, RoomOptions, SimulateScenario, webrtc::stats::RtcStats};
use tokio::{
    sync::mpsc::Receiver,
    time::{sleep, timeout},
};

use super::*;
use crate::api::{Client, Secret};
use crate::audio::{AudioPrefs, Engine, NoiseLevel, Sensitivity};
use crate::call::{Call, End, Event};

/// Sufixo fixo da identity da bancada.
const INSTALL: &str = "bancada-tela";
/// Tempo do "seletor": a fonte fica sem quadro nenhum, como no portal.
const SELETOR: Duration = Duration::from_secs(2);
const ESPERA: Duration = Duration::from_secs(20);

static ABERTAS: AtomicUsize = AtomicUsize::new(0);
static VIVAS: AtomicUsize = AtomicUsize::new(0);

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("faltou {name}"))
}

fn segundos() -> u64 {
    var("SEGUNDOS").parse().expect("SEGUNDOS")
}

/// Tela colorida em blocos. Rolar é só mudar o início do slice, então a fonte não gasta
/// CPU: o que a medida pega é conversão, redução e encoder.
fn padrao(w: usize, h: usize) -> Vec<u8> {
    let mut image = vec![0u8; w * h * 4];
    for (i, px) in image.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i % w) as u32, (i / w) as u32);
        let cell = (x / 32).wrapping_mul(2654435761) ^ (y / 32).wrapping_mul(2246822519);
        let tint = (cell >> 8) as u8;
        let line = if y % 16 < 2 { 40 } else { 0 };
        let green = (tint / 3).wrapping_add((x / 8) as u8);
        px.copy_from_slice(&[tint / 2 + line, green, (cell >> 16) as u8, 255]);
    }
    image
}

fn sintetica(fps: u32, stop: &AtomicBool, mut feed: Feed) {
    // A segunda abertura é a da troca: outra tela, de outro tamanho.
    let troca = ABERTAS.fetch_add(1, Ordering::SeqCst) > 0;
    VIVAS.fetch_add(1, Ordering::SeqCst);
    const FOLGA: usize = 2048;
    let (var, padrao_tela) = if troca {
        ("FONTE2", "1280x1024")
    } else {
        ("FONTE", "2560x1440")
    };
    let tela = std::env::var(var).unwrap_or_else(|_| padrao_tela.into());
    let (w, h) = tela.split_once('x').expect("FONTE");
    let (w, h): (usize, usize) = (w.parse().unwrap(), h.parse().unwrap());
    let image = padrao(w + FOLGA, h + 1);
    let inicio = Instant::now();
    while inicio.elapsed() < SELETOR && !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
    let mut n = 0;
    pace(fps, || {
        let start = (n * 8) % FOLGA * 4;
        feed.frame(&image[start..], (w + FOLGA) as u32 * 4, w as u32, h as u32);
        n += 1;
        !stop.load(Ordering::SeqCst)
    });
    VIVAS.fetch_sub(1, Ordering::SeqCst);
}

async fn cliente() -> Client {
    let base = var("CONCORD_URL");
    let senha = Secret(var("CONCORD_PASS"));
    let cookie = Client::new(&base, None)
        .login(&var("CONCORD_USER"), &senha)
        .await
        .expect("login");
    Client::new(&base, Some(cookie))
}

async fn entrar(client: &Client) -> Room {
    let details = client
        .connection_details(&var("CONCORD_ROOM"), INSTALL)
        .await
        .expect("token");
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    let (room, mut events) =
        Room::connect(&details.server_url, &details.participant_token, options)
            .await
            .expect("sala");
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if let RoomEvent::LocalTrackRepublished { .. } = event {
                println!("REPUBLICADA pelo SDK");
            }
        }
    });
    room
}

/// Ator ligado a uma sala que o teste controla.
struct Banca {
    cmd: UnboundedSender<Cmd>,
    estados: UnboundedReceiver<State>,
    room: watch::Sender<Option<LocalParticipant>>,
}

impl Banca {
    fn new(me: LocalParticipant) -> Self {
        let (cmd, cmds) = mpsc::unbounded_channel();
        let (states, estados) = mpsc::unbounded_channel();
        let (room, room_rx) = watch::channel(Some(me));
        tokio::spawn(run(
            cmds,
            room_rx,
            sintetica,
            Preview::default(),
            move |s| {
                let _ = states.send(s);
            },
            |v| println!("AUDIO {v:?}"),
        ));
        Self { cmd, estados, room }
    }

    async fn estado(&mut self) -> State {
        timeout(ESPERA, self.estados.recv())
            .await
            .expect("estado não veio")
            .expect("ator acabou")
    }

    async fn ao_vivo(&mut self, me: &LocalParticipant, quality: Quality) {
        self.cmd.send(Cmd::Start(quality, false, None)).unwrap();
        assert_eq!(self.estado().await, State::Choosing);
        sleep(SELETOR / 2).await;
        assert!(tela(me).is_none(), "publicou antes do primeiro quadro");
        assert_eq!(self.estado().await, State::Live);
        assert!(tela(me).is_some());
    }

    async fn parar(&mut self, me: &LocalParticipant) {
        self.cmd.send(Cmd::Stop).unwrap();
        assert_eq!(self.estado().await, State::Off);
        assert!(tela(me).is_none(), "ficou publicada");
        assert!(som(me).is_none(), "o áudio ficou publicado");
        solta().await;
    }
}

fn tela(me: &LocalParticipant) -> Option<livekit::publication::LocalTrackPublication> {
    me.track_publications()
        .into_values()
        .find(|p| p.source() == TrackSource::Screenshare)
}

fn som(me: &LocalParticipant) -> Option<livekit::publication::LocalTrackPublication> {
    me.track_publications()
        .into_values()
        .find(|p| p.name() == app_audio::TRACK)
}

/// O áudio sai depois do vídeo no ar.
#[cfg(target_os = "linux")]
async fn som_no_ar(me: &LocalParticipant) {
    let inicio = Instant::now();
    while som(me).is_none() {
        assert!(inicio.elapsed() < ESPERA, "o áudio não foi publicado");
        sleep(Duration::from_millis(50)).await;
    }
    let p = som(me).unwrap();
    assert_eq!(p.source(), TrackSource::Unknown);
}

/// Codec e bytes do áudio que o WebRTC está mandando.
#[cfg(target_os = "linux")]
async fn som_enviado(me: &LocalParticipant) -> String {
    let stats = som(me).unwrap().track().unwrap().get_stats().await.unwrap();
    let mut linha = String::from("SOM ENVIADO");
    for s in stats {
        match s {
            RtcStats::Codec(c) => {
                linha += &format!(
                    " codec={} canais={} fmtp={:?}",
                    c.codec.mime_type, c.codec.channels, c.codec.sdp_fmtp_line
                )
            }
            RtcStats::OutboundRtp(o) => {
                linha += &format!(
                    " bytes={} alvo={:.0} kbps",
                    o.sent.bytes_sent,
                    o.outbound.target_bitrate / 1e3
                )
            }
            _ => {}
        }
    }
    linha
}

/// A thread de captura saiu.
async fn solta() {
    let inicio = Instant::now();
    while VIVAS.load(Ordering::SeqCst) > 0 {
        assert!(inicio.elapsed() < ESPERA, "a thread de captura não saiu");
        sleep(Duration::from_millis(10)).await;
    }
}

/// Uma linha com o que o encoder está mandando agora.
async fn enviado(me: &LocalParticipant) -> Option<(u32, String)> {
    let stats = tela(me)?.track()?.get_stats().await.ok()?;
    let o = stats.into_iter().find_map(|s| match s {
        RtcStats::OutboundRtp(o) => Some(o.outbound),
        _ => None,
    })?;
    let linha = format!(
        "ENVIADO {}x{} {:.0} fps quadros={} encoder={} hardware={} alvo={:.2} Mbps limite={:?}",
        o.frame_width,
        o.frame_height,
        o.frames_per_second,
        o.frames_encoded,
        o.encoder_implementation,
        o.power_efficient_encoder,
        o.target_bitrate / 1e6,
        o.quality_limitation_reason,
    );
    Some((o.frames_encoded, linha))
}

/// Falha se o encoder não andar: faixa publicada e parada engana no `tela()`.
async fn andando(me: &LocalParticipant) {
    let inicio = Instant::now();
    loop {
        sleep(Duration::from_secs(1)).await;
        if let Some((antes, _)) = enviado(me).await {
            sleep(Duration::from_secs(2)).await;
            let (depois, linha) = enviado(me).await.expect("sumiu a faixa");
            println!("{linha}");
            if depois > antes {
                return;
            }
        }
        assert!(inicio.elapsed() < ESPERA, "o encoder não andou");
    }
}

/// Publica pelo tempo de `SEGUNDOS`, pra medir de fora. `bancada.sh` espera o `PUBLICADO`.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bancada_tela_qualidade() {
    let chave = format!("\"{}\"", var("QUALIDADE"));
    let quality: Quality = serde_json::from_str(&chave).expect("QUALIDADE");
    let client = cliente().await;
    let room = entrar(&client).await;
    let me = room.local_participant();
    let mut banca = Banca::new(me.clone());
    banca.ao_vivo(&me, quality).await;
    println!("PUBLICADO {}", var("QUALIDADE"));
    let fim = Instant::now() + Duration::from_secs(segundos());
    while Instant::now() < fim {
        sleep(Duration::from_secs(5)).await;
        if let Some((_, linha)) = enviado(&me).await {
            println!("{linha}");
        }
    }
    banca.parar(&me).await;
    let _ = room.close().await;
    println!("FIM");
}

/// Sala nova na call: mesma fonte, nenhuma captura nova, e o Chrome volta a receber.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bancada_tela_sala_nova() {
    let client = cliente().await;
    let room = entrar(&client).await;
    let me = room.local_participant();
    let mut banca = Banca::new(me.clone());
    banca.ao_vivo(&me, Quality::Media).await;
    andando(&me).await;
    println!("PUBLICADO sala 1");
    sleep(Duration::from_secs(segundos())).await;

    // Mesma sequência do `lifecycle`: a sala some antes de a próxima conectar.
    banca.room.send_replace(None);
    let _ = room.close().await;
    drop((room, me));
    sleep(Duration::from_secs(3)).await;
    let room = entrar(&client).await;
    let me = room.local_participant();
    banca.room.send_replace(Some(me.clone()));
    assert_eq!(banca.estado().await, State::Live);
    assert_eq!(ABERTAS.load(Ordering::SeqCst), 1, "abriu o seletor de novo");
    andando(&me).await;
    println!("PUBLICADO sala 2");
    sleep(Duration::from_secs(segundos())).await;

    banca.parar(&me).await;
    let _ = room.close().await;
    println!("FIM");
}

/// Retomada do SDK, na mesma sala: a faixa segue, e parar acha a publicação mesmo que o
/// SDK tenha trocado o sid.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bancada_tela_retomada() {
    let client = cliente().await;
    let room = entrar(&client).await;
    let me = room.local_participant();
    let mut banca = Banca::new(me.clone());
    banca.ao_vivo(&me, Quality::Media).await;
    andando(&me).await;

    for scenario in [
        SimulateScenario::SignalReconnect,
        SimulateScenario::FullReconnect,
    ] {
        let antes = tela(&me).expect("publicada").sid();
        println!("CENARIO {scenario:?}");
        room.simulate_scenario(scenario).await.expect("cenário");
        sleep(Duration::from_secs(12)).await;
        let depois = tela(&me).expect("a faixa sumiu").sid();
        println!("SID {} -> {}", antes, depois);
        andando(&me).await;
        assert_eq!(banca.estados.try_recv().ok(), None, "estado mudou sozinho");
    }

    assert_eq!(ABERTAS.load(Ordering::SeqCst), 1, "abriu o seletor de novo");
    banca.parar(&me).await;
    let _ = room.close().await;
    println!("FIM");
}

/// Troca de tela no ar com o Chrome assistindo: a faixa é a mesma do começo ao fim, o
/// tamanho muda e os quadros não param. Com o `npm run dev` do site e as variáveis do
/// `bancada.sh`: `cargo test bancada_tela_troca -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bancada_tela_troca() {
    let client = cliente().await;
    let room = entrar(&client).await;
    let me = room.local_participant();
    let mut banca = Banca::new(me.clone());
    banca.ao_vivo(&me, Quality::Media).await;
    andando(&me).await;
    let antes = tela(&me).expect("publicada").sid();

    let web = tokio::task::spawn_blocking(|| {
        Command::new("node")
            .arg("src/share/assiste-troca.mjs")
            .output()
            .expect("node")
    });
    // O Chrome entra, clica em Assistir e já está medindo quando a troca sai.
    sleep(Duration::from_secs(15)).await;
    banca.cmd.send(Cmd::Swap(None)).unwrap();
    println!("TROCOU");
    sleep(SELETOR + Duration::from_secs(2)).await;
    assert_eq!(ABERTAS.load(Ordering::SeqCst), 2);
    assert_eq!(VIVAS.load(Ordering::SeqCst), 1, "a captura antiga não saiu");
    assert_eq!(tela(&me).expect("despublicou").sid(), antes, "republicou");
    andando(&me).await;

    let web = web.await.unwrap();
    let saida = String::from_utf8_lossy(&web.stdout);
    println!("{saida}{}", String::from_utf8_lossy(&web.stderr));
    let linha = |nome: &str| {
        saida
            .lines()
            .find_map(|l| l.strip_prefix(nome))
            .unwrap_or_else(|| panic!("faltou {nome}"))
            .trim()
            .to_owned()
    };
    assert_eq!(
        linha("FAIXAS").matches(',').count(),
        0,
        "o Chrome viu outra faixa"
    );
    assert_eq!(linha("SEM_VIDEO"), "0", "o vídeo sumiu no meio");
    assert!(
        linha("TAMANHOS").contains("900x720"),
        "o Chrome não viu a tela nova"
    );
    assert!(banca.estados.try_recv().is_err(), "a troca mexeu no estado");

    banca.parar(&me).await;
    let _ = room.close().await;
    println!("FIM");
}

async fn evento(rx: &mut UnboundedReceiver<Event>) -> Event {
    timeout(ESPERA, rx.recv())
        .await
        .expect("evento não veio")
        .expect("a call acabou sem Ended")
}

/// Caminho inteiro por `Call`, com motor de áudio de verdade. Fora do `audio/bancada.sh`
/// o motor abriria o mic da máquina.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bancada_tela_call() {
    assert!(
        std::env::var_os("BANCADA_RAIZ").is_some(),
        "rode por src/audio/bancada.sh, num PipeWire próprio"
    );
    let _ = Command::new("pactl")
        .args(["load-module", "module-null-sink", "sink_name=bancada"])
        .output();
    let prefs = AudioPrefs {
        input: None,
        output: None,
        noise: NoiseLevel::Off,
        sensitivity: Sensitivity::Auto,
        master: 71,
        volumes: HashMap::new(),
    };
    let (engine, mic): (Engine, Receiver<Vec<i16>>) = Engine::start(prefs).expect("motor");
    let client = cliente().await;
    let (call, mut events) = Call::join_with(
        client,
        var("CONCORD_ROOM"),
        INSTALL.to_owned(),
        engine,
        mic,
        true,
        sintetica,
    );

    loop {
        if evento(&mut events).await == Event::Connected {
            break;
        }
    }
    call.start_share(Quality::Economica, false, None);
    let local_no_ar = |e: &Event| match e {
        Event::Participants(list) => list.iter().any(|p| p.is_local && p.screen_share),
        _ => false,
    };
    let mut estados = vec![];
    while estados.last() != Some(&State::Live) {
        let e = evento(&mut events).await;
        assert!(!local_no_ar(&e) || !estados.is_empty(), "LIVE sem quadro");
        if let Event::Share(state) = e {
            estados.push(state);
        }
    }
    assert_eq!(estados, [State::Choosing, State::Live]);
    println!("PUBLICADO pela call");
    sleep(Duration::from_secs(segundos())).await;

    call.leave();
    let mut viu_off = false;
    loop {
        match evento(&mut events).await {
            Event::Share(State::Off) => viu_off = true,
            Event::Ended(end) => {
                assert_eq!(end, End::Left);
                break;
            }
            _ => {}
        }
    }
    assert!(viu_off, "sair não parou a transmissão");
    solta().await;
    println!("FIM");
}

/// Tela com áudio até o Chrome, com apps falsos num PipeWire isolado: o jogo tem que chegar
/// nos dois lados certos, e a aba "Concord", este processo e o mic, nunca. Rode por
/// `src/audio/bancada.sh bancada_tela_audio`, com as variáveis do `share/bancada.sh` e `OUT`.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bancada_tela_audio() {
    super::app_audio::bancada::isolado();
    let _sons = super::app_audio::bancada::Sons::start();
    let client = cliente().await;
    let room = entrar(&client).await;
    let me = room.local_participant();
    let mut banca = Banca::new(me.clone());
    banca
        .cmd
        .send(Cmd::Start(Quality::Media, true, None))
        .unwrap();
    assert_eq!(banca.estado().await, State::Choosing);
    assert_eq!(banca.estado().await, State::Live);
    som_no_ar(&me).await;
    println!("PUBLICADO com áudio");

    let web = tokio::task::spawn_blocking(|| {
        Command::new("node")
            .arg("src/share/ouve-tela.mjs")
            .output()
            .expect("node")
    });
    sleep(Duration::from_secs(10)).await;
    println!("{}", som_enviado(&me).await);
    let web = web.await.unwrap();
    let saida = String::from_utf8_lossy(&web.stdout);
    println!("{saida}{}", String::from_utf8_lossy(&web.stderr));
    let depois = saida
        .lines()
        .find(|l| l.starts_with("DEPOIS"))
        .expect("DEPOIS");
    assert!(depois.contains("440="), "o Chrome não recebeu o áudio");

    // Sala nova: o áudio volta junto do vídeo, sem abrir o seletor de novo.
    banca.room.send_replace(None);
    let _ = room.close().await;
    drop((room, me));
    sleep(Duration::from_secs(3)).await;
    let room = entrar(&client).await;
    let me = room.local_participant();
    banca.room.send_replace(Some(me.clone()));
    assert_eq!(banca.estado().await, State::Live);
    som_no_ar(&me).await;
    sleep(Duration::from_secs(3)).await;
    println!("SALA NOVA {}", som_enviado(&me).await);
    assert_eq!(ABERTAS.load(Ordering::SeqCst), 1, "abriu o seletor de novo");

    banca.parar(&me).await;
    let _ = room.close().await;
    println!("FIM");
}

/// Chrome falando a fala TTS na sala, e a call do cliente tem que ouvir em toda volta:
/// entrar e sair com o som de saída tocando, prévia e assistir a tela de outro, e queda
/// pelo SDK. Rode por `src/audio/bancada.sh bancada_ouve`, com as variáveis do
/// `share/bancada.sh` e `OUT`.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bancada_ouve() {
    use crate::audio::{
        Effect,
        bancada::{Killed, level},
    };
    assert!(
        std::env::var_os("BANCADA_RAIZ").is_some(),
        "rode por src/audio/bancada.sh, num PipeWire próprio"
    );
    for args in [
        "load-module module-null-sink sink_name=bancada_ouve",
        "set-default-sink bancada_ouve",
    ] {
        let _ = Command::new("pactl").args(args.split(' ')).output();
    }
    let _chrome = Killed(
        Command::new("node")
            .arg(format!(
                "{}/ouvinte.mjs",
                option_env!("CONCORD_BANCADA").unwrap_or_default()
            ))
            .env(
                "FAKE_WAV",
                format!(
                    "{}/Portuguese_stable.wav",
                    option_env!("CONCORD_VOZES").unwrap_or_default()
                ),
            )
            .env("WAIT_MS", "900000")
            .spawn()
            .expect("node"),
    );
    let client = cliente().await;
    // Outra pessoa transmitindo: dá a prévia ao entrar e alguém pra assistir.
    let room = entrar(&client).await;
    let me = room.local_participant();
    let mut banca = Banca::new(me.clone());
    banca.ao_vivo(&me, Quality::Economica).await;
    let tela = me.identity().to_string();

    let mut mudas: Vec<String> = vec![];
    let mut ouvir = async |what: String| {
        // Duas janelas: a fala tem pausa entre as frases.
        let rms = tokio::task::spawn_blocking(|| {
            level("bancada_ouve.monitor").max(level("bancada_ouve.monitor"))
        })
        .await
        .unwrap();
        println!("OUVE {what}: rms={rms:.4}");
        if rms < 0.01 {
            mudas.push(what);
        }
    };
    let prefs = AudioPrefs {
        input: None,
        output: None,
        noise: NoiseLevel::Off,
        sensitivity: Sensitivity::Auto,
        master: 71,
        volumes: HashMap::new(),
    };
    for volta in 0..6 {
        let (engine, mic) = Engine::start(prefs.clone()).expect("motor");
        let (call, mut events) = Call::join_with(
            client.clone(),
            var("CONCORD_ROOM"),
            "bancada-ouve".to_owned(),
            engine.clone(),
            mic,
            true,
            sintetica,
        );
        // Conectado e com o Chrome de mic aberto na lista.
        let mut conectou = false;
        loop {
            match evento(&mut events).await {
                Event::Connected => conectou = true,
                Event::Participants(list)
                    if conectou
                        && list
                            .iter()
                            .any(|p| !p.is_local && p.identity != tela && !p.muted) =>
                {
                    break;
                }
                _ => {}
            }
        }
        sleep(Duration::from_secs(2)).await;
        ouvir(format!("volta {volta}")).await;
        if volta == 1 {
            for vez in 0..3 {
                call.watch(Some(tela.clone()));
                sleep(Duration::from_secs(3)).await;
                ouvir(format!("assistindo {vez}")).await;
                call.watch(None);
                sleep(Duration::from_secs(2)).await;
                ouvir(format!("parou de assistir {vez}")).await;
            }
        }
        if volta == 2 {
            for scenario in [
                SimulateScenario::SignalReconnect,
                SimulateScenario::FullReconnect,
                SimulateScenario::ServerLeave,
                SimulateScenario::NodeFailure,
                SimulateScenario::Migration,
            ] {
                call.simulate(scenario);
                let (mut caiu, inicio) = (false, Instant::now());
                while inicio.elapsed() < Duration::from_secs(40) {
                    match timeout(Duration::from_secs(1), events.recv()).await {
                        Ok(Some(Event::Reconnecting { .. })) => caiu = true,
                        Ok(Some(Event::Connected)) if caiu => break,
                        Ok(Some(Event::Ended(end))) => panic!("{scenario:?} acabou: {end:?}"),
                        _ => {}
                    }
                }
                println!("CENARIO {scenario:?}: caiu={caiu}");
                sleep(Duration::from_secs(4)).await;
                ouvir(format!("{scenario:?}")).await;
            }
        }
        // Como o botão de sair: o som, e o motor cai com ele tocando.
        engine.play(Effect::Leave);
        call.leave();
        drop((engine, call));
        while !matches!(evento(&mut events).await, Event::Ended(_)) {}
    }
    banca.parar(&me).await;
    let _ = room.close().await;
    assert!(mudas.is_empty(), "sem voz em: {mudas:?}");
}
