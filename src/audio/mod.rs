//! Motor de áudio: mic -> APM -> RNNoise -> gate -> sala, e sala -> mixer -> saída,
//! com o som final como referência do eco. Fixo em 48 kHz e blocos de 10 ms.
#[cfg(test)]
mod bancada;
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
};

use serde::{Deserialize, Serialize};

use mixer::VoiceQueue;
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
// Sem uso até a interface chegar nesta branch.
#[allow(dead_code)]
pub const MANUAL_DEFAULT_DB: f32 = -40.0;

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
    // Sem uso até a interface chegar nesta branch.
    #[allow(dead_code)]
    pub fn set_sensitivity(&self, sensitivity: Sensitivity) {
        self.send(Command::Sensitivity(sensitivity));
    }

    /// Mede também com o mic mudo.
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
