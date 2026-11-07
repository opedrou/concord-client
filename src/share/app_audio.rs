//! Áudio da transmissão, no Linux. Um `pw-record` filho, sem destino, recebe por `pw-link`
//! as portas de saída dos apps escolhidos e entrega PCM cru pela stdout. As regras de qual
//! node é app e qual é a call são as do `AUDIO-APP-LINUX.md`, seção 4.
//!
//! Por que não `module-null-sink` + `parec`, como no site: o módulo mora no
//! `pipewire-pulse` e fica pendurado se o processo cair. O node do `pw-record` morre com
//! ele, e os links morrem com o node.
use std::{
    collections::HashMap,
    io::Read,
    process::{Child, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

use futures_util::FutureExt;
use livekit::webrtc::{
    audio_source::native::NativeAudioSource,
    prelude::{AudioFrame, AudioSourceOptions},
};
use serde_json::Value;

use crate::audio::{FRAME, RATE};

/// Nome da faixa: é por ele que o site reconhece o áudio de app (`APP_AUDIO_TRACK_NAME`).
pub const TRACK: &str = "concord-app-audio";
const CHANNELS: u32 = 2;
/// App e jogo recriam stream: a cada volta os links são refeitos pelo grafo.
const POLL: Duration = Duration::from_secs(2);
/// O `pw-record` leva uns ms pra aparecer no grafo: nos primeiros [`BIRTH`], tenta de novo
/// a cada [`RETRY`].
const RETRY: Duration = Duration::from_millis(100);
const BIRTH: Duration = Duration::from_secs(3);
/// `node.always-process`: sem link nenhum ele segue escrevendo silêncio. Assim a sala
/// recebe 10 ms a cada 10 ms, e o `pw-record` morre de SIGPIPE se este processo cair.
const PROPS: &str = "{ node.name = concord-tela, media.name = \"Concord: áudio da tela\", node.always-process = true }";

/// O que vai junto da transmissão.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Choice {
    #[default]
    Silent,
    /// Todos os apps, menos o Concord.
    All,
    App(String),
}

/// Pra interface: a escolha atual e os apps tocando agora.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub choice: Choice,
    pub apps: Vec<String>,
    /// Janela sem app óbvio: a interface abre a lista pra pessoa escolher.
    pub ask: bool,
}

/// Captura em andamento. Soltar mata o `pw-record`, e as threads saem sozinhas.
pub struct AppAudio {
    child: Child,
    choose: Sender<Choice>,
    pub source: NativeAudioSource,
}

impl AppAudio {
    /// `frame` é o tamanho do primeiro quadro do vídeo: decide entre tela e janela.
    pub fn start(frame: (u32, u32), notify: impl Fn(View) + Send + 'static) -> Option<Self> {
        let mut child = Command::new("pw-record")
            .args([
                "--target",
                "0",
                "--rate",
                "48000",
                "--channels",
                "2",
                "--format",
                "s16",
            ])
            .args(["--latency", "20ms", "--raw", "-P", PROPS, "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take();
        let (choose, choices) = mpsc::channel();
        // Sem AEC, NS nem AGC: são feitos pra voz e mutilam música e jogo.
        let source = NativeAudioSource::new(AudioSourceOptions::default(), RATE, CHANNELS, 0);
        let audio = Self {
            child,
            choose,
            source,
        };
        let (stdout, feed) = (stdout?, audio.source.clone());
        let recorder = audio.child.id();
        thread::Builder::new()
            .name("concord-tela-som".into())
            .spawn(move || pump(stdout, feed))
            .ok()?;
        thread::Builder::new()
            .name("concord-tela-links".into())
            .spawn(move || watch(recorder, frame, choices, notify))
            .ok()?;
        Some(audio)
    }

    pub fn choose(&self, choice: Choice) {
        let _ = self.choose.send(choice);
    }
}

impl Drop for AppAudio {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Lê blocos de 10 ms até a stdout fechar. Fila zero entrega na hora: o `capture_frame`
/// já volta pronto, sem precisar de runtime.
fn pump(mut out: ChildStdout, source: NativeAudioSource) {
    let mut bytes = vec![0u8; FRAME * CHANNELS as usize * 2];
    let mut samples = vec![0i16; FRAME * CHANNELS as usize];
    while out.read_exact(&mut bytes).is_ok() {
        for (s, b) in samples.iter_mut().zip(bytes.chunks_exact(2)) {
            *s = i16::from_le_bytes([b[0], b[1]]);
        }
        let frame = AudioFrame {
            data: samples.as_slice().into(),
            sample_rate: RATE,
            num_channels: CHANNELS,
            samples_per_channel: FRAME as u32,
        };
        let _ = source.capture_frame(&frame).now_or_never();
    }
}

/// Mantém os links de acordo com a escolha, a cada [`POLL`] ou na hora em que ela muda.
fn watch(recorder: u32, frame: (u32, u32), choices: Receiver<Choice>, notify: impl Fn(View)) {
    let screen = is_screen(frame, monitors());
    let me = u64::from(std::process::id());
    let born = Instant::now();
    let mut choice = None;
    let mut ask = false;
    let mut shown = None;
    loop {
        let mut wait = POLL;
        if let Some(graph) = dump().map(|d| Graph::parse(&d, me, recorder.into())) {
            let apps = graph.apps();
            let current = choice.get_or_insert_with(|| {
                let (c, a) = initial(screen, &apps);
                ask = a;
                c
            });
            if graph.inputs.is_empty() && born.elapsed() < BIRTH {
                wait = RETRY;
            }
            let (add, remove) = graph.plan(current);
            for (out, input) in add {
                pw_link(&[out.to_string(), input.to_string()]);
            }
            for link in remove {
                pw_link(&["-d".into(), link.to_string()]);
            }
            let view = View {
                choice: current.clone(),
                apps,
                ask,
            };
            ask = false;
            if shown.as_ref() != Some(&view) {
                notify(view.clone());
                shown = Some(view);
            }
        }
        match choices.recv_timeout(wait) {
            Ok(c) => choice = Some(c),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn dump() -> Option<Vec<Value>> {
    let out = Command::new("pw-dump")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

fn pw_link(args: &[String]) {
    let _ = Command::new("pw-link")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Tela inteira quando o quadro tem o tamanho de um monitor, em qualquer orientação. Sem
/// a lista de monitores, também.
///
/// ponytail: janela em tela cheia tem o tamanho do monitor e é vista como tela. Saber a
/// janela de verdade exige o restore data do portal, que o libwebrtc não expõe.
fn is_screen(frame: (u32, u32), monitors: Option<Vec<(u32, u32)>>) -> bool {
    match monitors.filter(|m| !m.is_empty()) {
        Some(m) => m.iter().any(|&(w, h)| frame == (w, h) || frame == (h, w)),
        None => true,
    }
}

/// Escolha antes de a pessoa mexer, e se a lista abre sozinha.
fn initial(screen: bool, apps: &[String]) -> (Choice, bool) {
    match apps {
        _ if screen => (Choice::All, false),
        [one] => (Choice::App(one.clone()), false),
        _ => (Choice::Silent, true),
    }
}

/// Resolução atual de cada monitor, pelo Mutter. `None` fora do GNOME.
#[cfg(target_os = "linux")]
fn monitors() -> Option<Vec<(u32, u32)>> {
    use zbus::zvariant::OwnedValue;
    type Props = HashMap<String, OwnedValue>;
    type Mode = (String, i32, i32, f64, f64, Vec<f64>, Props);
    type Monitor = ((String, String, String, String), Vec<Mode>, Props);
    type Logical = (
        i32,
        i32,
        f64,
        u32,
        bool,
        Vec<(String, String, String, String)>,
        Props,
    );
    let name = "org.gnome.Mutter.DisplayConfig";
    let reply = zbus::blocking::Connection::session()
        .ok()?
        .call_method(
            Some(name),
            "/org/gnome/Mutter/DisplayConfig",
            Some(name),
            "GetCurrentState",
            &(),
        )
        .ok()?;
    let (_, monitors, _, _): (u32, Vec<Monitor>, Vec<Logical>, Props) =
        reply.body().deserialize().ok()?;
    let current = |m: &&Mode| m.6.get("is-current").and_then(|v| bool::try_from(v).ok());
    Some(
        monitors
            .iter()
            .flat_map(|(_, modes, _)| modes.iter().filter(|m| current(m) == Some(true)))
            .map(|m| (m.1 as u32, m.2 as u32))
            .collect(),
    )
}

#[cfg(not(target_os = "linux"))]
fn monitors() -> Option<Vec<(u32, u32)>> {
    None
}

/// Lado do estéreo de cada canal: *L na esquerda, *R na direita, o resto (MONO, FC, LFE)
/// nos dois. O PipeWire soma os links que chegam na mesma porta.
fn sides(channel: &str) -> &'static [&'static str] {
    if channel.ends_with('L') {
        &["FL"]
    } else if channel.ends_with('R') {
        &["FR"]
    } else {
        &["FL", "FR"]
    }
}

/// Portas de um node: (id, canal).
type Ports = Vec<(u64, String)>;

struct Stream {
    app: String,
    running: bool,
    /// As de saída.
    ports: Ports,
}

/// O pedaço do `pw-dump` que importa.
struct Graph {
    /// Streams de saída de app, já sem o Concord.
    streams: Vec<Stream>,
    /// Portas de entrada do `pw-record`.
    inputs: Ports,
    /// Links que chegam no `pw-record`: (id, porta de saída, porta de entrada).
    links: Vec<(u64, u64, u64)>,
}

fn num(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

impl Graph {
    /// `me` é o PID deste processo; `recorder`, o do `pw-record`.
    fn parse(dump: &[Value], me: u64, recorder: u64) -> Self {
        let of = |kind: &'static str| {
            dump.iter()
                .filter(move |o| o["type"] == kind)
                .map(|o| (num(&o["id"]), &o["info"]))
        };
        let clients: HashMap<_, _> = of("PipeWire:Interface:Client")
            .map(|(id, info)| (id, &info["props"]))
            .collect();
        let mut ports: HashMap<(Option<u64>, &str), Ports> = HashMap::new();
        for (id, info) in of("PipeWire:Interface:Port") {
            let p = &info["props"];
            let (Some(id), Some(dir)) = (id, p["port.direction"].as_str()) else {
                continue;
            };
            if p["port.monitor"] == true || p["port.monitor"] == "true" {
                continue;
            }
            let channel = p["audio.channel"].as_str().unwrap_or("MONO").to_owned();
            ports
                .entry((num(&p["node.id"]), dir))
                .or_default()
                .push((id, channel));
        }

        let mut graph = Graph {
            streams: Vec::new(),
            inputs: Vec::new(),
            links: Vec::new(),
        };
        let mut recorder_node = None;
        for (id, info) in of("PipeWire:Interface:Node") {
            let p = &info["props"];
            // Só objetos Node: o Client também carrega `media.class`, e não tem porta.
            let client = clients.get(&num(&p["client.id"])).copied();
            let client = client.unwrap_or(&Value::Null);
            // O Spotify não põe o PID no node, só no Client.
            let pid = num(&p["application.process.id"])
                .or_else(|| num(&client["application.process.id"]));
            let mut take = |dir| ports.remove(&(id, dir)).unwrap_or_default();
            match p["media.class"].as_str() {
                Some("Stream/Input/Audio") if pid == Some(recorder) => {
                    recorder_node = id;
                    graph.inputs = take("in");
                }
                Some("Stream/Output/Audio") => {
                    let media = p["media.name"].as_str().unwrap_or("");
                    // A própria call, a aba "Concord" do Firefox, e loopback ou filtro
                    // (eco, equalizador), que carregam a mistura com a call dentro.
                    let call = pid == Some(me) || media.to_lowercase().contains("concord");
                    if call || !p["node.link-group"].is_null() {
                        continue;
                    }
                    let app = [&client["application.name"], &p["application.name"]]
                        .into_iter()
                        .chain([&p["node.name"]])
                        .find_map(Value::as_str)
                        .unwrap_or("?")
                        .to_owned();
                    graph.streams.push(Stream {
                        app,
                        running: info["state"] == "running",
                        ports: take("out"),
                    });
                }
                _ => {}
            }
        }

        if recorder_node.is_some() {
            graph.links = of("PipeWire:Interface:Link")
                .filter(|(_, info)| num(&info["input-node-id"]) == recorder_node)
                .filter_map(|(id, info)| {
                    Some((
                        id?,
                        num(&info["output-port-id"])?,
                        num(&info["input-port-id"])?,
                    ))
                })
                .collect();
        }
        graph
    }

    /// Apps com som agora, em ordem e sem repetir: um app pode ter vários clients.
    fn apps(&self) -> Vec<String> {
        let mut apps: Vec<_> = self
            .streams
            .iter()
            .filter(|s| s.running)
            .map(|s| s.app.clone())
            .collect();
        apps.sort();
        apps.dedup();
        apps
    }

    /// Links a criar (saída, entrada) e ids de link a desfazer. Tudo que chega no
    /// `pw-record` e não foi pedido sai, inclusive o que não fomos nós que ligamos.
    fn plan(&self, choice: &Choice) -> (Vec<(u64, u64)>, Vec<u64>) {
        let input = |side: &str| self.inputs.iter().find(|(_, c)| c == side).map(|p| p.0);
        let want: Vec<(u64, u64)> = self
            .streams
            .iter()
            .filter(|s| match choice {
                Choice::Silent => false,
                Choice::All => true,
                Choice::App(app) => &s.app == app,
            })
            .flat_map(|s| &s.ports)
            .flat_map(|(port, ch)| {
                sides(ch)
                    .iter()
                    .filter_map(move |s| Some((*port, input(s)?)))
            })
            .collect();
        let add = want
            .iter()
            .filter(|w| !self.links.iter().any(|l| (l.1, l.2) == **w))
            .copied()
            .collect();
        let remove = self
            .links
            .iter()
            .filter(|l| !want.contains(&(l.1, l.2)))
            .map(|l| l.0)
            .collect();
        (add, remove)
    }
}

/// Apps falsos num PipeWire isolado, pras bancadas. Nunca na sessão de verdade.
#[cfg(test)]
pub(super) mod bancada {
    use std::f64::consts::TAU;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use futures_util::StreamExt;
    use livekit::webrtc::{audio_stream::native::NativeAudioStream, prelude::RtcAudioTrack};

    use super::*;

    /// Para o teste se `pw-link` fosse cair na sessão: só roda por `src/audio/bancada.sh`.
    pub fn isolado() {
        let raiz = std::env::var("BANCADA_RAIZ").expect("rode por src/audio/bancada.sh");
        let pw = std::env::var("PIPEWIRE_RUNTIME_DIR").unwrap_or_default();
        assert_eq!(pw, raiz, "pw-link cairia no PipeWire da sessão");
    }

    /// Jogo em estéreo (440 Hz na esquerda, 1000 na direita), a aba "Concord" do Firefox
    /// (330), este processo (770) e um mic (660). Só o jogo pode chegar na sala.
    pub struct Sons {
        filhos: Vec<Child>,
        _eu: cpal::Stream,
    }

    pub const JOGO: [f64; 2] = [440.0, 1000.0];
    pub const PROIBIDOS: [f64; 3] = [330.0, 770.0, 660.0];

    /// Toca `input` como o app `name`, num stream chamado `stream`.
    fn ffmpeg(input: &str, name: &str, stream: &str) -> Child {
        Command::new("ffmpeg")
            .args(["-loglevel", "error", "-re"])
            .args(input.split(' '))
            .args([
                "-f",
                "pulse",
                "-name",
                name,
                "-stream_name",
                stream,
                "default",
            ])
            .stdin(Stdio::null())
            .spawn()
            .expect("ffmpeg")
    }

    impl Sons {
        pub fn start() -> Self {
            let raiz = std::env::var("BANCADA_RAIZ").unwrap();
            let mic = format!("{raiz}/mic.wav");
            let ok = Command::new("ffmpeg")
                .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .args(["sine=frequency=660:duration=120", "-ac", "1", &mic])
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "mic.wav");
            let filhos = vec![
                ffmpeg(
                    "-f lavfi -i sine=frequency=440 -f lavfi -i sine=frequency=1000 \
                     -filter_complex amerge=inputs=2",
                    "Jogo",
                    "jogo",
                ),
                ffmpeg("-f lavfi -i sine=frequency=330", "Firefox", "Concord"),
                Command::new("pw-play")
                    .args([
                        "-P",
                        "{ media.class = Audio/Source, node.name = alsa_input.falso }",
                    ])
                    .arg(&mic)
                    .spawn()
                    .expect("pw-play"),
            ];
            thread::sleep(Duration::from_millis(500));
            let _ = Command::new("pactl")
                .args(["set-default-source", "alsa_input.falso"])
                .status();
            Self {
                filhos,
                _eu: tom_proprio(),
            }
        }
    }

    impl Drop for Sons {
        fn drop(&mut self) {
            for f in &mut self.filhos {
                let _ = f.kill();
                let _ = f.wait();
            }
        }
    }

    /// Saída deste processo, como a voz dos outros saindo do Concord.
    fn tom_proprio() -> cpal::Stream {
        let host = cpal::host_from_id(cpal::HostId::PulseAudio).unwrap();
        let device = host.default_output_device().expect("saída");
        let config = cpal::StreamConfig {
            channels: 2,
            sample_rate: RATE,
            buffer_size: cpal::BufferSize::Default,
        };
        let mut n = 0u64;
        let stream = device
            .build_output_stream(
                config,
                move |out: &mut [f32], _| {
                    for f in out.chunks_mut(2) {
                        let v = (TAU * 770.0 * n as f64 / f64::from(RATE)).sin() * 0.125;
                        f.fill(v as f32);
                        n += 1;
                    }
                },
                |_| {},
                None,
            )
            .expect("stream");
        stream.play().unwrap();
        stream
    }

    /// Valor RMS da componente `hz` de um canal, em amostras de 16 bits.
    pub fn tom(x: &[i16], canal: usize, hz: f64) -> f64 {
        let (mut s, mut c, mut n) = (0.0, 0.0, 0f64);
        for (i, v) in x.iter().skip(canal).step_by(2).enumerate() {
            let a = TAU * hz * i as f64 / f64::from(RATE);
            s += f64::from(*v) * a.sin();
            c += f64::from(*v) * a.cos();
            n += 1.0;
        }
        2f64.sqrt() * s.hypot(c) / n.max(1.0)
    }

    /// Ouve a faixa pela mesma porta que o WebRTC: o que sair daqui é o que vai pra sala.
    pub struct Ouvido(NativeAudioStream);

    impl Ouvido {
        pub fn new(track: RtcAudioTrack) -> Self {
            Self(NativeAudioStream::new(track, RATE as i32, 2))
        }

        /// Um segundo, depois de jogar fora o que estava na fila.
        pub async fn segundo(&mut self) -> Vec<i16> {
            while self.0.next().now_or_never().flatten().is_some() {}
            let mut x = Vec::new();
            while x.len() < RATE as usize * 2 {
                let f = self.0.next().await.expect("a faixa acabou");
                x.extend_from_slice(&f.data);
            }
            x
        }
    }

    /// Uma linha com o nível de cada tom em cada lado.
    pub fn niveis(x: &[i16]) -> String {
        let hz = JOGO.iter().chain(&PROIBIDOS);
        let lado = |c| hz.clone().map(move |h| format!("{h}={:.0}", tom(x, c, *h)));
        let l: Vec<_> = lado(0).collect();
        let r: Vec<_> = lado(1).collect();
        format!("L[{}] R[{}]", l.join(" "), r.join(" "))
    }

    /// Só o jogo, e no lado certo; nada do que é proibido.
    pub fn so_o_jogo(x: &[i16]) {
        assert!(
            tom(x, 0, JOGO[0]) > 1000.0 && tom(x, 1, JOGO[1]) > 1000.0,
            "{}",
            niveis(x)
        );
        assert!(
            tom(x, 0, JOGO[1]) < 30.0 && tom(x, 1, JOGO[0]) < 30.0,
            "{}",
            niveis(x)
        );
        for (c, h) in [0, 1].into_iter().flat_map(|c| PROIBIDOS.map(|h| (c, h))) {
            assert!(tom(x, c, h) < 30.0, "{h} Hz vazou: {}", niveis(x));
        }
    }

    /// Apps com link chegando no `pw-record`. Porta que não é de app vira `FORA`.
    pub fn ligados(recorder: u32) -> Vec<String> {
        let g = Graph::parse(
            &dump().unwrap(),
            u64::from(std::process::id()),
            recorder.into(),
        );
        let mut apps: Vec<_> = g
            .links
            .iter()
            .map(|l| {
                g.streams
                    .iter()
                    .find(|s| s.ports.iter().any(|p| p.0 == l.1))
                    .map_or(format!("FORA porta {}", l.1), |s| s.app.clone())
            })
            .collect();
        apps.sort();
        apps.dedup();
        apps
    }

    /// O `pw-link -l` inteiro do PipeWire isolado, pro relatório.
    pub fn topologia() -> String {
        let out = Command::new("pw-link").arg("-l").output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EU: u64 = 4242;
    const GRAVADOR: u64 = 5000;

    fn client(id: u64, name: &str, pid: u64) -> Value {
        json!({"id": id, "type": "PipeWire:Interface:Client", "info": {"props": {
            "application.name": name, "application.process.id": pid,
            // O Client também tem `media.class`: não pode virar node.
            "media.class": "Stream/Output/Audio"}}})
    }

    fn node(id: u64, client: u64, state: &str, props: Value) -> Value {
        let mut p = props;
        p["client.id"] = json!(client);
        json!({"id": id, "type": "PipeWire:Interface:Node", "info": {"state": state, "props": p}})
    }

    fn port(id: u64, node: u64, dir: &str, ch: &str) -> Value {
        json!({"id": id, "type": "PipeWire:Interface:Port", "info": {"props": {
            "node.id": node, "port.direction": dir, "audio.channel": ch}}})
    }

    fn link(id: u64, out: u64, input: u64, input_node: u64) -> Value {
        json!({"id": id, "type": "PipeWire:Interface:Link", "info": {
            "output-port-id": out, "input-port-id": input, "input-node-id": input_node}})
    }

    /// Firefox com uma aba de vídeo e a aba da call, Spotify com o PID só no Client, Chrome
    /// em dois processos, o próprio Concord, o mic, um filtro de eco e o `pw-record`.
    fn grafo() -> Graph {
        let out = "Stream/Output/Audio";
        let dump = vec![
            client(1, "Firefox", 100),
            node(
                10,
                1,
                "running",
                json!({"media.class": out, "media.name": "YouTube",
                "application.name": "Firefox", "application.process.id": 100}),
            ),
            port(11, 10, "out", "FL"),
            port(12, 10, "out", "FR"),
            node(
                13,
                1,
                "running",
                json!({"media.class": out, "media.name": "Concord",
                "application.name": "Firefox", "application.process.id": 100}),
            ),
            port(14, 13, "out", "FL"),
            port(15, 13, "out", "FR"),
            client(2, "Spotify", 200),
            node(
                20,
                2,
                "running",
                json!({"media.class": out, "node.name": "spotify",
                "media.name": "Spotify"}),
            ),
            port(21, 20, "out", "FL"),
            port(22, 20, "out", "FR"),
            client(3, "cpal-pulseaudio-4242", EU),
            node(
                30,
                3,
                "running",
                json!({"media.class": out,
                "node.name": "cpal-pulseaudio-4242"}),
            ),
            port(31, 30, "out", "FL"),
            port(32, 30, "out", "FR"),
            client(4, "pipewire", 1),
            node(
                40,
                4,
                "running",
                json!({"media.class": "Audio/Source",
                "node.name": "alsa_input.usb-Fifine"}),
            ),
            port(41, 40, "out", "MONO"),
            node(
                42,
                4,
                "running",
                json!({"media.class": out, "node.name": "echo-cancel-playback",
                "node.link-group": "echo-cancel-1-30"}),
            ),
            port(43, 42, "out", "FL"),
            client(5, "Google Chrome", 300),
            node(
                50,
                5,
                "running",
                json!({"media.class": out, "media.name": "Playback"}),
            ),
            port(51, 50, "out", "MONO"),
            client(6, "Google Chrome", 301),
            node(
                60,
                6,
                "idle",
                json!({"media.class": out, "media.name": "Playback"}),
            ),
            port(61, 60, "out", "FL"),
            port(62, 60, "out", "FR"),
            client(7, "speech-dispatcher-dummy", 400),
            node(70, 7, "idle", json!({"media.class": out})),
            port(71, 70, "out", "FL"),
            client(9, "pw-cat", GRAVADOR),
            node(
                90,
                9,
                "running",
                json!({"media.class": "Stream/Input/Audio",
                "node.name": "concord-tela"}),
            ),
            port(91, 90, "in", "FL"),
            port(92, 90, "in", "FR"),
            json!({"id": 93, "type": "PipeWire:Interface:Port", "info": {"props": {
                "node.id": 90, "port.direction": "out", "audio.channel": "FL",
                "port.monitor": true}}}),
            // Um link nosso de antes e um que ninguém pediu, do mic.
            link(95, 11, 91, 90),
            link(96, 41, 92, 90),
            // Link do Firefox pro fone: não chega no `pw-record`, fica fora da conta.
            link(97, 12, 500, 80),
        ];
        Graph::parse(&dump, EU, GRAVADOR)
    }

    fn ordenado(mut v: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
        v.sort();
        v
    }

    #[test]
    fn parser_tira_a_call_o_mic_e_o_filtro() {
        let g = grafo();
        assert_eq!(g.apps(), ["Firefox", "Google Chrome", "Spotify"]);
        let nomes: Vec<_> = g.streams.iter().map(|s| s.app.as_str()).collect();
        assert_eq!(
            nomes,
            [
                "Firefox",
                "Spotify",
                "Google Chrome",
                "Google Chrome",
                "speech-dispatcher-dummy"
            ],
            "sem a aba Concord, sem o próprio processo, sem o eco"
        );
        assert_eq!(g.inputs, [(91, "FL".into()), (92, "FR".into())]);
        assert_eq!(g.links, [(95, 11, 91), (96, 41, 92)]);
    }

    #[test]
    fn computador_todo_liga_tudo_menos_a_call_e_desfaz_o_mic() {
        let (add, remove) = grafo().plan(&Choice::All);
        assert_eq!(
            ordenado(add),
            [
                (12, 92),
                (21, 91),
                (22, 92),
                (51, 91),
                (51, 92),
                (61, 91),
                (62, 92),
                (71, 91)
            ]
        );
        assert_eq!(remove, [96]);
    }

    #[test]
    fn um_app_leva_todos_os_clients_dele() {
        let (add, remove) = grafo().plan(&Choice::App("Google Chrome".into()));
        assert_eq!(ordenado(add), [(51, 91), (51, 92), (61, 91), (62, 92)]);
        assert_eq!(remove, [95, 96]);

        let (add, remove) = grafo().plan(&Choice::App("Firefox".into()));
        assert_eq!(add, [(12, 92)], "a aba da call nunca entra");
        assert_eq!(remove, [96]);
    }

    #[test]
    fn sem_audio_desfaz_tudo() {
        let (add, remove) = grafo().plan(&Choice::Silent);
        assert!(add.is_empty());
        assert_eq!(remove, [95, 96]);
    }

    #[test]
    fn sem_o_gravador_no_grafo_nao_liga_nada() {
        let g = Graph::parse(&[], EU, GRAVADOR);
        assert_eq!(g.plan(&Choice::All), (vec![], vec![]));
        let mut g = grafo();
        g.inputs.clear();
        g.links.clear();
        assert_eq!(g.plan(&Choice::All), (vec![], vec![]));
    }

    #[test]
    fn canais_vao_pro_lado_certo() {
        for (ch, lado) in [
            ("FL", &["FL"][..]),
            ("FR", &["FR"]),
            ("RL", &["FL"]),
            ("SR", &["FR"]),
            ("MONO", &["FL", "FR"]),
            ("FC", &["FL", "FR"]),
            ("LFE", &["FL", "FR"]),
        ] {
            assert_eq!(sides(ch), lado, "{ch}");
        }
    }

    #[test]
    fn tela_ou_janela_pelo_tamanho_do_quadro() {
        let monitores = || Some(vec![(1280, 1024), (1600, 900)]);
        assert!(is_screen((1280, 1024), monitores()));
        assert!(is_screen((1600, 900), monitores()));
        assert!(is_screen((900, 1600), monitores()), "monitor em pé");
        assert!(!is_screen((1600, 868), monitores()), "janela maximizada");
        assert!(!is_screen((800, 600), monitores()));
        assert!(is_screen((800, 600), None), "sem DisplayConfig");
        assert!(is_screen((800, 600), Some(vec![])));
    }

    #[test]
    fn escolha_inicial() {
        let apps = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(initial(true, &apps(&[])), (Choice::All, false));
        assert_eq!(initial(true, &apps(&["A", "B"])), (Choice::All, false));
        assert_eq!(initial(false, &apps(&[])), (Choice::Silent, true));
        assert_eq!(
            initial(false, &apps(&["Jogo"])),
            (Choice::App("Jogo".into()), false)
        );
        assert_eq!(initial(false, &apps(&["A", "B"])), (Choice::Silent, true));
    }

    use bancada::{Ouvido, Sons};
    use livekit::{track::LocalAudioTrack, webrtc::prelude::RtcAudioSource};
    use tokio::{sync::mpsc::unbounded_channel, time::sleep};

    /// A cadeia de verdade, num PipeWire isolado: `src/audio/bancada.sh bancada_audio`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn bancada_audio_cadeia() {
        bancada::isolado();
        let _sons = Sons::start();
        sleep(Duration::from_secs(2)).await;
        let (tx, mut views) = unbounded_channel();
        let audio = AppAudio::start((800, 600), move |v| {
            let _ = tx.send(v);
        })
        .expect("pw-record");
        let recorder = audio.child.id();
        let janela = monitors().is_some();
        let view = views.recv().await.unwrap();
        println!("JANELA {janela} -> {view:?}");
        let primeira = if janela {
            Choice::App("Jogo".into())
        } else {
            Choice::All
        };
        assert_eq!(view.choice, primeira);
        assert_eq!(view.apps, ["Jogo"], "só o jogo conta como app tocando");
        assert!(!view.ask);

        let track = LocalAudioTrack::create_audio_track(
            "bancada",
            RtcAudioSource::Native(audio.source.clone()),
        );
        let mut ouvido = Ouvido::new(track.rtc_track());
        for choice in [
            primeira,
            Choice::All,
            Choice::Silent,
            Choice::App("Jogo".into()),
        ] {
            audio.choose(choice.clone());
            sleep(Duration::from_millis(500)).await;
            let ligados = bancada::ligados(recorder);
            let x = ouvido.segundo().await;
            println!("{choice:?}: ligados {ligados:?} {}", bancada::niveis(&x));
            if choice == Choice::All {
                println!("{}", bancada::topologia());
            }
            if choice == Choice::Silent {
                assert!(ligados.is_empty());
                assert!(x.iter().all(|v| *v == 0), "sem link tem que ser silêncio");
            } else {
                assert_eq!(ligados, ["Jogo"]);
                bancada::so_o_jogo(&x);
            }
        }

        drop(audio);
        assert!(
            !std::path::Path::new(&format!("/proc/{recorder}")).exists(),
            "o pw-record ficou vivo"
        );
        sleep(Duration::from_millis(300)).await;
        assert!(!bancada::topologia().contains("concord-tela"));
        println!("FIM");
    }

    /// Fica no ar pra derrubar o processo com `kill -9` de fora e ver o `pw-record` sumir.
    #[test]
    #[ignore]
    fn bancada_audio_queda() {
        bancada::isolado();
        let _rt = tokio::runtime::Runtime::new().unwrap();
        let audio = AppAudio::start((800, 600), |_| {}).expect("pw-record");
        println!("GRAVADOR {}", audio.child.id());
        thread::sleep(Duration::from_secs(60));
    }
}
