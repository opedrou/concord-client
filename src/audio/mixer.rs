//! Mistura as faixas remotas. Fila por faixa e volume por pessoa: mic e tela da mesma
//! pessoa não dividem fila.
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use super::{FRAME, RATE, gain};

const MAX_QUEUED: usize = RATE as usize / 5;
const MAX_GAIN: f32 = 2.0;
/// Quem não tem volume salvo fica em 100%.
const DEFAULT_SLIDER: u8 = 71;

/// A fila fica fora do `rtrb` porque só a sala e a thread de áudio tocam nela, nunca
/// o callback, e descartar o mais antigo exige o lado do produtor mexer na ponta do
/// consumidor.
#[derive(Default)]
pub struct VoiceQueue(Mutex<VecDeque<i16>>);

impl VoiceQueue {
    pub fn push(&self, pcm: &[i16]) {
        let mut queue = self.0.lock().unwrap();
        queue.extend(pcm);
        let excess = queue.len().saturating_sub(MAX_QUEUED);
        queue.drain(..excess);
    }
}

struct Track {
    name: String,
    queue: Arc<VoiceQueue>,
}

pub struct Mixer {
    tracks: HashMap<String, Track>,
    volumes: HashMap<String, u8>,
    pub master: u8,
    pub deafened: bool,
}

impl Mixer {
    pub fn new(master: u8, volumes: HashMap<String, u8>) -> Self {
        Mixer {
            tracks: HashMap::new(),
            volumes,
            master,
            deafened: false,
        }
    }

    pub fn add(&mut self, key: String, name: String, queue: Arc<VoiceQueue>) {
        self.tracks.insert(key, Track { name, queue });
    }

    pub fn set_person(&mut self, name: String, slider: u8) {
        self.volumes.insert(name, slider);
    }

    fn track_gain(&self, name: &str) -> f32 {
        let person = self.volumes.get(name).copied().unwrap_or(DEFAULT_SLIDER);
        (gain(person) * gain(self.master)).min(MAX_GAIN)
    }

    /// Surdo ainda consome as filas: ao sair, toca o agora, não o que ficou parado.
    pub fn mix(&mut self, out: &mut [f32; FRAME]) {
        // O mixer é o único outro dono da fila: contagem 1 quer dizer `VoiceSink` solto.
        self.tracks.retain(|_, t| Arc::strong_count(&t.queue) > 1);
        out.fill(0.0);
        for track in self.tracks.values() {
            let track_gain = self.track_gain(&track.name);
            let mut queue = track.queue.0.lock().unwrap();
            let available = queue.len().min(FRAME);
            for (o, s) in out.iter_mut().zip(queue.drain(..available)) {
                *o += s as f32 / 32768.0 * track_gain;
            }
        }
        for o in out {
            *o = if self.deafened {
                0.0
            } else {
                o.clamp(-1.0, 1.0)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(mixer: &mut Mixer, key: &str, name: &str) -> Arc<VoiceQueue> {
        let queue = Arc::new(VoiceQueue::default());
        mixer.add(key.into(), name.into(), queue.clone());
        queue
    }

    fn mix(mixer: &mut Mixer) -> [f32; FRAME] {
        let mut out = [1.0; FRAME];
        mixer.mix(&mut out);
        out
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.001
    }

    fn mixer(master: u8) -> Mixer {
        Mixer::new(master, HashMap::new())
    }

    #[test]
    fn ganho_individual_e_geral_multiplicam() {
        let mut m = mixer(50);
        m.set_person("ana".into(), 100);
        let q = voice(&mut m, "a", "ana");
        q.push(&[16384; FRAME]);
        assert!(close(mix(&mut m)[0], 0.5 * 2.0 * 0.5));
    }

    #[test]
    fn teto_de_ganho_em_2() {
        let mut m = mixer(100);
        m.set_person("ana".into(), 100);
        let q = voice(&mut m, "a", "ana");
        q.push(&[3277; FRAME]);
        assert!(close(mix(&mut m)[0], 0.1 * 2.0));
    }

    #[test]
    fn sem_entrada_no_mapa_usa_posicao_71() {
        let (mut sem, mut com) = (mixer(100), mixer(100));
        com.set_person("ana".into(), 71);
        let (a, b) = (voice(&mut sem, "a", "ana"), voice(&mut com, "a", "ana"));
        a.push(&[8192; FRAME]);
        b.push(&[8192; FRAME]);
        assert_eq!(mix(&mut sem), mix(&mut com));
    }

    #[test]
    fn volume_vale_para_todas_as_faixas_da_pessoa() {
        let mut m = mixer(71);
        let (a, b) = (voice(&mut m, "mic", "ana"), voice(&mut m, "tela", "ana"));
        m.set_person("ana".into(), 0);
        a.push(&[10000; FRAME]);
        b.push(&[10000; FRAME]);
        assert_eq!(mix(&mut m), [0.0; FRAME]);
    }

    #[test]
    fn soma_as_faixas_e_satura() {
        let mut m = mixer(100);
        m.set_person("ana".into(), 71);
        m.set_person("bia".into(), 71);
        let (a, b) = (voice(&mut m, "a", "ana"), voice(&mut m, "b", "bia"));
        // Ganho geral 2 e individual ~1 batem no teto: cada faixa vale 2x.
        a.push(&[4096; FRAME]);
        b.push(&[8192; FRAME]);
        assert!(close(mix(&mut m)[0], (0.125 + 0.25) * 2.0));
        a.push(&[30000; FRAME]);
        b.push(&[30000; FRAME]);
        assert_eq!(mix(&mut m), [1.0; FRAME]);
        a.push(&[-30000; FRAME]);
        b.push(&[-30000; FRAME]);
        assert_eq!(mix(&mut m), [-1.0; FRAME]);
    }

    #[test]
    fn fila_curta_completa_com_silencio() {
        let mut m = mixer(71);
        let q = voice(&mut m, "a", "ana");
        q.push(&[10000; 100]);
        let out = mix(&mut m);
        assert!(out[99] > 0.0 && out[100] == 0.0);
    }

    #[test]
    fn surdo_zera_tudo_e_consome_a_fila() {
        let mut m = mixer(71);
        let q = voice(&mut m, "a", "ana");
        q.push(&[10000; FRAME * 2]);
        m.deafened = true;
        assert_eq!(mix(&mut m), [0.0; FRAME]);
        assert_eq!(q.0.lock().unwrap().len(), FRAME);
        m.deafened = false;
        assert!(mix(&mut m)[0] > 0.0);
    }

    #[test]
    fn fila_cheia_descarta_o_mais_antigo() {
        let q = VoiceQueue::default();
        let pcm: Vec<i16> = (0..MAX_QUEUED as i32 + 500).map(|i| i as i16).collect();
        q.push(&pcm);
        let queue = q.0.lock().unwrap();
        assert_eq!(queue.len(), 9600, "200 ms em 48 kHz");
        assert_eq!(queue.front(), Some(&500));
        assert_eq!(queue.back(), pcm.last());
    }

    #[test]
    fn soltar_a_fila_tira_a_faixa() {
        let mut m = mixer(71);
        let q = voice(&mut m, "a", "ana");
        q.push(&[10000; FRAME * 4]);
        assert!(mix(&mut m)[0] > 0.0);
        drop(q);
        assert_eq!(mix(&mut m), [0.0; FRAME]);
        assert!(m.tracks.is_empty());
    }

    #[test]
    fn mesma_chave_troca_a_faixa() {
        let mut m = mixer(71);
        let antiga = voice(&mut m, "a", "ana");
        let nova = voice(&mut m, "a", "ana");
        antiga.push(&[10000; FRAME]);
        assert_eq!(mix(&mut m), [0.0; FRAME]);
        nova.push(&[10000; FRAME]);
        assert!(mix(&mut m)[0] > 0.0);
    }
}
