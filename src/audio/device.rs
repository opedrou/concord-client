//! Dispositivos e streams do cpal. Fixo em 48 kHz, f32 e buffer de 10 ms: com o buffer
//! padrão o eco caiu só 3 dB, com 10 ms caiu de 28 a 39 dB.
//!
//! O PulseAudio converte sozinho. O WASAPI só converte a saída: o cpal liga o
//! `AUTOCONVERTPCM` só nela, e mic com formato próprio estéreo ou a 44,1 kHz recusa mono
//! 48 kHz com `AUDCLNT_E_UNSUPPORTED_FORMAT`. Aí a entrada abre no formato dele e o
//! [`ToMono`] converte na borda, sem mexer no motor.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use cpal::{
    BufferSize, Device, Error, ErrorKind, Host, Stream, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};

use super::{Devices, FRAME, RATE};

/// Limita a saída a 30 ms e faz do espaço livre o relógio do mixer.
const OUTPUT_RING: usize = FRAME * 3;
const INPUT_RING: usize = FRAME * 10;
/// O servidor de áudio travado não pode segurar a thread de áudio pra sempre.
const OPEN_TIMEOUT: Option<Duration> = Some(Duration::from_secs(2));

/// Host novo a cada abertura: a conexão velha pode ser justamente o que caiu.
#[cfg(not(windows))]
pub fn host() -> Result<Host, String> {
    // Só PulseAudio: linkar libpipewire derruba o processo, ver o Cargo.toml.
    cpal::host_from_id(cpal::HostId::PulseAudio).map_err(|e| e.to_string())
}

#[cfg(windows)]
pub fn host() -> Result<Host, String> {
    Ok(cpal::default_host())
}

fn name_of(device: &Device) -> Option<String> {
    Some(device.description().ok()?.name().to_string())
}

fn is_monitor(device: &Device) -> bool {
    device
        .id()
        .is_ok_and(|id| id.to_string().ends_with(".monitor"))
}

pub fn list() -> Devices {
    let Ok(host) = host() else {
        return Devices::default();
    };
    let inputs = host.input_devices().into_iter().flatten();
    let outputs = host.output_devices().into_iter().flatten();
    Devices {
        inputs: inputs
            .filter(|d| !is_monitor(d))
            .filter_map(|d| name_of(&d))
            .collect(),
        outputs: outputs.filter_map(|d| name_of(&d)).collect(),
    }
}

/// O salvo, se está ligado, senão o padrão, sempre pelo dispositivo concreto da lista e
/// com a conta do [`Seen::target`], a mesma que o `follow` usa pra decidir se fecha. O
/// padrão virtual do cpal (`ActivateAudioInterfaceAsync`) falhou com `RPC_E_CHANGED_MODE`
/// logo depois de o fone Bluetooth se reconfigurar, em MTA e em STA, enquanto o mesmo
/// endpoint abria pela lista. Ele fica só de último recurso, com o padrão fora da lista.
fn pick(
    list: impl Iterator<Item = Device>,
    default: Option<Device>,
    name: Option<&str>,
) -> Option<Device> {
    let list: Vec<Device> = list.collect();
    let seen = Seen::of(default.clone(), list.iter().cloned());
    let target = seen.target(name).map(String::from);
    list.into_iter()
        .find(|d| target.is_some() && id_of(d) == target)
        .or(default)
}

fn id_of(device: &Device) -> Option<String> {
    Some(device.id().ok()?.to_string())
}

/// Um lado do servidor agora: o padrão e cada dispositivo, pra saber se o stream aberto
/// ainda está no lugar certo.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    /// Id do padrão do sistema.
    pub default: Option<String>,
    /// Nome e id, na ordem do servidor: a mesma que o [`pick`] percorre.
    pub devices: Vec<(String, String)>,
}

impl Seen {
    fn of(default: Option<Device>, list: impl Iterator<Item = Device>) -> Self {
        Seen {
            default: default.as_ref().and_then(id_of),
            devices: list
                .filter_map(|d| Some((name_of(&d)?, id_of(&d)?)))
                .collect(),
        }
    }

    /// Id de onde o `open_*` abriria agora: o salvo, se está ligado, senão o padrão.
    pub fn target(&self, wanted: Option<&str>) -> Option<&str> {
        wanted
            .and_then(|w| self.devices.iter().find(|(name, _)| name == w))
            .map(|(_, id)| id.as_str())
            .or(self.default.as_deref())
    }
}

/// Saída e entrada como estão no servidor. Bloqueia; `None` se a conexão caiu.
pub fn look(host: &Host) -> Option<(Seen, Seen)> {
    let outputs = host.output_devices().ok()?;
    let inputs = host.input_devices().ok()?;
    Some((
        Seen::of(host.default_output_device(), outputs),
        Seen::of(host.default_input_device(), inputs),
    ))
}

/// Falhas do callback, contadas lá dentro sem trava e lidas no relatório da thread de áudio.
#[derive(Default)]
pub struct Glitches {
    /// Amostras que faltaram (a saída tocou silêncio) ou sobraram (a entrada perdeu).
    missed: AtomicUsize,
    /// Maior intervalo entre dois callbacks, em µs: o sistema que atrasou o áudio.
    gap_us: AtomicU64,
}

impl Glitches {
    /// Marca a chamada do callback em `now`, depois da anterior em `last`.
    fn called(&self, last: &mut Option<Instant>, now: Instant) {
        if let Some(l) = last.replace(now) {
            let gap = now.duration_since(l).as_micros() as u64;
            self.gap_us.fetch_max(gap, Ordering::Relaxed);
        }
    }

    /// Amostras perdidas e o maior intervalo, em ms, desde a última chamada.
    pub fn take(&self) -> (usize, u64) {
        let missed = self.missed.swap(0, Ordering::Relaxed);
        let gap = self.gap_us.swap(0, Ordering::Relaxed);
        (missed * 1000 / RATE as usize, gap / 1000)
    }
}

/// Stream aberto, com o anel que liga o callback à thread de áudio.
pub struct Opened<R> {
    _stream: Stream,
    pub ring: R,
    pub glitches: Arc<Glitches>,
    lost: Arc<AtomicBool>,
    /// Nome e id do dispositivo de verdade, que pode não ser o pedido.
    pub name: String,
    pub id: Option<String>,
}

impl<R> Opened<R> {
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }
}

/// Buffer de 10 ms na taxa pedida.
fn config(channels: u16, rate: u32) -> StreamConfig {
    StreamConfig {
        channels,
        sample_rate: rate,
        buffer_size: BufferSize::Fixed(rate / 100),
    }
}

/// Junta os canais em mono e leva a taxa do dispositivo a 48 kHz. Em mono a 48 kHz só
/// atrasa uma amostra.
///
/// ponytail: interpolação linear sem filtro. Sobe 44,1 kHz bem; descer de 96 kHz dobra
/// ruído acima de 24 kHz pra dentro da banda. Rubato aqui se alguém ouvir isso.
struct ToMono {
    channels: usize,
    /// Amostras de entrada por amostra de saída.
    step: f64,
    /// Posição da próxima saída, contada da amostra anterior, em amostras de entrada.
    pos: f64,
    prev: f32,
}

impl ToMono {
    fn new(channels: u16, rate: u32) -> Self {
        ToMono {
            channels: channels.max(1) as usize,
            step: rate as f64 / RATE as f64,
            pos: 0.0,
            prev: 0.0,
        }
    }

    fn push(&mut self, data: &[f32], mut out: impl FnMut(f32)) {
        for frame in data.chunks(self.channels) {
            let cur = frame.iter().sum::<f32>() / frame.len() as f32;
            while self.pos < 1.0 {
                out(self.prev + (cur - self.prev) * self.pos as f32);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
    }
}

fn on_error(what: &'static str) -> (Arc<AtomicBool>, impl FnMut(Error) + Send + 'static) {
    let lost = Arc::new(AtomicBool::new(false));
    let flag = lost.clone();
    let callback = move |e: Error| {
        log!("áudio: erro no stream de {what}: {e}");
        if !matches!(
            e.kind(),
            ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied
        ) {
            flag.store(true, Ordering::Relaxed);
        }
    };
    (lost, callback)
}

/// Sem o dispositivo salvo, usa o padrão do sistema.
pub fn open_output(name: Option<&str>) -> Result<Opened<Producer<f32>>, String> {
    let host = host()?;
    let list = host.output_devices().into_iter().flatten();
    let device =
        pick(list, host.default_output_device(), name).ok_or("sem dispositivo de saída")?;
    let (ring, mut samples) = RingBuffer::new(OUTPUT_RING);
    let (lost, errors) = on_error("saída");
    let glitches = Arc::new(Glitches::default());
    let counter = glitches.clone();
    let mut last = None;
    let stream = device
        .build_output_stream(
            config(2, RATE),
            move |data: &mut [f32], _| {
                counter.called(&mut last, Instant::now());
                let mut missed = 0;
                for frame in data.chunks_mut(2) {
                    frame.fill(samples.pop().unwrap_or_else(|_| {
                        missed += 1;
                        0.0
                    }));
                }
                if missed > 0 {
                    counter.missed.fetch_add(missed, Ordering::Relaxed);
                }
            },
            errors,
            OPEN_TIMEOUT,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Opened {
        _stream: stream,
        ring,
        glitches,
        lost,
        name: name_of(&device).unwrap_or_default(),
        id: id_of(&device),
    })
}

/// Sem o dispositivo salvo, usa o padrão do sistema. Mono a 48 kHz, senão no formato
/// do próprio dispositivo, convertido pelo [`ToMono`].
pub fn open_input(name: Option<&str>) -> Result<Opened<Consumer<f32>>, String> {
    let host = host()?;
    let list = host.input_devices().into_iter().flatten();
    let device =
        pick(list, host.default_input_device(), name).ok_or("sem dispositivo de entrada")?;
    input_stream(&device, 1, RATE).or_else(|e| {
        let own = device
            .default_input_config()
            .map_err(|own| format!("{e}; formato do dispositivo: {own}"))?;
        let (channels, rate) = (own.channels(), own.sample_rate());
        let opened = input_stream(&device, channels, rate)
            .map_err(|own| format!("{e}; em {channels} canais a {rate} Hz: {own}"))?;
        log!("áudio: entrada recusou mono 48 kHz, abriu em {channels} canais a {rate} Hz");
        Ok(opened)
    })
}

fn input_stream(
    device: &Device,
    channels: u16,
    rate: u32,
) -> Result<Opened<Consumer<f32>>, String> {
    let (mut samples, ring) = RingBuffer::new(INPUT_RING);
    let (lost, errors) = on_error("entrada");
    let glitches = Arc::new(Glitches::default());
    let counter = glitches.clone();
    let mut last = None;
    let mut mono = ToMono::new(channels, rate);
    let stream = device
        .build_input_stream(
            config(channels, rate),
            move |data: &[f32], _| {
                counter.called(&mut last, Instant::now());
                // Anel cheio perde o mais novo: só acontece com a thread parada.
                let mut missed = 0;
                mono.push(data, |s| missed += samples.push(s).is_err() as usize);
                if missed > 0 {
                    counter.missed.fetch_add(missed, Ordering::Relaxed);
                }
            },
            errors,
            OPEN_TIMEOUT,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(Opened {
        _stream: stream,
        ring,
        glitches,
        lost,
        name: name_of(device).unwrap_or_default(),
        id: id_of(device),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(channels: u16, rate: u32, data: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        let mut mono = ToMono::new(channels, rate);
        // Em pedaços de 10 ms, como o callback entrega.
        for chunk in data.chunks(rate as usize / 100 * channels as usize) {
            mono.push(chunk, |s| out.push(s));
        }
        out
    }

    #[test]
    fn mono_48k_passa_igual() {
        let data: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin()).collect();
        let out = convert(1, RATE, &data);
        assert_eq!(out.len(), data.len());
        assert_eq!(out[1..], data[..data.len() - 1]);
    }

    #[test]
    fn estereo_44k_vira_mono_48k() {
        // 1 s de estéreo constante, esquerda 1 e direita 0,5.
        let data: Vec<f32> = (0..44_100).flat_map(|_| [1.0, 0.5]).collect();
        let out = convert(2, 44_100, &data);
        assert!(out.len().abs_diff(48_000) <= 1, "{} amostras", out.len());
        assert!(out[100..].iter().all(|s| (s - 0.75).abs() < 1e-6));
    }

    #[test]
    fn seno_sobrevive_a_reamostragem() {
        // 440 Hz a 44,1 kHz sai 440 Hz a 48 kHz, atrasado uma amostra de entrada.
        let tone = |t: f64| (std::f64::consts::TAU * 440.0 * t).sin() as f32;
        let data: Vec<f32> = (0..44_100).map(|i| tone(i as f64 / 44_100.0)).collect();
        let out = convert(1, 44_100, &data);
        let worst = (100..47_000)
            .map(|k| (out[k] - tone(k as f64 / 48_000.0 - 1.0 / 44_100.0)).abs())
            .fold(0f32, f32::max);
        assert!(worst < 1e-3, "erro {worst}");
    }
}
