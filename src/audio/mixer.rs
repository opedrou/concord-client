//! Mistura as faixas remotas. Fila por faixa e volume por pessoa: mic e tela da mesma
//! pessoa não dividem fila.
use std::{
    collections::{HashMap, VecDeque},
    fmt::Write,
    sync::{Arc, Mutex},
};

use super::{FRAME, RATE, gain};

const MAX_QUEUED: usize = RATE as usize / 5;
const MAX_GAIN: f32 = 2.0;
/// Quem não tem volume salvo fica em 100%.
const DEFAULT_SLIDER: u8 = 71;
/// Volume padrão dos sons de interface no site (`DEFAULT_SOUND_VOLUME`).
const EFFECT_GAIN: f32 = 0.6;
/// Atraso do [`Echo`], em blocos de 10 ms: meio segundo pra ouvir o que acabou de falar.
const ECHO_BLOCKS: usize = 50;

/// A fila fica fora do `rtrb` porque só a sala e a thread de áudio tocam nela, nunca
/// o callback, e descartar o mais antigo exige o lado do produtor mexer na ponta do
/// consumidor.
#[derive(Default)]
pub struct VoiceQueue(Mutex<Queue>);

#[derive(Default)]
struct Queue {
    samples: VecDeque<i16>,
    /// Desde o último [`Mixer::report`]: o que chegou, o que caiu por fila cheia e a
    /// energia e o pico do que chegou. Separa "não chegou" de "chegou mudo".
    pushed: usize,
    dropped: usize,
    energy: f64,
    peak: u16,
}

impl VoiceQueue {
    pub fn push(&self, pcm: &[i16]) {
        let mut q = self.0.lock().unwrap();
        q.samples.extend(pcm);
        q.pushed += pcm.len();
        for &s in pcm {
            q.energy += f64::from(s) * f64::from(s);
            q.peak = q.peak.max(s.unsigned_abs());
        }
        let excess = q.samples.len().saturating_sub(MAX_QUEUED);
        q.samples.drain(..excess);
        q.dropped += excess;
    }
}

/// Nível em dBFS de uma amplitude de 0 a 32768.
fn dbfs(amplitude: f64) -> f64 {
    20.0 * (amplitude / 32768.0).log10()
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
    /// Sons de interface tocando: PCM `i16` LE e a posição em amostras. Ficam fora do
    /// volume geral e dos de pessoa.
    effects: Vec<(&'static [u8], usize)>,
    /// Confirmação do mic e do fone, no mesmo formato. Ver [`Mixer::cue`].
    cue: Option<(&'static [u8], usize)>,
    /// Sons da soundboard tocando: PCM, posição e ganho. Fora dos `effects`, pro "Parar"
    /// da soundboard não cortar os sons de interface.
    sounds: Vec<(Arc<[i16]>, usize, f32)>,
}

/// Soma um som de interface a partir de `pos`, até o fim do bloco ou do som.
fn add_effect(out: &mut [f32; FRAME], pcm: &[u8], pos: &mut usize) {
    for o in out.iter_mut() {
        let Some(b) = pcm.get(*pos * 2..*pos * 2 + 2) else {
            break;
        };
        *o += i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0 * EFFECT_GAIN;
        *pos += 1;
    }
}

impl Mixer {
    pub fn new(master: u8, volumes: HashMap<String, u8>) -> Self {
        Mixer {
            tracks: HashMap::new(),
            volumes,
            master,
            deafened: false,
            effects: Vec::new(),
            cue: None,
            sounds: Vec::new(),
        }
    }

    /// Surdo não começa som nenhum, como o site.
    pub fn play(&mut self, pcm: &'static [u8]) {
        if !self.deafened {
            self.effects.push((pcm, 0));
        }
    }

    /// Som da soundboard, com o ganho de quem tocou e o geral já multiplicados. Surdo não
    /// começa, como no site, e os sons se sobrepõem.
    pub fn sound(&mut self, pcm: Arc<[i16]>, gain: f32) {
        if !self.deafened {
            self.sounds.push((pcm, 0, gain.min(MAX_GAIN)));
        }
    }

    /// "Parar" da soundboard: só os sons dela.
    pub fn stop_sounds(&mut self) {
        self.sounds.clear();
    }

    /// Toca surdo também, que é quando ela diz que o fone desligou. Uma por vez: alternar
    /// rápido corta a anterior em vez de empilhar.
    pub fn cue(&mut self, pcm: &'static [u8]) {
        self.cue = Some((pcm, 0));
    }

    pub fn playing(&self) -> bool {
        !self.effects.is_empty() || self.cue.is_some()
    }

    pub fn add(&mut self, key: String, name: String, queue: Arc<VoiceQueue>) {
        self.tracks.insert(key, Track { name, queue });
    }

    pub fn set_person(&mut self, name: String, slider: u8) {
        self.volumes.insert(name, slider);
    }

    /// Uma linha pro log com cada faixa desde a última chamada: quanto chegou, em que nível e
    /// quanto caiu por fila cheia. Devolve também quantas faixas há e quanto está na fila,
    /// em ms, pros contadores de saúde.
    pub fn report(&self) -> (String, usize, usize) {
        let ms = |samples: usize| samples * 1000 / RATE as usize;
        let mut names: Vec<_> = self.tracks.values().collect();
        names.sort_by(|a, b| a.name.cmp(&b.name));
        let (mut text, mut queued) = (String::new(), 0);
        for t in names {
            let mut q = t.queue.0.lock().unwrap();
            let q = &mut *q;
            queued += q.samples.len();
            let _ = write!(
                text,
                "{}{} ",
                if text.is_empty() { "" } else { "; " },
                t.name
            );
            if q.pushed == 0 {
                text.push_str("nada chegou");
            } else if q.peak == 0 {
                let _ = write!(text, "{:.1} s de silêncio", q.pushed as f64 / RATE as f64);
            } else {
                let rms = (q.energy / q.pushed as f64).sqrt();
                let _ = write!(
                    text,
                    "{:.1} s a {:.0} dBFS (pico {:.0})",
                    q.pushed as f64 / RATE as f64,
                    dbfs(rms),
                    dbfs(f64::from(q.peak))
                );
            }
            if q.dropped > 0 {
                let _ = write!(text, ", descartou {} ms", ms(q.dropped));
            }
            (q.pushed, q.dropped, q.energy, q.peak) = (0, 0, 0.0, 0);
        }
        (text, self.tracks.len(), ms(queued))
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
            let available = queue.samples.len().min(FRAME);
            for (o, s) in out.iter_mut().zip(queue.samples.drain(..available)) {
                *o += s as f32 / 32768.0 * track_gain;
            }
        }
        for (pcm, pos) in &mut self.effects {
            add_effect(out, pcm, pos);
        }
        self.effects.retain(|(pcm, pos)| *pos * 2 < pcm.len());
        for (pcm, pos, gain) in &mut self.sounds {
            let end = (*pos + FRAME).min(pcm.len());
            for (o, s) in out.iter_mut().zip(&pcm[*pos..end]) {
                *o += *s as f32 / 32768.0 * *gain;
            }
            *pos = end;
        }
        self.sounds.retain(|(pcm, pos, _)| *pos < pcm.len());
        if self.deafened {
            out.fill(0.0);
        }
        if let Some((pcm, pos)) = &mut self.cue {
            add_effect(out, pcm, pos);
        }
        self.cue = self.cue.filter(|(pcm, pos)| *pos * 2 < pcm.len());
        for o in out {
            *o = o.clamp(-1.0, 1.0);
        }
    }
}

/// Retorno do mic na aba Voz: o som já tratado volta na saída [`ECHO_BLOCKS`] depois, como
/// uma faixa de voz. Soltar tira a faixa do mixer.
pub struct Echo {
    delay: VecDeque<Vec<i16>>,
    queue: Arc<VoiceQueue>,
}

impl Echo {
    pub fn new(mixer: &mut Mixer) -> Self {
        let queue = Arc::new(VoiceQueue::default());
        mixer.add("retorno".into(), "retorno do mic".into(), queue.clone());
        Echo {
            delay: VecDeque::new(),
            queue,
        }
    }

    pub fn push(&mut self, block: &[i16]) {
        self.delay.push_back(block.to_vec());
        if self.delay.len() > ECHO_BLOCKS
            && let Some(old) = self.delay.pop_front()
        {
            self.queue.push(&old);
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
        assert_eq!(q.0.lock().unwrap().samples.len(), FRAME);
        m.deafened = false;
        assert!(mix(&mut m)[0] > 0.0);
    }

    #[test]
    fn efeito_toca_ate_o_fim_sem_ganho_de_pessoa_nem_geral() {
        static PCM: [u8; 4] = [0x00, 0x40, 0x00, 0x40]; // 2 amostras de 16384
        let mut m = mixer(0);
        m.play(&PCM);
        let out = mix(&mut m);
        assert!(close(out[0], 0.5 * EFFECT_GAIN) && close(out[1], 0.5 * EFFECT_GAIN));
        assert_eq!(out[2], 0.0);
        assert!(!m.playing());
    }

    #[test]
    fn surdo_nao_toca_efeito() {
        static PCM: [u8; 2] = [0x00, 0x40];
        let mut m = mixer(71);
        m.deafened = true;
        m.play(&PCM);
        assert!(!m.playing());
        m.deafened = false;
        assert_eq!(mix(&mut m)[0], 0.0);
    }

    #[test]
    fn confirmacao_toca_surdo_e_a_nova_corta_a_anterior() {
        static UM: [u8; 2] = [0x00, 0x40]; // 16384
        static DOIS: [u8; 4] = [0x00, 0x20, 0x00, 0x20]; // 2 amostras de 8192
        let mut m = mixer(71);
        m.deafened = true;
        let q = voice(&mut m, "a", "ana");
        q.push(&[10000; FRAME]);
        m.cue(&UM);
        m.cue(&DOIS);
        let out = mix(&mut m);
        assert!(close(out[0], 0.25 * EFFECT_GAIN), "só a nova, sem a voz");
        assert!(close(out[1], 0.25 * EFFECT_GAIN) && out[2] == 0.0);
        assert!(!m.playing());
    }

    #[test]
    fn som_da_soundboard_tem_o_ganho_pronto_e_se_sobrepoe() {
        let mut m = mixer(0);
        let som: Arc<[i16]> = vec![16384; FRAME + 10].into();
        m.sound(som.clone(), 1.5);
        m.sound(som, 0.5);
        let out = mix(&mut m);
        assert!(close(out[0], 0.5 * 2.0), "1,5 + 0,5, sem o volume geral");
        let out = mix(&mut m);
        assert!(close(out[9], 1.0) && out[10] == 0.0, "acaba no fim do som");
        assert!(m.sounds.is_empty());
        m.sound(vec![16384; 4].into(), 9.0);
        assert!(close(mix(&mut m)[0], 0.5 * MAX_GAIN), "teto de ganho em 2");
    }

    #[test]
    fn surdo_nao_comeca_som_e_parar_so_corta_a_soundboard() {
        static BIPE: [u8; 4] = [0x00, 0x40, 0x00, 0x40];
        let mut m = mixer(71);
        m.deafened = true;
        m.sound(vec![10000; FRAME].into(), 1.0);
        assert!(m.sounds.is_empty());
        m.deafened = false;
        m.sound(vec![16384; FRAME * 3].into(), 1.0);
        m.play(&BIPE);
        m.stop_sounds();
        let out = mix(&mut m);
        assert!(close(out[0], 0.5 * EFFECT_GAIN), "o som de interface segue");
        assert_eq!(out[2], 0.0, "a soundboard parou");
    }

    #[test]
    fn retorno_do_mic_sai_meio_segundo_depois() {
        let mut m = mixer(71);
        let mut echo = Echo::new(&mut m);
        for i in 0..ECHO_BLOCKS {
            echo.push(&[1000 + i as i16; FRAME]);
            assert_eq!(mix(&mut m), [0.0; FRAME], "bloco {i} ainda no atraso");
        }
        echo.push(&[0; FRAME]);
        let out = mix(&mut m);
        assert!(
            close(out[0], 1000.0 / 32768.0 * gain(71) * gain(71)),
            "sai o primeiro"
        );
        drop(echo);
        mix(&mut m);
        assert!(m.tracks.is_empty());
    }

    #[test]
    fn fila_cheia_descarta_o_mais_antigo() {
        let q = VoiceQueue::default();
        let pcm: Vec<i16> = (0..MAX_QUEUED as i32 + 500).map(|i| i as i16).collect();
        q.push(&pcm);
        let queue = &q.0.lock().unwrap().samples;
        assert_eq!(queue.len(), 9600, "200 ms em 48 kHz");
        assert_eq!(queue.front(), Some(&500));
        assert_eq!(queue.back(), pcm.last());
    }

    #[test]
    fn relatorio_separa_nada_silencio_e_nivel_e_zera() {
        let mut m = mixer(71);
        let ana = voice(&mut m, "a", "ana");
        let bia = voice(&mut m, "b", "bia");
        let _caio = voice(&mut m, "c", "caio");
        // 300 ms em meia escala: -6 dBFS, e 100 ms passam da fila.
        ana.push(&vec![16384; MAX_QUEUED + RATE as usize / 10]);
        bia.push(&[0; FRAME]);
        let (text, tracks, queued) = m.report();
        assert_eq!(
            text,
            "ana 0.3 s a -6 dBFS (pico -6), descartou 100 ms; bia 0.0 s de silêncio; \
             caio nada chegou"
        );
        assert_eq!((tracks, queued), (3, 210), "200 ms da ana e 10 ms da bia");
        let (text, ..) = m.report();
        assert_eq!(text, "ana nada chegou; bia nada chegou; caio nada chegou");
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
