//! Limitador de voz, como a sensibilidade de entrada do Discord. Nasce fechado e decide
//! por bloco em cima do áudio já limpo: o automático olha a probabilidade de voz do
//! RNNoise, o manual olha o mesmo nível que o medidor mostra.
//!
//! Limiares do automático medidos na bancada (`bancada_gate`). O nível mínimo vem depois
//! do AGC, que corre atrás de voz fraca: segura voz distante só nos primeiros segundos.
use super::{MANUAL_MIN_DB, Sensitivity};

const SAMPLES_PER_MS: usize = 48;
const HOLD: usize = 400 * SAMPLES_PER_MS;
const ATTACK_STEP: f32 = 1.0 / (12 * SAMPLES_PER_MS) as f32;
const RAMP_STEP: f32 = 1.0 / (220 * SAMPLES_PER_MS) as f32;
/// O áudio sai atrasado disto: a voz leva uns 30 ms pra ser reconhecida, e sem o atraso
/// esse pedaço do começo da frase se perde.
pub const LOOKAHEAD: usize = 50 * SAMPLES_PER_MS;

const VOICE_OPEN: f32 = 0.7;
const VOICE_KEEP: f32 = 0.5;
/// Abaixo disto a probabilidade não vale nada: o RNNoise passa de 0,9 em quase silêncio.
const VOICE_MIN_DB: f32 = -45.0;
/// Blocos seguidos de voz pra abrir: um clique de teclado cabe em dois.
const VOICE_BLOCKS: u32 = 3;
/// Blocos em que o automático não abre: o RNNoise frio chama ruído de voz por até 1,2 s.
const VOICE_WARMUP: u32 = 150;
/// O manual fecha este tanto abaixo do valor escolhido.
const MANUAL_CLOSE_BELOW_DB: f32 = 6.0;

pub struct Gate {
    sensitivity: Sensitivity,
    open: bool,
    gain: f32,
    /// Amostras desde o último motivo pra ficar aberto.
    quiet: usize,
    /// Blocos seguidos que pedem pra abrir o automático.
    voiced: u32,
    warmup: u32,
    delay: [i16; LOOKAHEAD],
    next: usize,
}

impl Gate {
    pub fn new(sensitivity: Sensitivity) -> Self {
        Gate {
            sensitivity,
            open: false,
            gain: 0.0,
            quiet: 0,
            voiced: 0,
            warmup: VOICE_WARMUP,
            delay: [0; LOOKAHEAD],
            next: 0,
        }
    }

    /// Vale a partir do próximo bloco; quem já está aberto ainda passa pelo hold.
    pub fn set_sensitivity(&mut self, sensitivity: Sensitivity) {
        self.sensitivity = sensitivity;
    }

    /// Descarta o que está no atraso, sem mexer na decisão.
    pub fn forget(&mut self) {
        self.delay.fill(0);
    }

    /// Algum som está saindo, mesmo que seja o fim da rampa.
    pub fn passing(&self) -> bool {
        self.gain > 0.0
    }

    #[cfg(test)]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// `db` vem de [`level_db`] e `voice` é a probabilidade de voz, de 0 a 1.
    pub fn process(&mut self, block: &mut [i16], db: f32, voice: f32) {
        let loud = db >= VOICE_MIN_DB;
        self.warmup = self.warmup.saturating_sub(1);
        self.voiced = if loud && voice >= VOICE_OPEN && self.warmup == 0 {
            self.voiced.saturating_add(1)
        } else {
            0
        };
        let (opens, keeps) = match self.sensitivity {
            Sensitivity::Auto => (self.voiced >= VOICE_BLOCKS, loud && voice >= VOICE_KEEP),
            Sensitivity::Manual(open_db) => (db >= open_db, db >= open_db - MANUAL_CLOSE_BELOW_DB),
        };
        if opens || (self.open && keeps) {
            self.open = true;
            self.quiet = 0;
        } else if self.open {
            self.quiet += block.len();
            self.open = self.quiet < HOLD;
        }
        for sample in block {
            self.gain = if self.open {
                (self.gain + ATTACK_STEP).min(1.0)
            } else {
                (self.gain - RAMP_STEP).max(0.0)
            };
            let delayed = std::mem::replace(&mut self.delay[self.next], *sample);
            self.next = (self.next + 1) % LOOKAHEAD;
            *sample = (delayed as f32 * self.gain) as i16;
        }
    }
}

/// RMS em dBFS. Bloco zerado fica no piso da escala, que é o fundo do slider.
pub fn level_db(block: &[i16]) -> f32 {
    let sum: f32 = block.iter().map(|s| (*s as f32 / 32768.0).powi(2)).sum();
    (10.0 * (sum / block.len() as f32).log10()).max(MANUAL_MIN_DB)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 480;
    const LOOK: usize = LOOKAHEAD / BLOCK;
    const MANUAL: Sensitivity = Sensitivity::Manual(-40.0);
    const HOLD_BLOCKS: usize = HOLD / BLOCK;
    const SILENCE: f32 = -80.0;

    /// Tom com o RMS pedido em dBFS.
    fn tone(db: f32) -> [i16; BLOCK] {
        let peak = 10f32.powf(db / 20.0) * 2f32.sqrt() * 32768.0;
        std::array::from_fn(|i| (peak * (i as f32 * 0.1).sin()) as i16)
    }

    fn feed(gate: &mut Gate, db: f32, voice: f32, blocks: usize) -> [i16; BLOCK] {
        let mut last = [0; BLOCK];
        for _ in 0..blocks {
            last = tone(db);
            let level = level_db(&last);
            gate.process(&mut last, level, voice);
        }
        last
    }

    /// Manual já aberto, com o gate no ganho cheio.
    fn opened() -> Gate {
        let mut gate = Gate::new(MANUAL);
        feed(&mut gate, -30.0, 0.0, 2);
        assert!(gate.open && gate.gain == 1.0);
        gate
    }

    fn closed(sensitivity: Sensitivity) -> Gate {
        let mut gate = Gate::new(sensitivity);
        feed(&mut gate, SILENCE, 0.0, VOICE_WARMUP as usize);
        assert!(!gate.open && gate.gain == 0.0);
        gate
    }

    fn close_fully(gate: &mut Gate) {
        feed(gate, SILENCE, 0.0, HOLD_BLOCKS + 22);
        assert_eq!(gate.gain, 0.0);
    }

    #[test]
    fn nasce_fechado_nos_dois_modos() {
        for sensitivity in [Sensitivity::Auto, MANUAL] {
            let mut gate = Gate::new(sensitivity);
            assert!(!gate.passing());
            let out = feed(&mut gate, -60.0, 0.0, 1);
            assert_eq!(out, [0; BLOCK]);
            assert!(!gate.open);
        }
    }

    #[test]
    fn manual_abre_no_valor_e_fecha_6_db_abaixo() {
        let mut gate = closed(MANUAL);
        gate.process(&mut [0; BLOCK], -40.1, 0.0);
        assert!(!gate.open, "abaixo do valor");
        gate.process(&mut [0; BLOCK], -40.0, 0.0);
        assert!(gate.open, "no valor");

        for _ in 0..HOLD_BLOCKS * 3 {
            gate.process(&mut [0; BLOCK], -46.0, 0.0);
        }
        assert!(gate.open, "6 dB abaixo ainda segura");
        for _ in 0..HOLD_BLOCKS {
            gate.process(&mut [0; BLOCK], -46.1, 0.0);
        }
        assert!(!gate.open, "abaixo disso fecha depois do hold");
    }

    #[test]
    fn hold_segura_400_ms_e_rampa_leva_220_ms() {
        let mut gate = opened();
        feed(&mut gate, SILENCE, 0.0, 39);
        assert!(gate.open && gate.gain == 1.0, "390 ms: ainda aberto");
        feed(&mut gate, SILENCE, 0.0, 1);
        assert!(!gate.open, "400 ms: começa a fechar");
        assert!((gate.gain - (1.0 - 10.0 / 220.0)).abs() < 0.001);
        feed(&mut gate, SILENCE, 0.0, 20);
        assert!(gate.passing(), "610 ms: rampa ainda no fim");
        feed(&mut gate, SILENCE, 0.0, 2);
        assert!(!gate.passing(), "630 ms: fechado");
    }

    #[test]
    fn attack_abre_em_12_ms() {
        let mut gate = closed(MANUAL);
        feed(&mut gate, -30.0, 0.0, 1);
        assert!(gate.open);
        assert!((gate.gain - 10.0 / 12.0).abs() < 0.001, "10 ms de 12");
        feed(&mut gate, -30.0, 0.0, 1);
        assert_eq!(gate.gain, 1.0);
    }

    #[test]
    fn atraso_guarda_o_comeco_da_fala_e_custa_50_ms() {
        let mut gate = Gate::new(MANUAL);
        let out = feed(&mut gate, -30.0, 0.0, LOOK);
        assert_eq!(out, [0; BLOCK], "até aqui saiu o que veio antes da fala");
        let out = feed(&mut gate, -30.0, 0.0, 1);
        assert_eq!(out, tone(-30.0), "primeiro bloco da fala sai inteiro");
        assert_eq!(LOOK, 5);
    }

    #[test]
    fn nivel_entre_limiares_nao_abre_nem_fecha() {
        let mut gate = closed(MANUAL);
        feed(&mut gate, -43.0, 0.0, 100);
        assert!(!gate.open && gate.gain == 0.0, "fechado continua fechado");

        let mut gate = opened();
        feed(&mut gate, -43.0, 0.0, 100);
        assert!(gate.open && gate.gain == 1.0, "aberto continua aberto");
    }

    #[test]
    fn nivel_alto_zera_o_hold() {
        let mut gate = opened();
        feed(&mut gate, SILENCE, 0.0, 39);
        feed(&mut gate, -30.0, 0.0, 1);
        feed(&mut gate, SILENCE, 0.0, 39);
        assert!(gate.open && gate.gain == 1.0);
    }

    #[test]
    fn voz_volta_no_meio_da_rampa() {
        let mut gate = opened();
        feed(&mut gate, SILENCE, 0.0, 45);
        assert!(!gate.open && gate.gain < 1.0);
        feed(&mut gate, -30.0, 0.0, 2);
        assert!(gate.open && gate.gain == 1.0);
    }

    #[test]
    fn silencio_digital_conta_como_abaixo() {
        let mut gate = opened();
        for _ in 0..40 {
            gate.process(&mut [0; BLOCK], level_db(&[0; BLOCK]), 0.0);
        }
        assert!(!gate.open);
    }

    #[test]
    fn piso_do_nivel_e_o_fundo_do_slider() {
        assert_eq!(level_db(&[0; BLOCK]), MANUAL_MIN_DB);
        assert!((level_db(&tone(-30.0)) - -30.0).abs() < 0.1);
    }

    #[test]
    fn automatico_abre_com_voz_sustentada() {
        let mut gate = closed(Sensitivity::Auto);
        feed(&mut gate, -30.0, 0.9, 2);
        assert!(!gate.open, "dois blocos ainda podem ser clique");
        feed(&mut gate, -30.0, 0.9, 1);
        assert!(gate.open);
    }

    #[test]
    fn automatico_ignora_clique_e_ruido_e_voz_baixa() {
        let mut gate = closed(Sensitivity::Auto);
        for _ in 0..50 {
            feed(&mut gate, -20.0, 0.95, 2);
            feed(&mut gate, -70.0, 0.95, 8);
        }
        assert!(!gate.open, "clique com probabilidade alta");

        feed(&mut gate, -20.0, 0.1, 100);
        assert!(!gate.open, "ruído forte sem voz");
        feed(&mut gate, -46.0, 1.0, 100);
        assert!(!gate.open, "voz abaixo do nível mínimo");
    }

    #[test]
    fn automatico_mantem_com_voz_media_e_fecha_depois_do_hold() {
        let mut gate = closed(Sensitivity::Auto);
        feed(&mut gate, -30.0, 0.9, 3);
        feed(&mut gate, -30.0, 0.5, 200);
        assert!(gate.open, "probabilidade média segura");
        feed(&mut gate, -30.0, 0.49, HOLD_BLOCKS - 1);
        assert!(gate.open, "ainda no hold");
        feed(&mut gate, -30.0, 0.49, 1);
        assert!(!gate.open, "acabou o hold");
    }

    #[test]
    fn automatico_nao_fica_preso_em_quase_silencio() {
        // O RNNoise segue acima de 0,9 no silêncio depois de uns cliques.
        let mut gate = closed(Sensitivity::Auto);
        feed(&mut gate, -30.0, 0.9, 3);
        feed(&mut gate, -70.0, 0.99, HOLD_BLOCKS);
        assert!(!gate.open);
    }

    #[test]
    fn automatico_nao_abre_com_o_rnnoise_frio() {
        let mut gate = Gate::new(Sensitivity::Auto);
        feed(&mut gate, -30.0, 0.9, VOICE_WARMUP as usize + 1);
        assert!(!gate.open, "aquecendo");
        feed(&mut gate, -30.0, 0.9, 1);
        assert!(gate.open);
    }

    #[test]
    fn troca_de_modo_vale_na_hora_e_passa_pelo_hold() {
        let mut gate = closed(Sensitivity::Auto);
        feed(&mut gate, -30.0, 0.0, 10);
        assert!(!gate.open, "automático ignora nível sem voz");
        gate.set_sensitivity(MANUAL);
        feed(&mut gate, -30.0, 0.0, 1);
        assert!(gate.open, "manual abre no bloco seguinte");

        gate.set_sensitivity(Sensitivity::Auto);
        feed(&mut gate, -30.0, 0.0, HOLD_BLOCKS - 1);
        assert!(gate.open, "aberto segue no hold");
        feed(&mut gate, -30.0, 0.0, 1);
        assert!(!gate.open, "automático fecha sem voz");

        gate.set_sensitivity(Sensitivity::Manual(-20.0));
        close_fully(&mut gate);
        feed(&mut gate, -30.0, 0.0, 10);
        assert!(!gate.open, "valor novo vale");
        gate.set_sensitivity(Sensitivity::Manual(-35.0));
        feed(&mut gate, -30.0, 0.0, 1);
        assert!(gate.open);
    }
}
