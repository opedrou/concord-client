//! Motor de áudio: mic -> APM -> RNNoise -> gate -> sala, e sala -> mixer -> saída,
//! com o som final como referência do eco. Fixo em 48 kHz e blocos de 10 ms.
#[cfg(test)]
pub(crate) mod bancada;
mod device;
mod gate;
mod mic;
mod mixer;
mod worker;

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use mixer::{Mixer, VoiceQueue};
use worker::Command;

pub const RATE: u32 = 48_000;
pub const FRAME: usize = 480;
/// 100 ms de folga pra sala antes de perder bloco.
const MIC_QUEUE: usize = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoiseLevel {
    /// Sem redução de ruído.
    Off,
    /// Só o NS do APM. Equivale a "Navegador" na web.
    Apm,
    /// RNNoise depois do APM.
    #[default]
    Rnnoise,
}

/// Quando o mic abre, como a sensibilidade de entrada do Discord.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Sensitivity {
    /// Detecta voz: ruído e clique de teclado não abrem.
    #[default]
    Auto,
    /// Abre com o nível acima deste valor, em dBFS.
    Manual(f32),
}

/// Faixa do slider de sensibilidade manual, em dBFS.
pub const MANUAL_MIN_DB: f32 = -80.0;
pub const MANUAL_MAX_DB: f32 = 0.0;
pub const MANUAL_DEFAULT_DB: f32 = -40.0;

/// Ganho do mic, no lugar em que o AGC age: depois do APM, antes do RNNoise e do gate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum MicGain {
    /// O AGC do APM nivela a voz sozinho.
    #[default]
    Auto,
    /// AGC desligado e este ganho fixo, em dB.
    Manual(f32),
}

/// Faixa do ganho manual, em dB. O AGC começa em +15 dB e mic a 25% sem ele sai baixo
/// demais: o +20 cobre isso. O -20 desfaz o reforço de mic que o sistema aplica.
pub const GAIN_MIN_DB: f32 = -20.0;
pub const GAIN_MAX_DB: f32 = 20.0;

/// Quanto o medidor cai por bloco de 10 ms: do topo ao fundo em 600 ms.
const METER_DECAY_DB: f32 = (MANUAL_MAX_DB - MANUAL_MIN_DB) / 60.0;

/// Leitura do mic pra interface e pra sala, sem travar a thread de áudio.
#[derive(Clone)]
pub struct Meter {
    /// Bits do `f32`: não existe `AtomicF32`.
    level: Arc<AtomicU32>,
    open: Arc<AtomicBool>,
}

impl Meter {
    pub(crate) fn new() -> Self {
        Meter {
            level: Arc::new(AtomicU32::new(MANUAL_MIN_DB.to_bits())),
            open: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Nível atual em dBFS, já com o decaimento do medidor. Nunca abaixo de
    /// [`MANUAL_MIN_DB`].
    pub fn level_db(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// O limitador está deixando o mic passar.
    pub fn open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }

    /// Só a thread de áudio escreve, então ler e gravar em duas etapas não perde nada.
    pub(crate) fn update(&self, db: f32, open: bool) {
        let shown = db.max(self.level_db() - METER_DECAY_DB);
        self.level.store(shown.to_bits(), Ordering::Relaxed);
        self.open.store(open, Ordering::Relaxed);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioPrefs {
    /// Nome do dispositivo; `None` usa o padrão do sistema.
    pub input: Option<String>,
    pub output: Option<String>,
    pub noise: NoiseLevel,
    pub sensitivity: Sensitivity,
    pub mic_gain: MicGain,
    /// Slider de 0 a 100; ver [`gain`].
    pub master: u8,
    /// Nome da pessoa -> slider.
    pub volumes: HashMap<String, u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Devices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// Blocos de [`FRAME`] amostras mono já processados, prontos pra sala.
pub type MicFrames = tokio::sync::mpsc::Receiver<Vec<i16>>;

/// Curva da web: `2·x²` com `x` de 0 a 1. 100% de volume fica na posição 71.
pub fn gain(slider: u8) -> f32 {
    let x = slider.min(100) as f32 / 100.0;
    2.0 * x * x
}

/// Lista os dispositivos. Bloqueia: chamar fora da thread da interface.
pub fn devices() -> Devices {
    device::list()
}

/// Sons de interface, só pra quem ouve: não entram na sala. Os mesmos arquivos do site.
///
/// O PCM é mono, 48 kHz, `i16` little-endian, convertido uma vez dos mp3 do site:
/// `ffmpeg -i X.mp3 -f s16le -ar 48000 -ac 1 assets/sounds/X.pcm`
///
/// O `notify.pcm` veio de um wav estéreo com os canais em oposição de fase: a média dos dois
/// perdia 5 dB e mudava o som. Sai só o esquerdo, com +8 dB pro nível dos outros (pico
/// -3,5 dBFS, RMS -19 dBFS):
/// `ffmpeg -i X.wav -af "pan=mono|c0=FL,volume=8dB" -f s16le -ar 48000 assets/sounds/notify.pcm`
///
/// O `hello.pcm` veio de um wav estéreo de 96 kHz, com os canais quase iguais: a média só perde
/// 1 dB. Sobe 8 dB pro pico dos outros (pico -2,8 dBFS, RMS -24 dBFS, 2,6 s):
/// `ffmpeg -i X.wav -af "volume=8dB" -f s16le -ar 48000 -ac 1 assets/sounds/hello.pcm`
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Effect {
    Join,
    Leave,
    StreamStart,
    StreamStop,
    /// Chamado pra call ou marcado com `@`.
    Notify,
    /// Primeira abertura da janela no dia.
    Hello,
}

impl Effect {
    fn pcm(self) -> &'static [u8] {
        match self {
            Effect::Join => include_bytes!("../../assets/sounds/join.pcm"),
            Effect::Leave => include_bytes!("../../assets/sounds/leave.pcm"),
            Effect::StreamStart => include_bytes!("../../assets/sounds/stream-start.pcm"),
            Effect::StreamStop => include_bytes!("../../assets/sounds/stream-stop.pcm"),
            Effect::Notify => include_bytes!("../../assets/sounds/notify.pcm"),
            Effect::Hello => include_bytes!("../../assets/sounds/hello.pcm"),
        }
    }
}

/// Folga do [`play_once`] além da duração do som: abrir a saída e esvaziar o anel.
const ONCE_SLACK: Duration = Duration::from_secs(2);

/// Toca um som sem o motor da call e sem abrir o mic: abre só a saída, toca e fecha. Não
/// bloqueia. Sem o dispositivo salvo, toca no padrão do sistema. Saída travada desiste
/// depois da duração do som mais [`ONCE_SLACK`]. Na call, o som vai pelo [`Engine::play`].
pub fn play_once(effect: Effect, output: Option<&str>) {
    let output = output.map(String::from);
    let spawned = thread::Builder::new()
        .name("concord-som".into())
        .spawn(move || once(effect.pcm(), output.as_deref()));
    if let Err(e) = spawned {
        log!("áudio: som avulso sem thread: {e}");
    }
}

fn once(pcm: &'static [u8], output: Option<&str>) {
    // O `open_output` já cai no padrão quando o salvo sumiu; isto cobre o salvo que existe
    // e não abre.
    let opened = device::open_output(output).or_else(|e| match output {
        Some(_) => device::open_output(None),
        None => Err(e),
    });
    let mut out = match opened {
        Ok(out) => out,
        Err(e) => {
            log!("áudio: som avulso sem saída: {e}");
            return;
        }
    };
    let length = Duration::from_secs_f64(pcm.len() as f64 / 2.0 / RATE as f64);
    let deadline = Instant::now() + length + ONCE_SLACK;
    let mut mixer = Mixer::new(0, HashMap::new());
    mixer.play(pcm);
    let mut mix = [0f32; FRAME];
    let capacity = out.ring.buffer().capacity();
    // Toca e espera o anel esvaziar: soltar o stream antes corta o fim.
    while (mixer.playing() || out.ring.slots() < capacity)
        && Instant::now() < deadline
        && !out.is_lost()
    {
        while mixer.playing() && out.ring.slots() >= FRAME {
            mixer.mix(&mut mix);
            for s in mix {
                let _ = out.ring.push(s);
            }
        }
        thread::sleep(Duration::from_millis(5));
    }
    // O que já saiu do anel ainda passa pelo buffer do servidor de áudio.
    thread::sleep(Duration::from_millis(100));
}

/// Dono da thread de áudio. Clone barato; o motor para quando o último clone cai.
#[derive(Clone)]
pub struct Engine {
    commands: mpsc::Sender<Command>,
    meter: Meter,
}

impl Engine {
    /// Abre mic e saída e devolve os blocos do mic. Mudo enquanto ninguém chamar
    /// [`Engine::set_mic_muted`]`(false)`: entrar na call nunca abre o mic sozinho.
    /// Bloqueia enquanto abre os dispositivos. Só dá erro se nenhum dos dois abrir; com
    /// um lado só, o motor sobe e segue tentando o outro.
    pub fn start(prefs: AudioPrefs) -> Result<(Engine, MicFrames), String> {
        let (commands, received) = mpsc::channel();
        let (frames, mic) = tokio::sync::mpsc::channel(MIC_QUEUE);
        let (ready, started) = mpsc::channel();
        let meter = Meter::new();
        let reader = meter.clone();
        thread::Builder::new()
            .name("concord-audio".into())
            .spawn(move || worker::run(prefs, meter, received, frames, ready))
            .map_err(|e| e.to_string())?;
        started
            .recv()
            .map_err(|_| "thread de áudio caiu".to_string())??;
        Ok((
            Engine {
                commands,
                meter: reader,
            },
            mic,
        ))
    }

    fn send(&self, command: Command) {
        // Só falha com a thread morta, e aí não há o que controlar.
        let _ = self.commands.send(command);
    }

    /// Mudo não entrega bloco nenhum em [`MicFrames`].
    pub fn set_mic_muted(&self, muted: bool) {
        self.send(Command::Mute(muted));
    }

    /// Surdo zera toda a saída.
    pub fn set_deafened(&self, deafened: bool) {
        self.send(Command::Deafen(deafened));
    }

    pub fn set_master(&self, slider: u8) {
        self.send(Command::Master(slider));
    }

    /// Vale para todas as faixas de voz dessa pessoa, atuais e futuras.
    pub fn set_person_volume(&self, name: &str, slider: u8) {
        self.send(Command::Person(name.to_string(), slider));
    }

    /// Vale na hora, sem recriar o APM: a adaptação do eco continua.
    pub fn set_sensitivity(&self, sensitivity: Sensitivity) {
        self.send(Command::Sensitivity(sensitivity));
    }

    /// Mede também com o mic mudo.
    /// Mexer no ganho manual vale na hora. Trocar entre automático e manual recria o APM,
    /// como a redução de ruído do navegador.
    pub fn set_mic_gain(&self, gain: MicGain) {
        self.send(Command::Gain(gain));
    }

    pub fn meter(&self) -> Meter {
        self.meter.clone()
    }

    pub fn set_noise(&self, level: NoiseLevel) {
        self.send(Command::Noise(level));
    }

    pub fn set_input(&self, name: Option<String>) {
        self.send(Command::Input(name));
    }

    pub fn set_output(&self, name: Option<String>) {
        self.send(Command::Output(name));
    }

    /// Toca um som de interface. Surdo não ouve: some, como no site. Se o motor cair
    /// logo depois (sair da call), a thread espera o som acabar.
    pub fn play(&self, effect: Effect) {
        self.send(Command::Effect(effect.pcm()));
    }

    /// Fila própria por faixa, identificada por `key`. Soltar o [`VoiceSink`] tira a
    /// faixa do mixer.
    pub fn add_voice(&self, key: String, name: String) -> VoiceSink {
        let queue = Arc::new(VoiceQueue::default());
        self.send(Command::Voice {
            key,
            name,
            queue: queue.clone(),
        });
        VoiceSink { queue }
    }
}

/// Entrada de PCM mono em 48 kHz de uma faixa remota.
pub struct VoiceSink {
    queue: Arc<VoiceQueue>,
}

impl VoiceSink {
    /// Fila cheia descarta o mais antigo, pra latência não crescer.
    pub fn push(&mut self, pcm: &[i16]) {
        self.queue.push(pcm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curva_de_ganho() {
        let close = |a: f32, b: f32| (a - b).abs() < 0.0001;
        assert_eq!(gain(0), 0.0);
        assert!(close(gain(50), 0.5));
        assert!(close(gain(71), 1.0082));
        assert!(close(gain(100), 2.0));
        assert_eq!(gain(255), 2.0);
    }

    #[test]
    fn efeitos_embutidos_tem_duracao_e_nivel_plausiveis() {
        for effect in [
            Effect::Join,
            Effect::Leave,
            Effect::StreamStart,
            Effect::StreamStop,
            Effect::Notify,
            Effect::Hello,
        ] {
            let pcm = effect.pcm();
            assert_eq!(pcm.len() % 2, 0, "{effect:?}: i16 inteiro");
            let samples: Vec<f32> = pcm
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| i16::from_le_bytes(b) as f32 / 32768.0)
                .collect();
            let secs = samples.len() as f32 / RATE as f32;
            assert!((0.3..3.0).contains(&secs), "{effect:?}: {secs} s");
            let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
            let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
            assert!((0.1..=1.0).contains(&peak), "{effect:?}: pico {peak}");
            assert!((0.01..0.5).contains(&rms), "{effect:?}: rms {rms}");
        }
    }

    #[test]
    fn handles_cruzam_threads() {
        fn cross<T: Send + Sync>() {}
        cross::<Engine>();
        cross::<Meter>();
        cross::<VoiceSink>();
    }

    #[test]
    fn medidor_sobe_na_hora_e_cai_suave() {
        let meter = Meter::new();
        let reader = meter.clone();
        assert_eq!(reader.level_db(), MANUAL_MIN_DB);
        assert!(!reader.open());

        meter.update(-20.0, true);
        assert_eq!(reader.level_db(), -20.0, "sobe no primeiro bloco");
        assert!(reader.open());

        meter.update(-70.0, false);
        assert_eq!(reader.level_db(), -20.0 - METER_DECAY_DB, "cai um degrau");
        assert!(!reader.open());

        meter.update(-10.0, false);
        assert_eq!(reader.level_db(), -10.0);

        for _ in 0..60 {
            meter.update(MANUAL_MIN_DB, false);
        }
        assert_eq!(reader.level_db(), MANUAL_MIN_DB, "600 ms até o fundo");
    }
}
