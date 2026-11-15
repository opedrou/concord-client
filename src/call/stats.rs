//! Estatísticas do WebRTC no log a cada [`EVERY`]: por faixa, a rede (taxa, perda, jitter,
//! áudio ocultado) e o vídeo (resolução, fps, decodificador e fps desenhado). Separa bug do
//! cliente de rede ruim, e "não chegou pacote" de "chegou e não tocou".
use std::{
    collections::HashMap,
    fmt::Write,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use livekit::{
    track::{LocalTrack, RemoteTrack, TrackSource},
    webrtc::stats::{IceCandidatePairState, RtcStats},
};
use tokio::time::timeout;

pub const EVERY: Duration = Duration::from_secs(30);
/// Um `get_stats` travado não pode prender a tarefa.
const WAIT: Duration = Duration::from_secs(3);

pub enum Track {
    Remote(RemoteTrack),
    Local(LocalTrack),
}

/// Uma faixa, como ela aparece no log, e os quadros já desenhados da tela assistida.
pub struct Labeled {
    pub label: String,
    pub track: Track,
    pub drawn: Option<u64>,
}

/// Contadores da volta anterior por id do stream, pra tirar taxa do acumulado.
#[derive(Clone, Default)]
pub struct Last(Arc<Mutex<HashMap<String, (Instant, Counters)>>>);

#[derive(Clone, Copy, Default)]
struct Counters {
    bytes: u64,
    packets: u64,
    lost: i64,
    concealed: u64,
    samples: u64,
    dropped: u32,
    freezes: u32,
    drawn: u64,
}

impl Counters {
    fn since(self, p: Counters) -> Counters {
        Counters {
            bytes: self.bytes.saturating_sub(p.bytes),
            packets: self.packets.saturating_sub(p.packets),
            lost: self.lost.saturating_sub(p.lost).max(0),
            concealed: self.concealed.saturating_sub(p.concealed),
            samples: self.samples.saturating_sub(p.samples),
            dropped: self.dropped.saturating_sub(p.dropped),
            freezes: self.freezes.saturating_sub(p.freezes),
            drawn: self.drawn.saturating_sub(p.drawn),
        }
    }
}

/// Como a fonte aparece no log: "mic", "tela".
pub fn source(source: TrackSource, name: &str) -> &'static str {
    match source {
        TrackSource::Microphone => "mic",
        TrackSource::Camera => "câmera",
        TrackSource::Screenshare => "tela",
        TrackSource::ScreenshareAudio => "áudio da tela",
        _ if name == crate::share::TRACK => "áudio da tela",
        _ => "faixa",
    }
}

/// Lê as estatísticas de cada faixa e grava uma linha por stream, mais o RTT de cada lado.
pub async fn log(tracks: Vec<Labeled>, last: Last) {
    let mut rtt = [None, None];
    for t in tracks {
        let stats = match &t.track {
            Track::Remote(r) => timeout(WAIT, r.get_stats()).await,
            Track::Local(l) => timeout(WAIT, l.get_stats()).await,
        };
        let Ok(Ok(stats)) = stats else {
            log!("rede: {}: estatísticas não vieram", t.label);
            continue;
        };
        let side = usize::from(matches!(t.track, Track::Local(_)));
        rtt[side] = rtt[side].or(round_trip(&stats));
        let lines = {
            let mut last = last.0.lock().unwrap_or_else(|e| e.into_inner());
            lines(&t.label, &stats, &mut last, Instant::now(), t.drawn)
        };
        for line in lines {
            log!("{line}");
        }
    }
    let ms = |r: Option<f64>| r.map_or("?".into(), |r| format!("{r:.0} ms"));
    if rtt != [None, None] {
        log!(
            "rede: RTT recebendo {}, enviando {}",
            ms(rtt[0]),
            ms(rtt[1])
        );
    }
}

/// RTT do par de candidatos em uso, em ms.
fn round_trip(stats: &[RtcStats]) -> Option<f64> {
    stats
        .iter()
        .filter_map(|s| match s {
            RtcStats::CandidatePair(p)
                if p.candidate_pair.nominated
                    && p.candidate_pair.state == Some(IceCandidatePairState::Succeeded) =>
            {
                Some(p.candidate_pair.current_round_trip_time * 1000.0)
            }
            _ => None,
        })
        .reduce(f64::max)
}

/// `opus`, `VP8`: o fim do `mimeType`.
fn codec(stats: &[RtcStats], id: &str) -> String {
    stats
        .iter()
        .find_map(|s| match s {
            RtcStats::Codec(c) if c.rtc.id == id => c.codec.mime_type.rsplit('/').next(),
            _ => None,
        })
        .unwrap_or("?")
        .to_owned()
}

fn pct(part: f64, whole: f64) -> f64 {
    if whole > 0.0 {
        part * 100.0 / whole
    } else {
        0.0
    }
}

fn implementation(name: &str, hardware: bool) -> String {
    let name = if name.is_empty() { "?" } else { name };
    let kind = if hardware { "hardware" } else { "software" };
    format!("{name} ({kind})")
}

/// Uma linha por stream de `stats`. Taxa e fps desenhado só a partir da segunda volta, que
/// tem com o que comparar; perda e áudio ocultado vêm do acumulado na primeira.
fn lines(
    label: &str,
    stats: &[RtcStats],
    last: &mut HashMap<String, (Instant, Counters)>,
    now: Instant,
    drawn: Option<u64>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut delta = |id: &str, c: Counters| match last.insert(id.to_owned(), (now, c)) {
        Some((at, prev)) => (c.since(prev), Some(now.duration_since(at).as_secs_f64())),
        None => (c, None),
    };
    for s in stats {
        match s {
            RtcStats::InboundRtp(i) => {
                let r = &i.inbound;
                let c = Counters {
                    bytes: r.bytes_received,
                    packets: i.received.packets_received,
                    lost: i.received.packets_lost,
                    concealed: r.concealed_samples,
                    samples: r.total_samples_received,
                    dropped: r.frames_dropped,
                    freezes: r.freeze_count,
                    drawn: drawn.unwrap_or(0),
                };
                let (d, dt) = delta(&i.rtc.id, c);
                let video = i.stream.kind == "video";
                let mut t = if video {
                    format!(
                        "vídeo: {label}: {} {}x{} {:.0} fps, {}",
                        codec(stats, &i.stream.codec_id),
                        r.frame_width,
                        r.frame_height,
                        r.frames_per_second,
                        implementation(&r.decoder_implementation, r.power_efficient_decoder)
                    )
                } else {
                    format!("rede: {label}: {}", codec(stats, &i.stream.codec_id))
                };
                if let Some(dt) = dt {
                    let _ = write!(t, ", {:.0} kb/s", d.bytes as f64 * 8.0 / dt / 1000.0);
                }
                if dt.is_some() && d.packets == 0 {
                    t.push_str(", nenhum pacote chegou");
                } else {
                    let lost = d.lost as f64;
                    let _ = write!(t, ", perda {:.1}%", pct(lost, lost + d.packets as f64));
                }
                if video {
                    let _ = write!(
                        t,
                        ", {} quadros descartados, {} congelamentos",
                        d.dropped, d.freezes
                    );
                    if let (Some(dt), Some(_)) = (dt, drawn) {
                        let _ = write!(t, ", {:.0} fps na tela", d.drawn as f64 / dt);
                    }
                } else {
                    let _ = write!(
                        t,
                        ", jitter {:.0} ms, ocultado {:.1}%",
                        i.received.jitter * 1000.0,
                        pct(d.concealed as f64, d.samples as f64)
                    );
                }
                out.push(t);
            }
            RtcStats::OutboundRtp(o) if o.outbound.active || o.stream.kind == "audio" => {
                let c = Counters {
                    bytes: o.sent.bytes_sent,
                    packets: o.sent.packets_sent,
                    ..Counters::default()
                };
                let (d, dt) = delta(&o.rtc.id, c);
                let remote = stats.iter().find_map(|s| match s {
                    RtcStats::RemoteInboundRtp(r) if r.remote_inbound.local_id == o.rtc.id => {
                        Some(&r.remote_inbound)
                    }
                    _ => None,
                });
                let w = &o.outbound;
                let rid = if w.rid.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", w.rid)
                };
                let mut t = if o.stream.kind == "video" {
                    format!(
                        "vídeo: {label}{rid}: {} {}x{} {:.0} fps, {}, limitada por {:?}",
                        codec(stats, &o.stream.codec_id),
                        w.frame_width,
                        w.frame_height,
                        w.frames_per_second,
                        implementation(&w.encoder_implementation, w.power_efficient_encoder),
                        w.quality_limitation_reason
                    )
                } else {
                    format!("rede: {label}: {}", codec(stats, &o.stream.codec_id))
                };
                if let Some(dt) = dt {
                    let _ = write!(t, ", {:.0} kb/s", d.bytes as f64 * 8.0 / dt / 1000.0);
                }
                if let Some(r) = remote {
                    let _ = write!(
                        t,
                        ", perda no servidor {:.1}%, RTT {:.0} ms",
                        r.fraction_lost * 100.0,
                        r.round_trip_time * 1000.0
                    );
                }
                out.push(t);
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use livekit::webrtc::stats::{CodecStats, InboundRtpStats};

    use super::*;

    fn codec_stats(id: &str, mime: &str) -> RtcStats {
        let mut c = CodecStats::default();
        c.rtc.id = id.into();
        c.codec.mime_type = mime.into();
        RtcStats::Codec(c)
    }

    fn inbound(kind: &str, bytes: u64, packets: u64, lost: i64) -> RtcStats {
        let mut i = InboundRtpStats::default();
        i.rtc.id = "in".into();
        i.stream.kind = kind.into();
        i.stream.codec_id = "c".into();
        i.received.packets_received = packets;
        i.received.packets_lost = lost;
        i.received.jitter = 0.012;
        i.inbound.bytes_received = bytes;
        i.inbound.total_samples_received = 1000;
        i.inbound.concealed_samples = 20;
        i.inbound.frame_width = 1920;
        i.inbound.frame_height = 1080;
        i.inbound.frames_per_second = 30.0;
        i.inbound.decoder_implementation = "libvpx".into();
        RtcStats::InboundRtp(i)
    }

    #[test]
    fn audio_mostra_perda_e_taxa_na_segunda_volta() {
        let mut last = HashMap::new();
        let t0 = Instant::now();
        let first = [codec_stats("c", "audio/opus"), inbound("audio", 0, 990, 10)];
        assert_eq!(
            lines("mic de ana", &first, &mut last, t0, None),
            ["rede: mic de ana: opus, perda 1.0%, jitter 12 ms, ocultado 2.0%"]
        );
        let second = [
            codec_stats("c", "audio/opus"),
            inbound("audio", 120_000, 990, 10),
        ];
        assert_eq!(
            lines("mic de ana", &second, &mut last, t0 + EVERY, None),
            ["rede: mic de ana: opus, 32 kb/s, nenhum pacote chegou, jitter 12 ms, ocultado 0.0%"]
        );
    }

    #[test]
    fn video_mostra_decodificador_e_fps_na_tela() {
        let mut last = HashMap::new();
        let t0 = Instant::now();
        let stats = [codec_stats("c", "video/VP8"), inbound("video", 0, 100, 0)];
        lines("tela de bia", &stats, &mut last, t0, Some(10));
        let stats = [codec_stats("c", "video/VP8"), inbound("video", 0, 200, 0)];
        assert_eq!(
            lines("tela de bia", &stats, &mut last, t0 + EVERY, Some(10 + 840)),
            [
                "vídeo: tela de bia: VP8 1920x1080 30 fps, libvpx (software), 0 kb/s, \
                 perda 0.0%, 0 quadros descartados, 0 congelamentos, 28 fps na tela"
            ]
        );
    }
}
