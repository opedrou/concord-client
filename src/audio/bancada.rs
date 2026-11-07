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
        master: 71,
        volumes: HashMap::new(),
    }
}

fn wav(name: &str) -> Vec<i16> {
    fs::read(format!("{WAV_DIR}/{name}")).expect("wav")[44..]
        .chunks_exact(2)
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
            for (i, block) in pcm.chunks_exact(FRAME).cycle().enumerate() {
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
                    .chunks_exact(2)
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

struct Killed(Child);

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
fn level(source: &str) -> f64 {
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
