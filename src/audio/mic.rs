//! Cadeia do mic: APM -> RNNoise -> gate. Processa bloco a bloco, sem I/O.
use livekit::webrtc::native::apm::AudioProcessingModule;
use nnnoiseless::DenoiseState;

use super::{FRAME, NoiseLevel, RATE, gate::Gate};

/// A bancada mede sem o gate, que zera a sobra de eco e esconderia o trabalho do APM.
#[cfg(test)]
pub static BYPASS_GATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct MicChain {
    apm: AudioProcessingModule,
    rnnoise: Box<DenoiseState<'static>>,
    gate: Gate,
    noise: NoiseLevel,
    pub muted: bool,
}

/// O AGC fica sempre ligado: mic a 25% sem ganho automático sai baixo demais.
fn new_apm(noise: NoiseLevel) -> AudioProcessingModule {
    AudioProcessingModule::new(true, true, true, noise == NoiseLevel::Apm)
}

impl MicChain {
    pub fn new(noise: NoiseLevel) -> Self {
        MicChain {
            apm: new_apm(noise),
            rnnoise: DenoiseState::new(),
            gate: Gate::new(),
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
        if self.noise == NoiseLevel::Rnnoise {
            self.denoise(block);
        }
        #[cfg(test)]
        if BYPASS_GATE.load(std::sync::atomic::Ordering::Relaxed) {
            return (!self.muted).then(|| block.to_vec());
        }
        self.gate.process(block);
        (!self.muted).then(|| block.to_vec())
    }

    fn denoise(&mut self, block: &mut [i16; FRAME]) {
        let input = block.map(|s| s as f32);
        let mut output = [0f32; FRAME];
        self.rnnoise.process_frame(&mut output, &input);
        *block = output.map(|s| s as i16);
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
        let mut chain = MicChain::new(NoiseLevel::Off);
        for t in 0..50 {
            assert_eq!(chain.process(&mut speech_like(t)), None);
        }
    }

    #[test]
    fn sem_mudo_entrega_blocos_de_480() {
        let mut chain = MicChain::new(NoiseLevel::Rnnoise);
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
            let mut chain = MicChain::new(NoiseLevel::Off);
            let mut last = [0; FRAME];
            for t in 0..300 {
                chain.muted = t < muted_until;
                last = speech_like(t);
                chain.process(&mut last);
            }
            last
        };
        assert_eq!(run(299), run(0));
    }

    #[test]
    fn trocar_o_nivel_nao_quebra_a_cadeia() {
        let mut chain = MicChain::new(NoiseLevel::Rnnoise);
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
