//! Áudio da transmissão, no Windows: WASAPI process loopback, como no spike (`s8`, `s9`).
//! Tela inteira leva o computador todo menos a árvore deste processo, ou seja, sem a call.
//! Janela leva só a árvore do processo dela: o Chrome toca num processo filho.
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use futures_util::FutureExt;
use livekit::webrtc::{
    audio_source::native::NativeAudioSource,
    prelude::{AudioFrame, AudioSourceOptions},
};
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat, initialize_mta};

use super::Source;
use crate::audio::{FRAME, RATE};

const CHANNELS: u32 = 2;
/// Quanto a thread espera um pacote antes de olhar se é pra parar.
const WAIT_MS: u32 = 100;

/// Captura em andamento. Soltar para a thread, que fecha o cliente WASAPI.
pub struct Loopback {
    pub source: NativeAudioSource,
    stop: Arc<AtomicBool>,
}

impl Loopback {
    /// `None` em `target` é a primeira tela. Volta `None` se o Windows recusar o loopback
    /// (anterior ao 10 20H2) ou a janela já tiver fechado.
    pub fn start(target: Option<&Source>) -> Option<Self> {
        // Sem AEC, NS nem AGC: são feitos pra voz e mutilam música e jogo.
        let source = NativeAudioSource::new(AudioSourceOptions::default(), RATE, CHANNELS, 0);
        let stop = open(target, &source)?;
        Some(Self { source, stop })
    }

    /// No Windows o que vai é o que foi escolhido no seletor: não há troca no ar.
    pub fn choose(&self, _: super::Choice) {}

    /// A transmissão trocou de alvo: o loopback troca junto, na mesma fonte, e a faixa
    /// publicada segue a mesma. Se o novo não abrir, fica mudo: o som do alvo antigo não
    /// combina com a imagem nova.
    pub fn retarget(&mut self, target: Option<&Source>) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(stop) = open(target, &self.source) {
            self.stop = stop;
        }
    }
}

/// Abre o loopback do alvo numa thread que entrega em `source`. Devolve o pedido de parar.
fn open(target: Option<&Source>, source: &NativeAudioSource) -> Option<Arc<AtomicBool>> {
    let (pid, include) = match target {
        Some(t) if t.window => (window_pid(t.id)?, true),
        _ => (std::process::id(), false),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready) = mpsc::channel();
    let (feed, flag) = (source.clone(), stop.clone());
    thread::Builder::new()
        .name("concord-tela-som".into())
        .spawn(move || capture(pid, include, &flag, &feed, ready_tx))
        .ok()?;
    ready.recv().ok()?.then_some(stop)
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Abre o cliente, avisa em `ready` se deu certo e entrega blocos de 10 ms até `stop`.
fn capture(
    pid: u32,
    include: bool,
    stop: &AtomicBool,
    source: &NativeAudioSource,
    ready: mpsc::Sender<bool>,
) {
    let opened = (|| {
        initialize_mta().ok().ok()?;
        let format = WaveFormat::new(32, 32, &SampleType::Float, RATE as usize, 2, None);
        let mut client = AudioClient::new_application_loopback_client(pid, include).ok()?;
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 0,
        };
        client
            .initialize_client(&format, &Direction::Capture, &mode)
            .ok()?;
        let event = client.set_get_eventhandle().ok()?;
        let capture = client.get_audiocaptureclient().ok()?;
        client.start_stream().ok()?;
        Some((client, event, capture))
    })();
    let _ = ready.send(opened.is_some());
    let Some((client, event, capture)) = opened else {
        return;
    };

    let mut bytes = VecDeque::new();
    let mut blocks = Blocks::default();
    while !stop.load(Ordering::Relaxed) {
        // Sem som tocando o loopback não manda nada: o site só ouve silêncio, sem buraco.
        let _ = event.wait_for_event(WAIT_MS);
        while capture.get_next_packet_size().ok().flatten().unwrap_or(0) > 0 {
            if capture.read_from_device_to_deque(&mut bytes).is_err() {
                break;
            }
        }
        let floats: Vec<f32> = bytes
            .drain(..bytes.len() / 4 * 4)
            .collect::<Vec<_>>()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        blocks.push(&floats, |samples| {
            let frame = AudioFrame {
                data: samples.into(),
                sample_rate: RATE,
                num_channels: CHANNELS,
                samples_per_channel: FRAME as u32,
            };
            let _ = source.capture_frame(&frame).now_or_never();
        });
    }
    let _ = client.stop_stream();
}

/// Junta o que o WASAPI entrega, em pacotes de qualquer tamanho, em blocos de 10 ms
/// estéreo intercalado, já em 16 bits.
#[derive(Default)]
struct Blocks {
    pending: Vec<f32>,
}

impl Blocks {
    fn push(&mut self, samples: &[f32], mut out: impl FnMut(&[i16])) {
        const BLOCK: usize = FRAME * CHANNELS as usize;
        self.pending.extend_from_slice(samples);
        let full = self.pending.len() / BLOCK * BLOCK;
        for block in self.pending[..full].as_chunks::<BLOCK>().0 {
            let pcm: Vec<i16> = block
                .iter()
                .map(|v| (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16)
                .collect();
            out(&pcm);
        }
        self.pending.drain(..full);
    }
}

/// PID do dono da janela. O id do `DesktopCapturer` no Windows é o próprio HWND.
fn window_pid(hwnd: u64) -> Option<u32> {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    }
    let mut pid = 0u32;
    // SAFETY: só lê o PID de um HWND; HWND inválido devolve 0 e não escreve nada.
    let thread = unsafe { GetWindowThreadProcessId(hwnd as isize, &mut pid) };
    (thread != 0 && pid != 0).then_some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junta_pacotes_soltos_em_blocos_de_10_ms() {
        let mut blocks = Blocks::default();
        let mut saida: Vec<Vec<i16>> = Vec::new();
        // 7 ms, depois 7 ms: dá um bloco e sobram 4 ms.
        let sete = vec![0.5f32; 336 * 2];
        blocks.push(&sete, |b| saida.push(b.to_vec()));
        assert!(saida.is_empty());
        blocks.push(&sete, |b| saida.push(b.to_vec()));
        assert_eq!(saida.len(), 1);
        assert_eq!(saida[0].len(), FRAME * 2);
        assert_eq!(saida[0][0], 16383);
        assert_eq!(blocks.pending.len(), (672 - 480) * 2);
    }

    #[test]
    fn satura_em_vez_de_dar_volta() {
        let mut blocks = Blocks::default();
        let mut saida = Vec::new();
        blocks.push(&vec![2.0f32; FRAME * 2], |b| saida.extend_from_slice(b));
        assert!(saida.iter().all(|&v| v == i16::MAX));
    }

    /// Abre o loopback de verdade, da tela, sem tocar nada:
    /// `cargo test -- --ignored loopback_abre`.
    #[test]
    #[ignore]
    fn loopback_abre_no_windows_de_verdade() {
        let inicio = std::time::Instant::now();
        let audio = Loopback::start(None);
        println!(
            "abriu={} em {} ms",
            audio.is_some(),
            inicio.elapsed().as_millis()
        );
        assert!(audio.is_some());
    }

    #[test]
    fn janela_invalida_nao_tem_pid() {
        assert_eq!(window_pid(0), None);
    }
}
