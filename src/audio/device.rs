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

fn find(mut list: impl Iterator<Item = Device>, name: &str) -> Option<Device> {
    list.find(|d| name_of(d).as_deref() == Some(name))
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
    /// Nome e id, na ordem do servidor: a mesma que o [`find`] percorre.
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

/// Stream aberto, com o anel que liga o callback à thread de áudio.
pub struct Opened<R> {
    _stream: Stream,
    pub ring: R,
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
        name: name_of(&device).unwrap_or_default(),
        id: id_of(&device),
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
        name: name_of(&device).unwrap_or_default(),
        id: id_of(&device),
    })
}
