//! Dispositivos e streams do cpal. Fixo em 48 kHz, f32 e buffer de 10 ms: com o buffer
//! padrão o eco caiu só 3 dB, com 10 ms caiu de 28 a 39 dB.
//!
//! ponytail: sem rubato. O PulseAudio já reamostra e o WASAPI funcionou em 44,1 kHz na
//! VM. Se um dispositivo no Windows recusar 48 kHz, o stream não abre: é onde o rubato
//! entra, na borda, sem mexer no motor.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
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
fn host() -> Result<Host, String> {
    // Só PulseAudio: linkar libpipewire derruba o processo, ver o Cargo.toml.
    cpal::host_from_id(cpal::HostId::PulseAudio).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn host() -> Result<Host, String> {
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

fn find(mut list: impl Iterator<Item = Device>, name: &str) -> Option<Device> {
    list.find(|d| name_of(d).as_deref() == Some(name))
}

/// Stream aberto, com o anel que liga o callback à thread de áudio.
pub struct Opened<R> {
    _stream: Stream,
    pub ring: R,
    lost: Arc<AtomicBool>,
}

impl<R> Opened<R> {
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }
}

fn config(channels: u16) -> StreamConfig {
    StreamConfig {
        channels,
        sample_rate: RATE,
        buffer_size: BufferSize::Fixed(FRAME as u32),
    }
}

fn on_error(what: &'static str) -> (Arc<AtomicBool>, impl FnMut(Error) + Send + 'static) {
    let lost = Arc::new(AtomicBool::new(false));
    let flag = lost.clone();
    let callback = move |e: Error| {
        eprintln!("áudio: {what}: {e}");
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
    let device = name
        .and_then(|name| find(host.output_devices().ok()?, name))
        .or_else(|| host.default_output_device())
        .ok_or("sem dispositivo de saída")?;
    let (ring, mut samples) = RingBuffer::new(OUTPUT_RING);
    let (lost, errors) = on_error("saída");
    let stream = device
        .build_output_stream(
            config(2),
            move |data: &mut [f32], _| {
                for frame in data.chunks_mut(2) {
                    frame.fill(samples.pop().unwrap_or(0.0));
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
        lost,
    })
}

/// Sem o dispositivo salvo, usa o padrão do sistema.
pub fn open_input(name: Option<&str>) -> Result<Opened<Consumer<f32>>, String> {
    let host = host()?;
    let device = name
        .and_then(|name| find(host.input_devices().ok()?, name))
        .or_else(|| host.default_input_device())
        .ok_or("sem dispositivo de entrada")?;
    let (mut samples, ring) = RingBuffer::new(INPUT_RING);
    let (lost, errors) = on_error("entrada");
    let stream = device
        .build_input_stream(
            config(1),
            move |data: &[f32], _| {
                for sample in data {
                    // Anel cheio perde o mais novo: só acontece com a thread parada.
                    let _ = samples.push(*sample);
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
        lost,
    })
}
