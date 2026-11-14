//! Bancada com o motor real, por `src/audio/bancada.sh`. Topologia do `rig.sh` do spike:
//! saída -> atraso de 50 ms -> "sala" -> mic falso, tudo em sinks nulos.
use std::{
    fs,
    io::Read,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use super::*;

// Pasta da bancada web (puppeteer e wav), por `CONCORD_BANCADA` na compilação.
const WAV_DIR: &str = match option_env!("CONCORD_BANCADA") {
    Some(p) => p,
    None => "",
};
/// Frases em português de TTS, fora do repositório como os wav do spike. `fala.wav` é
/// rajada sintética, não voz: o RNNoise quase nunca passa de 0,9 nela.
const VOZ_DIR: &str = match option_env!("CONCORD_VOZES") {
    Some(p) => p,
    None => "",
};
const VOZES: [&str; 6] = [
    "local_Portuguese",
    "local_PortugueseBusiness",
    "local_PortugueseGrandpaFarm",
    "Portuguese_stable",
    "Portuguese_expressive",
    "podcast_pizza",
];
const FONE: &str = "BancadaFone";
const MIC: &str = "BancadaMic";

fn pactl(args: &[&str]) -> String {
    let out = Command::new("pactl").args(args).output().expect("pactl");
    assert!(
        out.status.success(),
        "pactl {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Módulos carregados. Soltar descarrega, mesmo com o teste em pânico.
struct Rig(Vec<String>);

impl Rig {
    fn new() -> Self {
        let mut rig = Rig(vec![]);
        rig.load(
            "module-null-sink sink_name=bancada_fone channels=2 rate=48000 \
             sink_properties=device.description=BancadaFone",
        );
        rig.load(
            "module-null-sink sink_name=bancada_sala channels=1 channel_map=mono rate=48000 \
             sink_properties=device.description=BancadaSala",
        );
        rig.load("module-loopback source=bancada_fone.monitor sink=bancada_sala latency_msec=50");
        rig.load(
            "module-remap-source master=bancada_sala.monitor source_name=bancada_mic channels=1 \
             channel_map=mono source_properties=device.description=BancadaMic",
        );
        thread::sleep(Duration::from_secs(1));
        // Nome errado cai no padrão do sistema, que fora do `bancada.sh` é a caixa real.
        let found = devices();
        assert!(found.outputs.iter().any(|d| d == FONE), "{found:?}");
        assert!(found.inputs.iter().any(|d| d == MIC), "{found:?}");
        rig
    }

    fn load(&mut self, module: &str) -> String {
        let args: Vec<&str> = ["load-module"]
            .into_iter()
            .chain(module.split_whitespace())
            .collect();
        let id = pactl(&args);
        self.0.push(id.clone());
        id
    }

    fn unload(&mut self, id: &str) {
        pactl(&["unload-module", id]);
        self.0.retain(|m| m != id);
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        for id in self.0.iter().rev() {
            let _ = Command::new("pactl")
                .args(["unload-module", id])
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn prefs(noise: NoiseLevel) -> AudioPrefs {
    AudioPrefs {
        input: Some(MIC.into()),
        output: Some(FONE.into()),
        noise,
        sensitivity: Sensitivity::Auto,
        mic_gain: MicGain::Auto,
        master: 71,
        volumes: HashMap::new(),
    }
}

fn wav(name: &str) -> Vec<i16> {
    fs::read(format!("{WAV_DIR}/{name}")).expect("wav")[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect()
}

/// Empurra PCM em tempo real, como a sala faria, até ser solto.
struct Far(Arc<AtomicBool>);

impl Far {
    fn play(mut sink: VoiceSink, pcm: Vec<i16>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        thread::spawn(move || {
            let start = Instant::now();
            for (i, block) in pcm.as_chunks::<FRAME>().0.iter().cycle().enumerate() {
                if done.load(Ordering::Relaxed) {
                    return;
                }
                sink.push(block);
                let next = start + Duration::from_millis(10) * (i as u32 + 1);
                thread::sleep(next.saturating_duration_since(Instant::now()));
            }
        });
        Far(stop)
    }
}

impl Drop for Far {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct Energy {
    sum: f64,
    tone: f64,
    samples: u64,
}

impl Energy {
    fn add(&mut self, block: &[i16]) {
        self.sum += block.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        self.tone += goertzel(block, 1000.0);
        self.samples += block.len() as u64;
    }

    fn rms(&self) -> f64 {
        (self.sum / self.samples.max(1) as f64).sqrt() / 32768.0
    }
}

/// Energia de uma frequência num bloco: separa o tom "perto" do eco.
fn goertzel(x: &[i16], hz: f64) -> f64 {
    let w = std::f64::consts::TAU * hz / RATE as f64;
    let c = 2.0 * w.cos();
    let (mut a, mut b) = (0f64, 0f64);
    for s in x {
        let v = *s as f64 + c * a - b;
        b = a;
        a = v;
    }
    a * a + b * b - c * a * b
}

fn db(a: f64, b: f64) -> f64 {
    10.0 * (a.max(1e-9) / b.max(1e-9)).log10()
}

/// `parec` de uma fonte, em paralelo ao motor: enxerga o sinal antes do APM.
struct Capture {
    child: Child,
    energy: Arc<Mutex<Energy>>,
}

impl Capture {
    fn start(source: &str) -> Self {
        let mut child = Command::new("parec")
            .args(["-d", source, "--format=s16le", "--rate=48000"])
            .args(["--channels=1", "--raw", "--latency-msec=10"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("parec");
        let mut stdout = child.stdout.take().unwrap();
        let energy = Arc::new(Mutex::new(Energy::default()));
        let sum = energy.clone();
        thread::spawn(move || {
            let mut bytes = [0u8; FRAME * 2];
            while stdout.read_exact(&mut bytes).is_ok() {
                let block: Vec<i16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes([b[0], b[1]]))
                    .collect();
                sum.lock().unwrap().add(&block);
            }
        });
        Capture { child, energy }
    }

    fn reset(&self) {
        *self.energy.lock().unwrap() = Energy::default();
    }

    fn rms(&self) -> f64 {
        self.energy.lock().unwrap().rms()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) struct Killed(pub(crate) Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn play_near(file: &str) -> Killed {
    Killed(
        Command::new("paplay")
            .args(["--device=bancada_sala", &format!("{WAV_DIR}/{file}")])
            .spawn()
            .expect("paplay"),
    )
}

/// Conta os blocos do mic durante `time`, somando a energia de quem foi passado.
fn drain(mic: &mut MicFrames, time: Duration, mut energy: Option<&mut Energy>) -> (usize, usize) {
    let end = Instant::now() + time;
    let (mut blocks, mut silent) = (0, 0);
    while Instant::now() < end {
        match mic.try_recv() {
            Ok(block) => {
                assert_eq!(block.len(), FRAME);
                blocks += 1;
                silent += block.iter().all(|s| *s == 0) as usize;
                if let Some(e) = &mut energy {
                    e.add(&block);
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(1)),
        }
    }
    (blocks, silent)
}

/// Aquece 10 s pro AEC convergir e mede 10 s.
fn round(name: &str, noise: NoiseLevel, gate: bool, far: bool, near: Option<&str>) {
    mic::BYPASS_GATE.store(!gate, Ordering::Relaxed);
    let (engine, mut mic) = Engine::start(prefs(noise)).expect("start");
    let raw = Capture::start("bancada_mic");
    let _far = far.then(|| {
        let sink = engine.add_voice("far".into(), "ana".into());
        Far::play(sink, wav("fala.wav"))
    });
    let _near = near.map(play_near);
    let (muted_blocks, _) = drain(&mut mic, Duration::from_secs(1), None);
    assert_eq!(muted_blocks, 0, "mudo não entrega bloco");
    engine.set_mic_muted(false);
    drain(&mut mic, Duration::from_secs(9), None);

    raw.reset();
    let (mut out, mut worst) = (Energy::default(), 0f64);
    let (mut blocks, mut silent) = (0, 0);
    for _ in 0..10 {
        let mut second = Energy::default();
        let (b, s) = drain(&mut mic, Duration::from_secs(1), Some(&mut second));
        (blocks, silent) = (blocks + b, silent + s);
        worst = worst.max(second.rms());
        out.sum += second.sum;
        out.tone += second.tone;
        out.samples += second.samples;
    }
    let raw = raw.energy.lock().unwrap();
    println!(
        "BANCADA {name}: blocos/s={} blocos_zerados={:.0}% mic_rms={:.5} saida_rms={:.6} \
         pior_segundo={:.6} queda={:.1} dB tom_1k_queda={:.1} dB",
        blocks / 10,
        silent as f64 * 100.0 / blocks.max(1) as f64,
        raw.rms(),
        out.rms(),
        worst,
        db(
            raw.sum / raw.samples.max(1) as f64,
            out.sum / out.samples.max(1) as f64
        ),
        db(
            raw.tone / raw.samples.max(1) as f64,
            out.tone / out.samples.max(1) as f64
        ),
    );
    assert!(
        (95..=105).contains(&(blocks / 10)),
        "{blocks} blocos em 10 s"
    );
}

#[test]
#[ignore = "precisa de servidor de áudio: src/audio/bancada.sh"]
fn bancada_eco() {
    let _rig = Rig::new();
    use NoiseLevel::*;
    round("eco, só APM sem gate", Off, false, true, None);
    round("eco, APM+NS sem gate", Apm, false, true, None);
    round("eco, APM+RNNoise sem gate", Rnnoise, false, true, None);
    round("eco, Off com gate", Off, true, true, None);
    round("eco, Apm com gate", Apm, true, true, None);
    round(
        "eco, motor padrão (Rnnoise e gate)",
        Rnnoise,
        true,
        true,
        None,
    );
    round(
        "voz perto sem eco, motor padrão",
        Rnnoise,
        true,
        false,
        Some("fala.wav"),
    );
    round(
        "eco + tom perto, Off com gate",
        Off,
        true,
        true,
        Some("perto.wav"),
    );
}

/// O que chega numa fonte em pouco mais de 1 s.
pub(crate) fn level(source: &str) -> f64 {
    let capture = Capture::start(source);
    thread::sleep(Duration::from_millis(1200));
    capture.rms()
}

fn blocks_per_second(mic: &mut MicFrames) -> usize {
    drain(mic, Duration::from_millis(100), None);
    drain(mic, Duration::from_secs(1), None).0
}

/// Threads filhas herdam o nome, então zero quer dizer que os streams também foram.
fn audio_threads() -> usize {
    fs::read_dir("/proc/self/task")
        .unwrap()
        .flatten()
        .filter(|t| {
            fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.trim() == "concord-audio")
        })
        .count()
}

fn streams() -> String {
    format!(
        "saída [{}] entrada [{}]",
        pactl(&["list", "short", "sink-inputs"]).replace('\n', "; "),
        pactl(&["list", "short", "source-outputs"]).replace('\n', "; "),
    )
}

/// Só no `bancada.sh`: cair no padrão do sistema aqui toca de verdade.
#[test]
#[ignore = "precisa do PipeWire próprio: src/audio/bancada.sh"]
fn bancada_queda() {
    let raiz = std::env::var("BANCADA_RAIZ").expect("rode por src/audio/bancada.sh");
    let wait = Duration::from_secs(3);
    let mut rig = Rig::new();
    let mic_module = rig.0[3].clone();
    let dois = rig.load(
        "module-null-sink sink_name=bancada_dois channels=2 rate=48000 \
         sink_properties=device.description=BancadaDois",
    );
    rig.load(
        "module-null-sink sink_name=bancada_reserva channels=2 rate=48000 \
         sink_properties=device.description=BancadaReserva",
    );
    pactl(&["set-default-sink", "bancada_reserva"]);
    pactl(&["set-default-source", "bancada_reserva.monitor"]);
    let found = devices();
    println!("BANCADA dispositivos: {found:?}");
    assert!(!found.inputs.iter().any(|d| d.starts_with("Monitor")));

    let (engine, mut mic) = Engine::start(prefs(NoiseLevel::Off)).expect("start");
    let sink = engine.add_voice("far".into(), "ana".into());
    let _far = Far::play(sink, wav("fala.wav"));
    engine.set_mic_muted(false);
    let check = |source: &str, mic: &mut MicFrames, what: &str| {
        let (rms, blocks) = (level(source), blocks_per_second(mic));
        println!(
            "BANCADA {what}: {source} rms={rms:.4} blocos/s={blocks} | {}",
            streams()
        );
        assert!(rms > 0.05 && blocks >= 90, "{what}");
    };
    check("bancada_fone.monitor", &mut mic, "início");

    engine.set_deafened(true);
    thread::sleep(Duration::from_millis(200));
    let deaf = level("bancada_fone.monitor");
    println!("BANCADA surdo: rms={deaf}");
    assert_eq!(deaf, 0.0);
    engine.set_deafened(false);

    engine.set_noise(NoiseLevel::Apm);
    engine.set_output(Some("BancadaDois".into()));
    thread::sleep(Duration::from_millis(500));
    check("bancada_dois.monitor", &mut mic, "set_output");
    assert_eq!(level("bancada_fone.monitor"), 0.0, "saída antiga parou");

    engine.set_input(None);
    thread::sleep(Duration::from_millis(500));
    assert!(blocks_per_second(&mut mic) >= 90, "set_input");
    engine.set_input(Some(MIC.into()));
    thread::sleep(Duration::from_millis(500));

    rig.unload(&mic_module);
    println!("BANCADA mic sumiu: {}", streams());
    thread::sleep(wait);
    check("bancada_dois.monitor", &mut mic, "mic sumiu");

    rig.unload(&dois);
    println!("BANCADA saída sumiu: {}", streams());
    thread::sleep(wait);
    check("bancada_reserva.monitor", &mut mic, "saída sumiu");

    // Servidor cai: aqui é o erro do callback que tem que reabrir.
    let pid = fs::read_to_string(format!("{raiz}/pulse/pid")).unwrap();
    assert!(
        Command::new("kill")
            .arg(pid.trim())
            .status()
            .unwrap()
            .success()
    );
    rig.0.clear();
    thread::sleep(Duration::from_secs(1));
    rig.load(
        "module-null-sink sink_name=bancada_nova channels=2 rate=48000 \
         sink_properties=device.description=BancadaNova",
    );
    thread::sleep(wait);
    check("bancada_nova.monitor", &mut mic, "servidor voltou");

    drop(engine);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(audio_threads(), 0, "motor para com o último clone");

    let saved = AudioPrefs {
        input: Some("Sumiu".into()),
        output: Some("Sumiu".into()),
        ..prefs(NoiseLevel::Off)
    };
    let (engine, mut mic) = Engine::start(saved).expect("salvo sumiu cai no padrão");
    let sink = engine.add_voice("far".into(), "ana".into());
    let _far = Far::play(sink, wav("fala.wav"));
    engine.set_mic_muted(false);
    thread::sleep(Duration::from_millis(300));
    check("bancada_nova.monitor", &mut mic, "dispositivo salvo sumiu");
    let clone = engine.clone();
    drop(engine);
    thread::sleep(Duration::from_millis(100));
    assert!(audio_threads() > 0, "um clone mantém o motor");
    drop(clone);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(audio_threads(), 0);
    println!("BANCADA fim: {}", streams());
}

/// Som de saída tocando quando o motor cai, como ao sair da call: a call seguinte ouve, e
/// saída que morre no meio do som não prende a thread.
#[test]
#[ignore = "precisa do PipeWire próprio: src/audio/bancada.sh"]
fn bancada_som_de_saida() {
    let raiz = std::env::var("BANCADA_RAIZ").expect("rode por src/audio/bancada.sh");
    let mut rig = Rig::new();
    for volta in 0..5 {
        let (engine, _mic) = Engine::start(prefs(NoiseLevel::Off)).expect("start");
        let _far = Far::play(
            engine.add_voice("far".into(), "ana".into()),
            wav("fala.wav"),
        );
        let rms = level("bancada_fone.monitor");
        println!(
            "BANCADA volta {volta}: rms={rms:.4} threads={}",
            audio_threads()
        );
        assert!(rms > 0.05, "volta {volta} sem voz");
        engine.play(Effect::Leave);
        drop(engine);
    }
    thread::sleep(Duration::from_secs(3));
    assert_eq!(audio_threads(), 0, "o som acabou e as threads também");

    let (engine, _mic) = Engine::start(prefs(NoiseLevel::Off)).expect("start");
    thread::sleep(Duration::from_millis(300));
    engine.play(Effect::Leave);
    drop(engine);
    let pid = fs::read_to_string(format!("{raiz}/pulse/pid")).unwrap();
    assert!(
        Command::new("kill")
            .arg(pid.trim())
            .status()
            .unwrap()
            .success()
    );
    rig.0.clear();
    thread::sleep(LINGER_TEST);
    println!("BANCADA saída morreu no som: threads={}", audio_threads());
    assert_eq!(audio_threads(), 0, "saída morta segurou a thread");
}

/// Prazo do `LINGER` do motor com folga.
const LINGER_TEST: Duration = Duration::from_secs(4);

/// Fone Bluetooth que some e volta, e padrão do sistema que muda: a voz vai junto. Sem
/// isso, o stream ficava no dispositivo pra onde o servidor o empurrou, sem erro nenhum.
#[test]
#[ignore = "precisa do PipeWire próprio: src/audio/bancada.sh"]
fn bancada_bluetooth() {
    std::env::var("BANCADA_RAIZ").expect("rode por src/audio/bancada.sh");
    let wait = Duration::from_secs(3);
    let bt = "module-null-sink sink_name=bancada_bt channels=2 rate=48000 \
              sink_properties=device.description=BancadaBT";
    let mut rig = Rig::new();
    rig.load(
        "module-null-sink sink_name=bancada_reserva channels=2 rate=48000 \
         sink_properties=device.description=BancadaReserva",
    );
    pactl(&["set-default-sink", "bancada_reserva"]);
    let mut falhas = vec![];
    let mut ouve = |sink: &str, what: &str| {
        let rms = level(&format!("{sink}.monitor"));
        println!("BANCADA {what}: {sink} rms={rms:.4} | {}", streams());
        if rms < 0.05 {
            falhas.push(what.to_owned());
        }
    };

    let escolhido = AudioPrefs {
        output: Some("BancadaBT".into()),
        ..prefs(NoiseLevel::Off)
    };
    let (engine, _mic) = Engine::start(escolhido).expect("start");
    let _far = Far::play(
        engine.add_voice("far".into(), "ana".into()),
        wav("fala.wav"),
    );
    ouve("bancada_reserva", "fone desligado ao entrar");
    let modulo = rig.load(bt);
    thread::sleep(wait);
    ouve("bancada_bt", "fone ligou depois");
    rig.unload(&modulo);
    thread::sleep(wait);
    ouve("bancada_reserva", "fone caiu");
    rig.load(bt);
    thread::sleep(wait);
    ouve("bancada_bt", "fone voltou");
    drop((_far, engine));

    // Fone que é também o padrão do sistema, como um fone Bluetooth: cai e volta como padrão.
    pactl(&["set-default-sink", "bancada_bt"]);
    let escolhido = AudioPrefs {
        output: Some("BancadaBT".into()),
        ..prefs(NoiseLevel::Off)
    };
    let (engine, _mic) = Engine::start(escolhido).expect("start");
    let _far = Far::play(
        engine.add_voice("far".into(), "ana".into()),
        wav("fala.wav"),
    );
    ouve("bancada_bt", "fone padrão");
    let modulo = rig.0.last().unwrap().clone();
    rig.unload(&modulo);
    thread::sleep(wait);
    ouve("bancada_reserva", "fone padrão caiu");
    rig.load(bt);
    pactl(&["set-default-sink", "bancada_bt"]);
    thread::sleep(wait);
    ouve("bancada_bt", "fone padrão voltou");
    drop((_far, engine));
    pactl(&["set-default-sink", "bancada_reserva"]);

    let padrao = AudioPrefs {
        output: None,
        ..prefs(NoiseLevel::Off)
    };
    let (engine, _mic) = Engine::start(padrao).expect("start");
    let _far = Far::play(
        engine.add_voice("far".into(), "ana".into()),
        wav("fala.wav"),
    );
    ouve("bancada_reserva", "padrão");
    pactl(&["set-default-sink", "bancada_bt"]);
    thread::sleep(wait);
    ouve("bancada_bt", "padrão mudou");
    assert!(falhas.is_empty(), "sem voz em: {falhas:?}");
}

// --- Calibração do limitador -------------------------------------------------

/// Nível da fala normal, e o gabarito: bloco com fala é o que passa de 20 dB abaixo dela.
const FALA_DB: f32 = -24.0;
const COM_FALA_DB: f32 = FALA_DB - 20.0;
/// Pausa maior que isto começa outra frase.
const PAUSA_BLOCOS: usize = 25;
/// Ruído de fundo que toda sala tem, e o RNNoise trata silêncio digital à parte.
const PISO_DB: f32 = -78.0;
/// Depois do aquecimento do gate, pra medir a fala e não o aquecimento.
const SOBRA: f32 = 3.0;
const MANUAL: Sensitivity = Sensitivity::Manual(MANUAL_DEFAULT_DB);

fn amp(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn block_db(block: &[f32]) -> f32 {
    let sum: f32 = block.iter().map(|s| s * s).sum();
    10.0 * (sum / block.len() as f32).max(1e-12).log10()
}

/// Ruído previsível entre rodadas.
struct Dado(u32);

impl Dado {
    /// De -1 a 1.
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }

    /// Ruído branco com o RMS pedido.
    fn ruido(&mut self, secs: f32, db: f32) -> Vec<f32> {
        let rms = amp(db) * 3f32.sqrt();
        (0..(secs * RATE as f32) as usize)
            .map(|_| self.next() * rms)
            .collect()
    }

    /// Tecla descendo e subindo, seis por segundo em média. `decay_ms` longo é mecânico.
    fn cliques(&mut self, secs: f32, pico_db: f32, decay_ms: f32) -> Vec<f32> {
        let mut pcm = self.ruido(secs, -72.0);
        let (decay, ms) = (decay_ms * 48.0, RATE as f32 / 1000.0);
        let mut at = 300.0;
        while at < (secs - 0.5) * 1000.0 {
            for (extra, pico) in [(0.0, 1.0), (80.0 + self.next().abs() * 40.0, 0.5)] {
                let start = ((at + extra) * ms) as usize;
                for (i, s) in pcm[start..].iter_mut().take(decay as usize * 7).enumerate() {
                    let envelope = (-(i as f32) / decay).exp();
                    *s += self.next() * envelope * pico * amp(pico_db);
                }
            }
            at += 60.0 + self.next().abs() * 2000.0 / 6.0;
        }
        pcm
    }
}

/// As vozes em sequência, com 1 s entre elas, em [`FALA_DB`].
fn voz() -> Vec<f32> {
    voz_de(&VOZES)
}

fn voz_de(nomes: &[&str]) -> Vec<f32> {
    let mut pcm = vec![];
    for nome in nomes {
        let bytes = fs::read(format!("{VOZ_DIR}/{nome}.wav")).expect("wav de voz");
        let base: Vec<f32> = bytes[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .take(24_000 * 14)
            .collect();
        // Arquivos em 24 kHz.
        let dobro: Vec<f32> = base
            .windows(2)
            .flat_map(|w| [w[0], (w[0] + w[1]) / 2.0])
            .collect();
        let blocos: Vec<f32> = dobro
            .as_chunks::<FRAME>()
            .0
            .iter()
            .map(|b| block_db(b))
            .collect();
        let topo = blocos.iter().copied().fold(-200.0, f32::max);
        let ativos: Vec<f32> = blocos.into_iter().filter(|d| *d > topo - 30.0).collect();
        let medio = ativos.iter().map(|d| 10f32.powf(d / 10.0)).sum::<f32>() / ativos.len() as f32;
        let ganho = amp(FALA_DB - 10.0 * medio.log10());
        pcm.extend(vec![0.0; RATE as usize]);
        pcm.extend(dobro.iter().map(|s| s * ganho));
    }
    pcm.extend(vec![0.0; RATE as usize]);
    pcm
}

fn soma(a: &[f32], ganho_a: f32, b: &[f32]) -> Vec<f32> {
    a.iter().zip(b).map(|(a, b)| a * ganho_a + b).collect()
}

fn write_wav(path: &str, pcm: &[f32]) {
    let bytes = pcm.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + bytes as usize);
    wav.extend(b"RIFF");
    wav.extend((36 + bytes).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(RATE.to_le_bytes());
    wav.extend((RATE * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(bytes.to_le_bytes());
    for s in pcm {
        wav.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    fs::write(path, wav).expect("wav");
}

/// `gabarito` é a fala limpa, se tiver: diz onde cada frase começa e termina.
struct Sinal {
    nome: &'static str,
    pcm: Vec<f32>,
    gabarito: Option<Vec<f32>>,
}

/// Toca o sinal e sobe o motor com a sala já soando, como numa call de verdade.
fn escuta(sinal: &Sinal, prefs: AudioPrefs) -> Vec<mic::Traced> {
    let raiz = std::env::var("BANCADA_RAIZ").expect("rode por src/audio/bancada.sh");
    let path = format!("{raiz}/sinal.wav");
    write_wav(&path, &sinal.pcm);
    let _near = Killed(
        Command::new("paplay")
            .args(["--device=bancada_sala", &path])
            .spawn()
            .expect("paplay"),
    );
    let inicio = Instant::now();
    thread::sleep(Duration::from_millis(500));
    mic::TRACE.lock().unwrap().clear();
    let (engine, mut mic) = Engine::start(prefs).expect("start");
    engine.set_mic_muted(false);
    // Para antes do fim: o som cortando seco engana o RNNoise e não existe na sala.
    let fim = Duration::from_secs_f32(sinal.pcm.len() as f32 / RATE as f32 - 0.5);
    drain(&mut mic, fim.saturating_sub(inicio.elapsed()), None);
    drop(engine);
    std::mem::take(&mut *mic::TRACE.lock().unwrap())
}

/// Envelope em janelas de 5 ms.
fn envelope(pcm: impl Iterator<Item = f32>) -> Vec<f32> {
    pcm.map(f32::abs)
        .collect::<Vec<_>>()
        .as_chunks::<240>()
        .0
        .iter()
        .map(|c| c.iter().sum())
        .collect()
}

/// Em quantas amostras o sinal que entrou no gate está depois do `gabarito`. Compara os
/// envelopes, que resistem ao AGC. O motor sobe 500 ms depois do som: dá negativo.
fn atraso(gabarito: &[f32], trace: &[mic::Traced]) -> isize {
    let a = envelope(gabarito.iter().copied());
    let b = envelope(
        trace
            .iter()
            .flat_map(|t| t.entered)
            .map(|s| s as f32 / 32768.0),
    );
    let casa = |lag: isize| -> f32 {
        a.iter()
            .enumerate()
            .filter_map(|(i, x)| Some(x * b.get(usize::try_from(i as isize + lag).ok()?)?))
            .sum()
    };
    (-200..100)
        .max_by(|x, y| casa(*x).total_cmp(&casa(*y)))
        .unwrap()
        * 240
}

#[derive(Default)]
struct Cortes {
    passou: f32,
    inicio: Vec<usize>,
    fim: Vec<usize>,
}

/// Blocos do gabarito com fala saindo com ganho de pelo menos metade, e quanto cada frase
/// perdeu nas pontas, em ms.
fn cortes(gabarito: &[f32], trace: &[mic::Traced], atraso: isize) -> Cortes {
    let fala: Vec<bool> = gabarito
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(|b| block_db(b) >= COM_FALA_DB)
        .collect();
    let desvio = (atraso + super::gate::LOOKAHEAD as isize) / FRAME as isize;
    let saiu = |i: usize| {
        usize::try_from(i as isize + desvio)
            .ok()
            .and_then(|k| trace.get(k))
            .is_some_and(|t| t.gain >= 0.5)
    };
    let com_fala: Vec<usize> = (0..fala.len()).filter(|i| fala[*i]).collect();
    let passou = com_fala.iter().filter(|i| saiu(**i)).count();
    let mut frases: Vec<(usize, usize)> = vec![];
    for &i in &com_fala {
        match frases.last_mut() {
            Some((_, fim)) if i - *fim <= PAUSA_BLOCOS => *fim = i,
            _ => frases.push((i, i)),
        }
    }
    let mut medido = Cortes {
        passou: passou as f32 * 100.0 / com_fala.len().max(1) as f32,
        ..Default::default()
    };
    for (a, b) in frases.into_iter().filter(|(a, b)| b - a >= 3) {
        let dentro: Vec<usize> = (a..=b).filter(|i| saiu(*i)).collect();
        let (primeiro, ultimo) = match (dentro.first(), dentro.last()) {
            (Some(p), Some(u)) => (*p, *u),
            _ => (b + 1, b + 1),
        };
        medido.inicio.push((primeiro - a) * 10);
        medido.fim.push((b + 1).saturating_sub(ultimo + 1) * 10);
    }
    medido
}

fn resumo(ms: &mut [usize]) -> String {
    if ms.is_empty() {
        return "-".into();
    }
    ms.sort_unstable();
    let media = ms.iter().sum::<usize>() / ms.len();
    let p90 = ms[ms.len() * 9 / 10];
    format!("{media}/{p90}/{}", ms[ms.len() - 1])
}

/// Quanto o gate atrasa na cadeia real: acha na entrada o bloco que saiu com ganho cheio.
fn atraso_gate_ms(trace: &[mic::Traced]) -> Option<f32> {
    let entrou: Vec<i16> = trace.iter().flat_map(|t| t.entered).collect();
    let k = trace
        .windows(2)
        .position(|w| w[0].gain == 1.0 && w[1].gain == 1.0 && block_db_i16(&w[1].left) > -40.0)?
        + 1;
    let at = entrou.windows(FRAME).position(|w| w == trace[k].left)?;
    Some((k * FRAME - at) as f32 / 48.0)
}

fn block_db_i16(block: &[i16]) -> f32 {
    let pcm: Vec<f32> = block.iter().map(|s| *s as f32 / 32768.0).collect();
    block_db(&pcm)
}

/// Uma linha da tabela. Devolve quantas vezes abriu e a porcentagem de fala que passou.
fn mede(sinal: &Sinal, noise: NoiseLevel, sensitivity: Sensitivity) -> (usize, f32) {
    let trace = escuta(
        sinal,
        AudioPrefs {
            sensitivity,
            ..prefs(noise)
        },
    );
    let minutos = trace.len() as f32 / 6000.0;
    let aberturas = trace
        .windows(2)
        .filter(|w| !w[0].passing && w[1].passing)
        .count();
    let aberto = trace.iter().filter(|t| t.passing).count() as f32 * 100.0 / trace.len() as f32;
    let mut medido = Cortes::default();
    if let Some(gabarito) = &sinal.gabarito {
        medido = cortes(gabarito, &trace, atraso(gabarito, &trace));
    }
    println!(
        "GATE | {:<26} | {noise:?} {sensitivity:?} | fala {:5.1}% | início {} | fim {} | \
         aberturas {aberturas} ({:.1}/min) | aberto {aberto:.0}% | atraso {:?} ms",
        sinal.nome,
        medido.passou,
        resumo(&mut medido.inicio),
        resumo(&mut medido.fim),
        aberturas as f32 / minutos,
        atraso_gate_ms(&trace),
    );
    (aberturas, medido.passou)
}

#[test]
#[ignore = "precisa do PipeWire próprio: src/audio/bancada.sh"]
fn bancada_gate() {
    let _rig = Rig::new();
    let mut dado = Dado(0x1234_5678);
    let silencio = vec![0.0; RATE as usize * SOBRA as usize];
    let fala: Vec<f32> = silencio.iter().copied().chain(voz()).collect();
    let segundos = fala.len() as f32 / RATE as f32;
    let piso = dado.ruido(segundos, PISO_DB);

    let wav_com_sobra = |nome: &str| -> Vec<f32> {
        silencio
            .iter()
            .copied()
            .chain(wav(nome).iter().map(|s| *s as f32 / 32768.0))
            .collect()
    };
    let (sintetica, tom) = (wav_com_sobra("fala.wav"), wav_com_sobra("perto.wav"));
    // Perto até a metade, depois a mesma voz 30 dB mais longe.
    let meio = fala.len() / 2;
    let distante: Vec<f32> = fala[RATE as usize..]
        .iter()
        .enumerate()
        .map(|(i, s)| if i < meio { *s } else { *s * amp(-30.0) })
        .collect();
    let liga: Vec<f32> = dado
        .ruido(8.0, PISO_DB)
        .into_iter()
        .chain(dado.ruido(20.0, -40.0))
        .collect();

    let vozes = [
        ("voz normal (-24)", 0.0, PISO_DB),
        ("voz baixa + ruído (-36/-52)", -12.0, -52.0),
        ("voz + ruído forte (-30/-40)", -6.0, -40.0),
    ];
    let mut sinais: Vec<Sinal> = vozes
        .into_iter()
        .map(|(nome, ganho, ruido_db)| Sinal {
            nome,
            pcm: soma(&fala, amp(ganho), &dado.ruido(segundos, ruido_db)),
            gabarito: Some(fala.clone()),
        })
        .collect();
    sinais.extend([
        Sinal {
            nome: "voz perto, depois longe",
            pcm: soma(&distante, 1.0, &piso),
            gabarito: None,
        },
        Sinal {
            nome: "fala.wav (sintética)",
            pcm: sintetica.clone(),
            gabarito: Some(sintetica),
        },
        Sinal {
            nome: "perto.wav (tom)",
            pcm: tom.clone(),
            gabarito: Some(tom),
        },
    ]);
    for (nome, pcm) in [
        ("ruído branco -60", dado.ruido(40.0, -60.0)),
        ("ruído branco -50", dado.ruido(40.0, -50.0)),
        ("ruído branco -40", dado.ruido(40.0, -40.0)),
        ("ruído liga do nada (-40)", liga),
        ("clique -30", dado.cliques(40.0, -30.0, 2.0)),
        ("clique -20", dado.cliques(40.0, -20.0, 2.0)),
        ("clique -10", dado.cliques(40.0, -10.0, 2.0)),
        ("clique mecânico -12", dado.cliques(40.0, -12.0, 6.0)),
    ] {
        sinais.push(Sinal {
            nome,
            pcm,
            gabarito: None,
        });
    }

    // Rodada completa leva 35 min: `BANCADA_SO=clique` mede só o que tem esse nome.
    let so = std::env::var("BANCADA_SO").unwrap_or_default();
    sinais.retain(|s| s.nome.contains(&so));
    let mut falhou = vec![];
    for sinal in &sinais {
        let (aberturas, passou) = mede(sinal, NoiseLevel::Rnnoise, Sensitivity::Auto);
        mede(sinal, NoiseLevel::Rnnoise, MANUAL);
        let so_ruido = sinal.nome.contains("ruído branco") || sinal.nome.contains("clique");
        if so_ruido && aberturas > 0 {
            falhou.push(sinal.nome);
        }
        if sinal.nome.starts_with("voz normal") && passou < 95.0 {
            falhou.push(sinal.nome);
        }
    }
    // Os outros níveis mudam o que o gate vê: sem RNNoise o ruído chega inteiro nele.
    for noise in [NoiseLevel::Off, NoiseLevel::Apm] {
        for sinal in sinais.iter().filter(|s| {
            s.nome.starts_with("voz normal")
                || s.nome == "ruído branco -50"
                || s.nome == "clique -20"
        }) {
            mede(sinal, noise, Sensitivity::Auto);
            mede(sinal, noise, MANUAL);
        }
    }
    assert!(falhou.is_empty(), "{falhou:?}");
}

/// Eco de voz de verdade: quando o AEC ainda não convergiu, é voz chegando no mic.
#[test]
#[ignore = "precisa do PipeWire próprio: src/audio/bancada.sh"]
fn bancada_gate_eco() {
    let _rig = Rig::new();
    let pcm: Vec<i16> = voz().iter().map(|s| (s * 32767.0) as i16).collect();
    for sensitivity in [Sensitivity::Auto, MANUAL] {
        mic::TRACE.lock().unwrap().clear();
        let (engine, mut mic) = Engine::start(AudioPrefs {
            sensitivity,
            ..prefs(NoiseLevel::Rnnoise)
        })
        .expect("start");
        engine.set_mic_muted(false);
        let raw = Capture::start("bancada_mic");
        let sink = engine.add_voice("far".into(), "ana".into());
        let _far = Far::play(sink, pcm.clone());
        drain(&mut mic, Duration::from_secs(60), None);
        drop(engine);
        let trace = std::mem::take(&mut *mic::TRACE.lock().unwrap());
        let abriu: Vec<usize> = trace
            .windows(2)
            .enumerate()
            .filter(|(_, w)| !w[0].passing && w[1].passing)
            .map(|(i, _)| i / 100)
            .collect();
        let aberto = trace.iter().filter(|t| t.passing).count() as f32 * 100.0 / trace.len() as f32;
        println!(
            "GATE | eco de voz | {sensitivity:?} | mic_rms={:.4} | abriu nos segundos {abriu:?} | \
             aberto {aberto:.1}%",
            raw.rms(),
        );
    }
}

// --- Ganho de entrada --------------------------------------------------------

/// Nível médio dos blocos com fala no gabarito, como entraram no gate, em dBFS.
fn nivel_fala(gabarito: &[f32], trace: &[mic::Traced], atraso: isize) -> f32 {
    let desvio = atraso / FRAME as isize;
    let potencias: Vec<f32> = gabarito
        .as_chunks::<FRAME>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, b)| block_db(*b) >= COM_FALA_DB)
        .filter_map(|(i, _)| trace.get(usize::try_from(i as isize + desvio).ok()?))
        .map(|t| 10f32.powf(block_db_i16(&t.entered) / 10.0))
        .collect();
    10.0 * (potencias.iter().sum::<f32>() / potencias.len().max(1) as f32).log10()
}

/// A fala direto na cadeia, sem servidor de áudio. Sai igual bit a bit entre rodadas: é
/// assim que o automático se compara antes e depois de mexer na cadeia.
fn sem_servidor(pcm: &[f32], prefs: &AudioPrefs) -> (f32, u64) {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut chain =
        mic::MicChain::new(prefs.noise, prefs.sensitivity, prefs.mic_gain, Meter::new());
    chain.muted = false;
    let (mut soma, mut n, mut hash) = (0f64, 0usize, DefaultHasher::new());
    for bloco in pcm.as_chunks::<FRAME>().0 {
        chain.reverse(&mut [0; FRAME]);
        let mut b = bloco.map(|s| (s * 32767.0) as i16);
        let saiu = chain.process(&mut b).expect("sem mudo");
        saiu.hash(&mut hash);
        soma += saiu
            .iter()
            .map(|s| (*s as f64 / 32768.0).powi(2))
            .sum::<f64>();
        n += FRAME;
    }
    mic::TRACE.lock().unwrap().clear();
    ((10.0 * (soma / n as f64).log10()) as f32, hash.finish())
}

/// Ganho de entrada: a razão entre os ganhos manuais, o automático igual ao de antes e o
/// limitador abrindo com voz e fechado no ruído nos extremos.
#[test]
#[ignore = "precisa do PipeWire próprio: src/audio/bancada.sh"]
fn bancada_ganho() {
    let _rig = Rig::new();
    let mut dado = Dado(0x1234_5678);
    let silencio = vec![0.0; RATE as usize * SOBRA as usize];
    let limpa: Vec<f32> = silencio
        .iter()
        .copied()
        .chain(voz_de(&["Portuguese_stable"]))
        .collect();
    let segundos = limpa.len() as f32 / RATE as f32;
    // A fala tem pico 8 dB acima do nível: a -10 ainda não corta na entrada.
    let mut falando = |nome, db: f32| Sinal {
        nome,
        pcm: soma(&limpa, amp(db - FALA_DB), &dado.ruido(segundos, PISO_DB)),
        gabarito: Some(limpa.clone()),
    };
    let fala = falando("fala -24", FALA_DB);
    let alta = falando("fala alta -10", -10.0);
    let baixa = falando("fala baixa -44", -44.0);
    let ruido = Sinal {
        nome: "ruído branco -60",
        pcm: dado.ruido(20.0, -60.0),
        gabarito: None,
    };
    let com = |gain, sensitivity| AudioPrefs {
        sensitivity,
        mic_gain: gain,
        ..prefs(NoiseLevel::Rnnoise)
    };
    let (auto, man) = (Sensitivity::Auto, MANUAL);
    let m = MicGain::Manual;
    let rodadas: Vec<(&Sinal, AudioPrefs)> = vec![
        (&fala, com(MicGain::Auto, auto)),
        (&fala, com(MicGain::Auto, man)),
        (&ruido, com(MicGain::Auto, auto)),
        (&ruido, com(MicGain::Auto, man)),
        (&fala, com(m(0.0), auto)),
        (&fala, com(m(6.0), auto)),
        (&fala, com(m(-6.0), auto)),
        (&baixa, com(m(20.0), auto)),
        (&baixa, com(m(20.0), man)),
        (&fala, com(m(20.0), auto)),
        (&fala, com(m(20.0), man)),
        (&ruido, com(m(20.0), auto)),
        (&ruido, com(m(20.0), man)),
        (&alta, com(m(-20.0), auto)),
        (&alta, com(m(-20.0), man)),
        (&fala, com(m(-20.0), auto)),
        (&fala, com(m(-20.0), man)),
        (&ruido, com(m(-20.0), auto)),
        (&ruido, com(m(-20.0), man)),
    ];
    // Rodada completa leva 5 min: `BANCADA_SO=topologia` fica só no começo.
    let so = std::env::var("BANCADA_SO").unwrap_or_default();
    let rodadas: Vec<_> = rodadas
        .into_iter()
        .filter(|(s, _)| s.nome.contains(&so))
        .collect();

    // O motor grava do mic falso, e a fala toca na sala que vira esse mic.
    let _toca = play_near("fala.wav");
    let (engine, mut mic) = Engine::start(com(m(6.0), auto)).expect("start");
    engine.set_mic_muted(false);
    let mut saiu = Energy::default();
    drain(&mut mic, Duration::from_secs(2), Some(&mut saiu));
    let links = Command::new("pw-link").arg("-l").output().expect("pw-link");
    println!(
        "GANHO | topologia | {} | saída rms {:.4} | {}\n{}",
        streams(),
        saiu.rms(),
        level("bancada_mic"),
        String::from_utf8_lossy(&links.stdout)
    );
    drop((engine, _toca));

    for p in [com(MicGain::Auto, auto), com(MicGain::Auto, man)] {
        let (nivel, hash) = sem_servidor(&fala.pcm, &p);
        println!(
            "GANHO | sem servidor | {:?} {:?} | saída {nivel:6.2} dBFS | hash {hash:016x}",
            p.mic_gain, p.sensitivity
        );
    }
    for (sinal, p) in rodadas {
        // O que cortaria seco sem o limite suave, direto do sinal que entra.
        let ganho = match p.mic_gain {
            MicGain::Manual(db) => amp(db),
            MicGain::Auto => 1.0,
        };
        let seco = sinal
            .pcm
            .iter()
            .filter(|s| (*s * ganho).abs() >= 1.0)
            .count();
        let nome = format!("{:?} {:?}", p.mic_gain, p.sensitivity);
        let trace = escuta(sinal, p);
        let aberturas = trace
            .windows(2)
            .filter(|w| !w[0].passing && w[1].passing)
            .count();
        let saiu: Vec<i16> = trace.iter().flat_map(|t| t.left).collect();
        let pico = saiu.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        let cortadas = saiu.iter().filter(|s| s.unsigned_abs() >= 32767).count();
        let (nivel, passou) = match &sinal.gabarito {
            Some(g) => {
                let a = atraso(g, &trace);
                (nivel_fala(g, &trace, a), cortes(g, &trace, a).passou)
            }
            None => (block_db_i16(&saiu), 0.0),
        };
        println!(
            "GANHO | {:<16} | {nome:<28} | nível {nivel:7.2} dBFS | fala {passou:5.1}% | \
             aberturas {aberturas} | pico {pico} | cortadas {cortadas} (sem limite {seco})",
            sinal.nome,
        );
    }
}
