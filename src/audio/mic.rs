//! Cadeia do mic: APM -> ganho manual -> RNNoise -> limite -> gate. Processa bloco a bloco,
//! sem I/O.
use livekit::webrtc::native::apm::AudioProcessingModule;
use nnnoiseless::DenoiseState;

use super::{
    FRAME, Meter, MicGain, NoiseLevel, RATE, Sensitivity,
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
    gain: MicGain,
    pub muted: bool,
}

/// O AGC fica ligado no automático: mic a 25% sem ganho automático sai baixo demais. No
/// manual ele desfaria o ajuste da pessoa.
fn new_apm(noise: NoiseLevel, gain: MicGain) -> AudioProcessingModule {
    AudioProcessingModule::new(true, gain == MicGain::Auto, true, noise == NoiseLevel::Apm)
}

/// Até aqui a amostra passa igual: -6 dBFS.
const KNEE: f32 = 0.5;

/// Limite suave do ganho manual, numa amostra na escala do `i16`. Desligar o AGC leva
/// junto o limitador que vem nele, e voz alta com ganho alto bateria seco no teto. Acima
/// do joelho uma tangente hiperbólica encosta no teto, com a mesma inclinação no joelho.
/// Não tem memória: não atrasa nem bombeia, mas distorce o pico de quem passa muito do
/// teto. Se isso incomodar, o próximo passo é um limitador com envelope.
fn limit(sample: f32) -> i16 {
    let x = sample / 32768.0;
    let over = (x.abs() - KNEE).max(0.0) / (1.0 - KNEE);
    let y = x.signum() * (x.abs().min(KNEE) + (1.0 - KNEE) * over.tanh());
    // `as` satura: 1,0 vira 32767.
    (y * 32768.0) as i16
}

impl MicChain {
    pub fn new(noise: NoiseLevel, sensitivity: Sensitivity, gain: MicGain, meter: Meter) -> Self {
        MicChain {
            apm: new_apm(noise, gain),
            rnnoise: DenoiseState::new(),
            gate: Gate::new(sensitivity),
            meter,
            noise,
            gain,
            muted: true,
        }
    }

    /// O binding não liga o NS do APM em tempo real. Recriar custa a adaptação do eco,
    /// por isso só acontece quando o NS do APM muda de verdade.
    pub fn set_noise(&mut self, noise: NoiseLevel) {
        if (noise == NoiseLevel::Apm) != (self.noise == NoiseLevel::Apm) {
            self.apm = new_apm(noise, self.gain);
        }
        self.noise = noise;
    }

    /// Com o AGC é a mesma coisa: só recria na troca entre automático e manual. Mudar o
    /// valor do manual vale no bloco seguinte.
    pub fn set_gain(&mut self, gain: MicGain) {
        if (gain == MicGain::Auto) != (self.gain == MicGain::Auto) {
            self.apm = new_apm(self.noise, gain);
        }
        self.gain = gain;
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
    ///
    /// O ganho manual entra aqui, no lugar do AGC: o RNNoise, o limitador de voz e o
    /// medidor veem o mesmo que veriam depois dele, e o eco do APM segue medido no mic
    /// cru. Segue em float até a saída: filtrar um pico achatado faz ele passar do teto de
    /// novo, então o limite só vale depois do RNNoise.
    fn denoise(&mut self, block: &mut [i16; FRAME]) -> f32 {
        let gain = match self.gain {
            MicGain::Auto => 1.0,
            MicGain::Manual(db) => 10f32.powf(db / 20.0),
        };
        let input = block.map(|s| s as f32 * gain);
        let mut output = [0f32; FRAME];
        let voice = self.rnnoise.process_frame(&mut output, &input);
        let clean = if self.noise == NoiseLevel::Rnnoise {
            output
        } else {
            input
        };
        *block = match self.gain {
            MicGain::Auto => clean.map(|s| s as i16),
            MicGain::Manual(_) => clean.map(limit),
        };
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
        let mut chain = MicChain::new(
            NoiseLevel::Off,
            Sensitivity::Manual(-80.0),
            MicGain::Auto,
            Meter::new(),
        );
        for t in 0..50 {
            assert_eq!(chain.process(&mut speech_like(t)), None);
        }
    }

    #[test]
    fn sem_mudo_entrega_blocos_de_480() {
        let mut chain = MicChain::new(
            NoiseLevel::Rnnoise,
            Sensitivity::Manual(-80.0),
            MicGain::Auto,
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
            let mut chain = MicChain::new(
                NoiseLevel::Off,
                Sensitivity::Manual(-80.0),
                MicGain::Auto,
                Meter::new(),
            );
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
        let mut chain = MicChain::new(
            NoiseLevel::Off,
            Sensitivity::Manual(-80.0),
            MicGain::Auto,
            Meter::new(),
        );
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
        let mut chain = MicChain::new(
            NoiseLevel::Off,
            Sensitivity::Manual(-60.0),
            MicGain::Auto,
            meter.clone(),
        );
        for t in 0..50 {
            assert_eq!(chain.process(&mut speech_like(t)), None);
        }
        assert!(meter.level_db() > -40.0, "{}", meter.level_db());
        assert!(meter.open());
    }

    #[test]
    fn rnnoise_roda_em_todo_nivel_mas_so_limpa_no_dele() {
        let voice = |noise| {
            let mut chain = MicChain::new(noise, Sensitivity::Auto, MicGain::Auto, Meter::new());
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
            MicGain::Auto,
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

    #[test]
    fn limite_suave_so_age_acima_de_menos_6_dbfs() {
        for s in [-16384i16, -9000, -1, 0, 1, 9000, 16384] {
            assert_eq!(limit(s as f32), s, "abaixo do joelho passa igual");
        }
        // Do silêncio a 20 dB acima do teto: sobe sempre, sem passar do `i16`.
        let ramp: Vec<i16> = (0..=1000).map(|i| limit(i as f32 * 327.68)).collect();
        assert!(ramp.windows(2).all(|w| w[0] <= w[1]));
        assert!(
            limit(32768.0) < 32767,
            "o teto de entrada ainda não encosta"
        );
        assert!(ramp[1000] >= 32_700);
        assert!(limit(-327_680.0) <= -32_700);
    }

    #[test]
    fn ganho_alto_nao_corta_nem_depois_do_rnnoise() {
        let mut chain = MicChain::new(
            NoiseLevel::Rnnoise,
            Sensitivity::Manual(-80.0),
            MicGain::Manual(20.0),
            Meter::new(),
        );
        chain.muted = false;
        let peak = (0..300)
            .filter_map(|t| chain.process(&mut speech_like(t)))
            .flatten()
            .map(|s| s.unsigned_abs())
            .max();
        // O tom tem pico a -12,5 dBFS: com +20 dB passaria do teto em 7,5 dB.
        assert!(
            peak.is_some_and(|p| (30_000..32_767).contains(&p)),
            "{peak:?}"
        );
    }

    /// Saída do mic em dBFS depois de 3 s, com o gate aberto.
    fn output_db(chain: &mut MicChain, from: usize) -> f32 {
        chain.muted = false;
        let mut last = vec![];
        for t in from..from + 300 {
            chain.reverse(&mut [0; FRAME]);
            last = chain.process(&mut speech_like(t)).expect("bloco");
        }
        level_db(&last)
    }

    #[test]
    fn ganho_manual_vale_na_hora() {
        let chain = |gain| {
            MicChain::new(
                NoiseLevel::Off,
                Sensitivity::Manual(-80.0),
                gain,
                Meter::new(),
            )
        };
        // O tom entra a -16,8 dBFS. O AGC não mexe em tom, que não é voz: a diferença
        // dele pro manual só aparece com fala, na `bancada_ganho`.
        let zero = output_db(&mut chain(MicGain::Manual(0.0)), 0);
        let minus = output_db(&mut chain(MicGain::Manual(-6.0)), 0);
        let auto = output_db(&mut chain(MicGain::Auto), 0);
        assert!((zero - -16.8).abs() < 0.5, "{zero}");
        assert!((zero - minus - 6.0).abs() < 0.1, "{zero} {minus}");

        let mut live = chain(MicGain::Manual(0.0));
        output_db(&mut live, 0);
        live.set_gain(MicGain::Manual(-6.0));
        let changed = output_db(&mut live, 300);
        assert!((changed - minus).abs() < 0.1, "{changed} {minus}");
        live.set_gain(MicGain::Auto);
        let back = output_db(&mut live, 600);
        assert!((back - auto).abs() < 1.0, "{back} {auto}");
    }
}
