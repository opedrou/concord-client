//! Cadeia do mic: APM -> RNNoise -> gate. Processa bloco a bloco, sem I/O.
use livekit::webrtc::native::apm::AudioProcessingModule;
use nnnoiseless::DenoiseState;

use super::{
    FRAME, Meter, NoiseLevel, RATE, Sensitivity,
    gate::{Gate, level_db},
};

/// A bancada mede sem o gate, que zera a sobra de eco e esconderia o trabalho do APM.
#[cfg(test)]
pub static BYPASS_GATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A bancada grava o que entra e o que sai do gate, bloco a bloco.
#[cfg(test)]
pub static TRACE: std::sync::Mutex<Vec<Traced>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub struct Traced {
    pub entered: [i16; FRAME],
    pub left: [i16; FRAME],
    pub gain: f32,
    pub passing: bool,
}

pub struct MicChain {
    apm: AudioProcessingModule,
    rnnoise: Box<DenoiseState<'static>>,
    pub gate: Gate,
    meter: Meter,
    noise: NoiseLevel,
    pub muted: bool,
}

/// O AGC fica sempre ligado: mic a 25% sem ganho automático sai baixo demais.
fn new_apm(noise: NoiseLevel) -> AudioProcessingModule {
    AudioProcessingModule::new(true, true, true, noise == NoiseLevel::Apm)
}

impl MicChain {
    pub fn new(noise: NoiseLevel, sensitivity: Sensitivity, meter: Meter) -> Self {
        MicChain {
            apm: new_apm(noise),
            rnnoise: DenoiseState::new(),
            gate: Gate::new(sensitivity),
            meter,
            noise,
            muted: true,
        }
    }

    /// O binding não liga o NS do APM em tempo real. Recriar custa a adaptação do eco,
    /// por isso só acontece quando o NS do APM muda de verdade.
    pub fn set_noise(&mut self, noise: NoiseLevel) {
        if (noise == NoiseLevel::Apm) != (self.noise == NoiseLevel::Apm) {
            self.apm = new_apm(noise);
        }
        self.noise = noise;
    }

    /// Som final da saída: tem que entrar antes do bloco de mic correspondente.
    pub fn reverse(&mut self, block: &mut [i16; FRAME]) {
        self.apm
            .process_reverse_stream(block, RATE as i32, 1)
            .inspect_err(|e| eprintln!("áudio: APM reverse: {e}"))
            .ok();
    }

    /// Mudo só deixa de entregar: a cadeia roda igual pra não perder adaptação.
    pub fn process(&mut self, block: &mut [i16; FRAME]) -> Option<Vec<i16>> {
        self.apm
            .process_stream(block, RATE as i32, 1)
            .inspect_err(|e| eprintln!("áudio: APM: {e}"))
            .ok();
        let voice = self.denoise(block);
        let db = level_db(block);
        #[cfg(test)]
        if BYPASS_GATE.load(std::sync::atomic::Ordering::Relaxed) {
            return (!self.muted).then(|| block.to_vec());
        }
        #[cfg(test)]
        let entered = *block;
        self.gate.process(block, db, voice);
        self.meter.update(db, self.gate.passing());
        // O atraso do gate não pode soltar depois o que foi dito no mudo.
        if self.muted {
            self.gate.forget();
        }
        #[cfg(test)]
        TRACE.lock().unwrap().push(Traced {
            entered,
            left: *block,
            gain: self.gate.gain(),
            passing: self.gate.passing(),
        });
        (!self.muted).then(|| block.to_vec())
    }

    /// Roda em todo nível: o automático precisa da probabilidade de voz, e trocar o
    /// nível não pode começar o RNNoise do zero. O áudio limpo só vale com
    /// [`NoiseLevel::Rnnoise`].
    fn denoise(&mut self, block: &mut [i16; FRAME]) -> f32 {
        let input = block.map(|s| s as f32);
        let mut output = [0f32; FRAME];
        let voice = self.rnnoise.process_frame(&mut output, &input);
        if self.noise == NoiseLevel::Rnnoise {
            *block = output.map(|s| s as i16);
        }
        voice
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speech_like(t: usize) -> [i16; FRAME] {
        std::array::from_fn(|i| {
            let x = (t * FRAME + i) as f32 / RATE as f32;
            let tone = (x * 220.0 * std::f32::consts::TAU).sin()
                + 0.5 * (x * 440.0 * std::f32::consts::TAU).sin();
            (tone * 6000.0) as i16
        })
    }

    #[test]
    fn comeca_mudo_e_nao_entrega_bloco() {
        let mut chain = MicChain::new(NoiseLevel::Off, Sensitivity::Manual(-80.0), Meter::new());
        for t in 0..50 {
            assert_eq!(chain.process(&mut speech_like(t)), None);
        }
    }

    #[test]
    fn sem_mudo_entrega_blocos_de_480() {
        let mut chain = MicChain::new(
            NoiseLevel::Rnnoise,
            Sensitivity::Manual(-80.0),
            Meter::new(),
        );
        chain.muted = false;
        for t in 0..50 {
            chain.reverse(&mut [0; FRAME]);
            let block = chain.process(&mut speech_like(t)).expect("bloco");
            assert_eq!(block.len(), FRAME);
        }
    }

    #[test]
    fn mudo_mantem_o_apm_processando() {
        // Mudo e sem mudo têm que sair idênticos, senão o mudo pulou etapa da cadeia.
        let run = |muted_until: usize| {
            let mut chain =
                MicChain::new(NoiseLevel::Off, Sensitivity::Manual(-80.0), Meter::new());
            let mut last = [0; FRAME];
            for t in 0..300 {
                chain.muted = t < muted_until;
                last = speech_like(t);
                chain.process(&mut last);
            }
            last
        };
        // Desmuta antes do fim, pra o último bloco já ter saído do atraso do gate.
        assert_eq!(run(290), run(0));
    }

    #[test]
    fn desmutar_nao_solta_o_que_foi_dito_no_mudo() {
        let mut chain = MicChain::new(NoiseLevel::Off, Sensitivity::Manual(-80.0), Meter::new());
        for t in 0..50 {
            chain.process(&mut speech_like(t));
        }
        chain.muted = false;
        let silent = (50..60)
            .map(|t| chain.process(&mut speech_like(t)).expect("bloco"))
            .take_while(|block| block.iter().all(|s| *s == 0))
            .count();
        assert_eq!(silent, super::super::gate::LOOKAHEAD / FRAME);
    }

    #[test]
    fn medidor_e_limitador_seguem_com_o_mic_mudo() {
        let meter = Meter::new();
        let mut chain = MicChain::new(NoiseLevel::Off, Sensitivity::Manual(-60.0), meter.clone());
        for t in 0..50 {
            assert_eq!(chain.process(&mut speech_like(t)), None);
        }
        assert!(meter.level_db() > -40.0, "{}", meter.level_db());
        assert!(meter.open());
    }

    #[test]
    fn rnnoise_roda_em_todo_nivel_mas_so_limpa_no_dele() {
        let voice = |noise| {
            let mut chain = MicChain::new(noise, Sensitivity::Auto, Meter::new());
            let (mut sum, mut changed) = (0.0, false);
            for t in 0..100 {
                let before = speech_like(t);
                let mut block = before;
                sum += chain.denoise(&mut block);
                changed |= block != before;
            }
            (sum, changed)
        };
        let (off, off_changed) = voice(NoiseLevel::Off);
        let (rnnoise, rnnoise_changed) = voice(NoiseLevel::Rnnoise);
        assert_eq!(off, rnnoise, "mesma probabilidade nos dois");
        assert!(off > 0.0);
        assert!(!off_changed);
        assert!(rnnoise_changed);
    }

    #[test]
    fn trocar_o_nivel_nao_quebra_a_cadeia() {
        let mut chain = MicChain::new(
            NoiseLevel::Rnnoise,
            Sensitivity::Manual(-80.0),
            Meter::new(),
        );
        chain.muted = false;
        for (t, level) in [NoiseLevel::Off, NoiseLevel::Apm, NoiseLevel::Rnnoise]
            .into_iter()
            .enumerate()
        {
            chain.set_noise(level);
            assert!(chain.process(&mut speech_like(t)).is_some());
        }
    }
}
