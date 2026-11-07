//! Gate de sensibilidade, com os valores da web. Decide em cima do áudio já limpo,
//! pra não abrir com clique de teclado.

/// Escala de 0 a 100. Fixo até existir o controle na interface.
const SENSITIVITY: f32 = 12.0;
const CLOSE_DB: f32 = -55.0 + SENSITIVITY / 100.0 * 40.0;
const OPEN_DB: f32 = CLOSE_DB + 6.0;
const SAMPLES_PER_MS: usize = 48;
const HOLD: usize = 400 * SAMPLES_PER_MS;
const ATTACK_STEP: f32 = 1.0 / (12 * SAMPLES_PER_MS) as f32;
const RAMP_STEP: f32 = 1.0 / (220 * SAMPLES_PER_MS) as f32;

pub struct Gate {
    open: bool,
    gain: f32,
    /// Amostras desde que o nível caiu abaixo do limiar de fechamento.
    quiet: Option<usize>,
}

impl Gate {
    pub fn new() -> Self {
        Gate {
            open: true,
            gain: 1.0,
            quiet: None,
        }
    }

    pub fn process(&mut self, block: &mut [i16]) {
        let db = level_db(block);
        if db >= OPEN_DB {
            self.open = true;
            self.quiet = None;
        } else if db < CLOSE_DB || self.quiet.is_some() {
            // Entre os limiares a contagem segue, mas só fecha com o nível abaixo do
            // fechamento: é o comportamento da web.
            let quiet = self.quiet.unwrap_or(0) + block.len();
            self.quiet = Some(quiet);
            if db < CLOSE_DB && quiet >= HOLD {
                self.open = false;
            }
        }
        for sample in block {
            self.gain = if self.open {
                (self.gain + ATTACK_STEP).min(1.0)
            } else {
                (self.gain - RAMP_STEP).max(0.0)
            };
            *sample = (*sample as f32 * self.gain) as i16;
        }
    }
}

/// RMS em dBFS. Bloco zerado dá infinito negativo.
fn level_db(block: &[i16]) -> f32 {
    let sum: f32 = block.iter().map(|s| (*s as f32 / 32768.0).powi(2)).sum();
    10.0 * (sum / block.len() as f32).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 480;

    /// Tom com o RMS pedido em dBFS.
    fn tone(db: f32) -> [i16; BLOCK] {
        let peak = 10f32.powf(db / 20.0) * 2f32.sqrt() * 32768.0;
        std::array::from_fn(|i| (peak * (i as f32 * 0.1).sin()) as i16)
    }

    fn feed(gate: &mut Gate, db: f32, blocks: usize) -> [i16; BLOCK] {
        let mut last = [0; BLOCK];
        for _ in 0..blocks {
            last = tone(db);
            gate.process(&mut last);
        }
        last
    }

    fn close_fully(gate: &mut Gate) {
        feed(gate, -80.0, 40 + 22);
        assert_eq!(gate.gain, 0.0);
    }

    #[test]
    fn limiares_do_valor_12() {
        assert!((CLOSE_DB - -50.2).abs() < 0.001);
        assert!((OPEN_DB - -44.2).abs() < 0.001);
    }

    #[test]
    fn hold_segura_400_ms_e_rampa_leva_220_ms() {
        let mut gate = Gate::new();
        feed(&mut gate, -80.0, 39);
        assert!(gate.open && gate.gain == 1.0, "390 ms: ainda aberto");
        feed(&mut gate, -80.0, 1);
        assert!(!gate.open, "400 ms: começa a fechar");
        assert!((gate.gain - (1.0 - 10.0 / 220.0)).abs() < 0.001);
        feed(&mut gate, -80.0, 20);
        assert!(gate.gain > 0.0, "610 ms: rampa ainda no fim");
        feed(&mut gate, -80.0, 2);
        assert_eq!(gate.gain, 0.0, "630 ms: fechado");
    }

    #[test]
    fn attack_abre_em_12_ms() {
        let mut gate = Gate::new();
        close_fully(&mut gate);
        feed(&mut gate, -40.0, 1);
        assert!(gate.open);
        assert!((gate.gain - 10.0 / 12.0).abs() < 0.001, "10 ms de 12");
        feed(&mut gate, -40.0, 1);
        assert_eq!(gate.gain, 1.0);
        let out = feed(&mut gate, -40.0, 1);
        assert_eq!(out, tone(-40.0), "aberto passa sem alterar");
    }

    #[test]
    fn nivel_entre_limiares_nao_abre_nem_fecha() {
        let mut gate = Gate::new();
        close_fully(&mut gate);
        feed(&mut gate, -47.0, 100);
        assert!(!gate.open && gate.gain == 0.0, "fechado continua fechado");

        let mut gate = Gate::new();
        feed(&mut gate, -47.0, 100);
        assert!(gate.open && gate.gain == 1.0, "aberto continua aberto");
    }

    #[test]
    fn nivel_alto_zera_o_hold() {
        let mut gate = Gate::new();
        feed(&mut gate, -80.0, 39);
        feed(&mut gate, -40.0, 1);
        feed(&mut gate, -80.0, 39);
        assert!(gate.open && gate.gain == 1.0);
    }

    #[test]
    fn voz_volta_no_meio_da_rampa() {
        let mut gate = Gate::new();
        feed(&mut gate, -80.0, 45);
        assert!(!gate.open && gate.gain < 1.0);
        feed(&mut gate, -40.0, 2);
        assert!(gate.open && gate.gain == 1.0);
    }

    #[test]
    fn silencio_digital_conta_como_abaixo() {
        let mut gate = Gate::new();
        for _ in 0..40 {
            gate.process(&mut [0; BLOCK]);
        }
        assert!(!gate.open);
    }
}
