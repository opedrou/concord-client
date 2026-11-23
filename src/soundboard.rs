//! Soundboard: o que vai e vem pela sala, no mesmo formato do site, e o arquivo baixado
//! convertido pro motor. Cada um baixa e toca o som na própria máquina: nada de áudio passa
//! pela sala.
use std::{
    collections::HashMap,
    io::Cursor,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::json;
use symphonia::core::{
    codecs::audio::AudioDecoderOptions,
    errors::Error as SymError,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

use crate::api::Sound;
use crate::audio::{RATE, ToMono};

/// Topic das mensagens de dados do LiveKit, o mesmo do site.
pub const TOPIC: &str = "soundboard";
/// Dois sons da mesma pessoa dentro disso: só o primeiro vale, como no site.
const COOLDOWN: Duration = Duration::from_millis(800);
/// ponytail: teto de 2 min depois do corte, porque o arquivo vem de outra pessoa e um mp3 de
/// 1 MiB em taxa baixa passa de 4 min. Subir se alguém quiser som mais longo.
const MAX_SECS: f64 = 120.0;

/// Mensagem da sala no topic [`TOPIC`].
#[derive(Clone, Debug, PartialEq)]
pub enum Wire {
    Play(Sound),
    /// Corta os sons da soundboard tocando, na máquina de todo mundo.
    Stop,
}

/// O que o site manda, com tudo opcional: mensagem sem `url` é ignorada, e sem corte vale o
/// arquivo inteiro.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Incoming {
    #[serde(default)]
    stop: bool,
    sound_id: Option<i64>,
    name: Option<String>,
    url: Option<String>,
    trim_start: Option<f64>,
    trim_end: Option<f64>,
}

impl Wire {
    /// `{"soundId","name","url","trimStart","trimEnd"}`, como o `play` do site.
    pub fn play(sound: &Sound) -> Vec<u8> {
        json!({
            "soundId": sound.id,
            "name": sound.name,
            "url": sound.url,
            "trimStart": sound.trim_start,
            "trimEnd": sound.trim_end,
        })
        .to_string()
        .into_bytes()
    }

    pub fn stop() -> Vec<u8> {
        br#"{"stop":true}"#.to_vec()
    }

    /// O `stop` vale antes de qualquer outra coisa, como no site.
    pub fn decode(payload: &[u8]) -> Option<Wire> {
        let m: Incoming = serde_json::from_slice(payload).ok()?;
        if m.stop {
            return Some(Wire::Stop);
        }
        Some(Wire::Play(Sound {
            id: m.sound_id.unwrap_or_default(),
            name: m.name.unwrap_or_else(|| "um som".into()),
            url: m.url?,
            trim_start: m.trim_start.unwrap_or_default(),
            trim_end: m.trim_end,
            uploaded_by: None,
        }))
    }
}

/// Um som por pessoa a cada [`COOLDOWN`], pelo nome. O relógio vem de fora, pros testes.
#[derive(Debug, Default)]
pub struct Cooldown(HashMap<String, Instant>);

impl Cooldown {
    /// `true` se o som de `by` vale agora; aí a espera recomeça.
    pub fn allow(&mut self, by: &str, now: Instant) -> bool {
        let waiting = self
            .0
            .get(by)
            .is_some_and(|t| now.saturating_duration_since(*t) < COOLDOWN);
        if !waiting {
            self.0.insert(by.to_owned(), now);
        }
        !waiting
    }
}

/// Arquivo baixado em mono a 48 kHz, já cortado. O corte é em segundos, como o site: fora do
/// arquivo vira a borda, e corte vazio dá um som vazio. Formato que não abre é erro.
pub fn decode(bytes: Vec<u8>, start: f64, end: Option<f64>) -> Result<Arc<[i16]>, String> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| e.to_string())?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or("sem faixa de áudio")?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("faixa sem codec de áudio")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let track_id = track.id;

    let seconds = |s: f64| (s.max(0.0) * RATE as f64) as usize;
    let first = seconds(start);
    let last = first + seconds(end.map_or(MAX_SECS, |e| (e - start).min(MAX_SECS)));
    let (mut out, mut at) = (Vec::new(), 0usize);
    let (mut mono, mut data): (Option<ToMono>, Vec<f32>) = (None, Vec::new());
    while at < last {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(b) => b,
            // Pacote estragado no meio: pula, como o exemplo do symphonia.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let spec = buffer.spec();
        let mono = mono
            .get_or_insert_with(|| ToMono::new(spec.channels().count().max(1) as u16, spec.rate()));
        buffer.copy_to_vec_interleaved(&mut data);
        mono.push(&data, |s| {
            if (first..last).contains(&at) {
                out.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
            at += 1;
        });
    }
    Ok(out.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn som(trim_end: Option<f64>) -> Sound {
        Sound {
            id: 3,
            name: "buzina".into(),
            url: "/api/sounds/3?v=ab.mp3".into(),
            trim_start: 0.5,
            trim_end,
            uploaded_by: None,
        }
    }

    #[test]
    fn mensagem_do_site_entra_e_a_nossa_sai_com_os_mesmos_campos() {
        let site = br#"{"soundId":3,"name":"buzina","url":"/api/sounds/3?v=ab.mp3","trimStart":0.5,"trimEnd":null}"#;
        assert_eq!(Wire::decode(site), Some(Wire::Play(som(None))));
        for s in [som(None), som(Some(1.25))] {
            let nossa: serde_json::Value = serde_json::from_slice(&Wire::play(&s)).unwrap();
            let mut campos: Vec<_> = nossa.as_object().unwrap().keys().cloned().collect();
            campos.sort();
            assert_eq!(campos, ["name", "soundId", "trimEnd", "trimStart", "url"]);
            assert_eq!(Wire::decode(&Wire::play(&s)), Some(Wire::Play(s)));
        }
        // Do site velho, sem corte e sem nome.
        let Some(Wire::Play(s)) = Wire::decode(br#"{"url":"/api/sounds/1?v=a.ogg"}"#) else {
            panic!("sem corte também toca");
        };
        assert_eq!(
            (s.trim_start, s.trim_end, s.name.as_str()),
            (0.0, None, "um som")
        );
    }

    #[test]
    fn parar_passa_antes_e_lixo_e_ignorado() {
        assert_eq!(Wire::decode(&Wire::stop()), Some(Wire::Stop));
        assert_eq!(Wire::decode(br#"{"stop":true}"#), Some(Wire::Stop));
        let com_url = br#"{"stop":true,"url":"/api/sounds/3?v=ab.mp3"}"#;
        assert_eq!(Wire::decode(com_url), Some(Wire::Stop));
        assert_eq!(Wire::decode(br#"{"name":"sem url"}"#), None);
        assert_eq!(Wire::decode(br#"{"url":3}"#), None);
        assert_eq!(Wire::decode(b"nada"), None);
    }

    #[test]
    fn cooldown_descarta_em_800_ms_por_pessoa() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut c = Cooldown::default();
        assert!(c.allow("ana", t0));
        assert!(!c.allow("ana", ms(799)));
        assert!(c.allow("bia", ms(799)), "outra pessoa não espera");
        assert!(c.allow("ana", ms(800)));
        assert!(!c.allow("ana", ms(1000)), "conta do último que valeu");
    }

    /// Wav de 1 s com amostras que contam: dá pra ver onde o corte caiu.
    fn wav(rate: u32, samples: &[i16]) -> Vec<u8> {
        let data = (samples.len() * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data.to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn corte_bate_em_amostras() {
        let rampa: Vec<i16> = (0..48_000).map(|i| (i % 30_000) as i16).collect();
        let pcm = decode(wav(48_000, &rampa), 0.25, Some(0.5)).unwrap();
        assert_eq!(pcm.len(), 12_000);
        // O `ToMono` atrasa uma amostra em 48 kHz.
        assert!((i32::from(pcm[0]) - 11_999).abs() <= 1, "{}", pcm[0]);
        let inteiro = decode(wav(48_000, &rampa), 0.0, None).unwrap();
        assert!(
            (47_990..=48_000).contains(&inteiro.len()),
            "{}",
            inteiro.len()
        );
        assert!(
            decode(wav(48_000, &rampa), 0.8, Some(0.2))
                .unwrap()
                .is_empty()
        );
        assert!(decode(wav(48_000, &rampa), 3.0, None).unwrap().is_empty());
        assert!(decode(b"isto nao e audio".to_vec(), 0.0, None).is_err());
    }

    #[test]
    fn decodifica_os_formatos_do_site_em_mono_48_khz() {
        let wav_22k: Vec<i16> = (0..6_615)
            .map(|i| ((i as f32 * 0.125).sin() * 8_000.0) as i16)
            .collect();
        let arquivos = [
            ("mp3", include_bytes!("../assets/test/som.mp3").to_vec()),
            ("ogg", include_bytes!("../assets/test/som.ogg").to_vec()),
            ("flac", include_bytes!("../assets/test/som.flac").to_vec()),
            ("m4a", include_bytes!("../assets/test/som.m4a").to_vec()),
            ("wav", wav(22_050, &wav_22k)),
        ];
        for (nome, bytes) in arquivos {
            let pcm = decode(bytes, 0.0, None).unwrap_or_else(|e| panic!("{nome}: {e}"));
            let secs = pcm.len() as f32 / RATE as f32;
            // mp3 e aac ganham o atraso do codificador no começo.
            assert!((0.28..0.40).contains(&secs), "{nome}: {secs} s");
            let rms = (pcm
                .iter()
                .map(|&s| (s as f32 / 32768.0).powi(2))
                .sum::<f32>()
                / pcm.len() as f32)
                .sqrt();
            // Seno a -12 dBFS (0,25 de pico) dá RMS de 0,18; o wav é de 0,24 de pico.
            assert!((0.12..0.22).contains(&rms), "{nome}: rms {rms}");
        }
    }
}
