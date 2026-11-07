//! Config e preferências em `~/.config/concord/` ou `%APPDATA%\Concord\`.
use std::collections::HashMap;
use std::{env, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audio::{
    AudioPrefs, MANUAL_DEFAULT_DB, MANUAL_MAX_DB, MANUAL_MIN_DB, NoiseLevel, Sensitivity,
};
use crate::share::Quality;

pub const SIDEBAR_MIN: f32 = 200.0;
pub const SIDEBAR_MAX: f32 = 480.0;
const SIDEBAR_PADRAO: f32 = 288.0;
/// Posição do slider em que o ganho é 1: a mesma da web.
pub const VOLUME_PADRAO: u8 = 71;
/// Sufixo da chave de volume da transmissão de alguém. Ver [`Config::screen_key`].
pub const SCREEN: &str = " · tela";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: Option<String>,
    /// Fixo por instalação: a identity na sala é `<username>__<install_id>`.
    pub install_id: String,
    pub sidebar_width: f32,
    pub mic_muted: bool,
    pub deafened: bool,
    pub mic_before_deafen: bool,
    /// Nome do dispositivo; `None` é o padrão do sistema.
    pub input: Option<String>,
    pub output: Option<String>,
    pub noise: NoiseLevel,
    pub sensitivity: Sensitivity,
    /// Último limiar manual, guardado também com o automático ligado.
    pub manual_db: f32,
    pub master_volume: u8,
    /// Nome da pessoa -> slider. Quem não está aqui fica em [`VOLUME_PADRAO`].
    pub volumes: HashMap<String, u8>,
    pub share_quality: Quality,
    /// Interruptor "Transmitir áudio". Só no Linux por enquanto.
    pub share_audio: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: None,
            install_id: Uuid::new_v4().to_string(),
            sidebar_width: SIDEBAR_PADRAO,
            mic_muted: false,
            deafened: false,
            mic_before_deafen: false,
            input: None,
            output: None,
            noise: NoiseLevel::default(),
            sensitivity: Sensitivity::default(),
            manual_db: MANUAL_DEFAULT_DB,
            master_volume: VOLUME_PADRAO,
            volumes: HashMap::new(),
            share_quality: Quality::default(),
            share_audio: true,
        }
    }
}

impl Config {
    /// Arquivo ilegível vira config nova: o que se perde é o endereço e as
    /// preferências, nada que impeça de abrir.
    pub fn load() -> Self {
        let mut c: Self = path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if Uuid::parse_str(&c.install_id).is_err() {
            c.install_id = Uuid::new_v4().to_string();
        }
        c.sidebar_width = c.sidebar_width.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        c.master_volume = c.master_volume.min(100);
        c.manual_db = clamp_db(c.manual_db);
        if let Sensitivity::Manual(db) = c.sensitivity {
            c.manual_db = clamp_db(db);
            c.sensitivity = Sensitivity::Manual(c.manual_db);
        }
        c.volumes.values_mut().for_each(|v| *v = (*v).min(100));
        c
    }

    /// Escreve num temporário e renomeia, pra queda no meio não truncar o arquivo.
    pub fn save(&self) -> io::Result<()> {
        let p = path().ok_or_else(|| io::Error::other("sem pasta de config"))?;
        fs::create_dir_all(p.parent().expect("config tem pasta"))?;
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        fs::rename(tmp, p)
    }

    pub fn audio_prefs(&self) -> AudioPrefs {
        AudioPrefs {
            input: self.input.clone(),
            output: self.output.clone(),
            noise: self.noise,
            sensitivity: self.sensitivity,
            master: self.master_volume,
            volumes: self.volumes.clone(),
        }
    }

    /// Desligar o automático volta ao último limiar manual.
    pub fn set_auto(&mut self, auto: bool) {
        self.sensitivity = if auto {
            Sensitivity::Auto
        } else {
            Sensitivity::Manual(self.manual_db)
        };
    }

    /// Só vale no modo manual; o slider nem aparece no automático.
    pub fn set_threshold(&mut self, db: f32) {
        if let Sensitivity::Manual(_) = self.sensitivity {
            self.manual_db = clamp_db(db);
            self.sensitivity = Sensitivity::Manual(self.manual_db);
        }
    }

    /// Chave do volume da transmissão de `name`: separada da voz, e é o rótulo do tile.
    pub fn screen_key(name: &str) -> String {
        format!("{name}{SCREEN}")
    }

    pub fn volume_of(&self, name: &str) -> u8 {
        self.volumes.get(name).copied().unwrap_or(VOLUME_PADRAO)
    }

    /// Nomes com voz em zero, em ordem, pra `concord.muted`. Volume de transmissão não
    /// é voz: fica fora.
    pub fn muted_people(&self) -> Vec<String> {
        let mut names: Vec<_> = self
            .volumes
            .iter()
            .filter(|(n, v)| **v == 0 && !n.ends_with(SCREEN))
            .map(|(n, _)| n.clone())
            .collect();
        names.sort();
        names
    }

    /// Devolve a lista de mudos nova só quando alguém entrou ou saiu do zero.
    pub fn set_volume(&mut self, name: &str, slider: u8) -> Option<Vec<String>> {
        let slider = slider.min(100);
        let was_zero = self.volume_of(name) == 0;
        self.volumes.insert(name.to_owned(), slider);
        (was_zero != (slider == 0)).then(|| self.muted_people())
    }

    /// Desmutar desliga o surdo, igual à web (`setMicMuted`).
    pub fn toggle_mic(&mut self) {
        self.mic_muted = !self.mic_muted;
        if !self.mic_muted {
            self.deafened = false;
        }
    }

    /// Ficar surdo muta junto; sair devolve o mic ao estado anterior.
    pub fn toggle_deafen(&mut self) {
        if self.deafened {
            self.deafened = false;
            self.mic_muted = self.mic_before_deafen;
        } else {
            self.mic_before_deafen = self.mic_muted;
            self.deafened = true;
            self.mic_muted = true;
        }
    }
}

/// Mantém o limiar dentro do slider; NaN do arquivo cai no mínimo.
pub fn clamp_db(db: f32) -> f32 {
    if db.is_nan() {
        MANUAL_MIN_DB
    } else {
        db.clamp(MANUAL_MIN_DB, MANUAL_MAX_DB)
    }
}

fn path() -> Option<PathBuf> {
    Some(dir()?.join("config.json"))
}

#[cfg(windows)]
fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(env::var_os("APPDATA")?).join("Concord"))
}

#[cfg(not(windows))]
fn dir() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".config")))?;
    Some(base.join("concord"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surdo_guarda_e_devolve_o_mic() {
        let mut c = Config::default();
        c.toggle_deafen();
        assert!(c.deafened && c.mic_muted);
        c.toggle_deafen();
        assert!(!c.deafened && !c.mic_muted);

        c.toggle_mic();
        c.toggle_deafen();
        c.toggle_deafen();
        assert!(
            !c.deafened && c.mic_muted,
            "quem já estava mudo continua mudo"
        );
    }

    #[test]
    fn desmutar_desliga_o_surdo() {
        let mut c = Config::default();
        c.toggle_deafen();
        c.toggle_mic();
        assert!(!c.deafened && !c.mic_muted);
    }

    #[test]
    fn volume_so_avisa_ao_cruzar_o_zero() {
        let mut c = Config::default();
        assert_eq!(c.volume_of("Ana"), VOLUME_PADRAO);
        assert_eq!(c.set_volume("Ana", 40), None);
        assert_eq!(c.set_volume("Ana", 0), Some(vec!["Ana".to_owned()]));
        assert_eq!(c.set_volume("Ana", 0), None);
        assert_eq!(
            c.set_volume("Bia", 0),
            Some(vec!["Ana".into(), "Bia".into()])
        );
        assert_eq!(c.set_volume("Ana", 5), Some(vec!["Bia".to_owned()]));
    }

    #[test]
    fn config_antiga_ganha_os_padroes_de_voz() {
        let c: Config = serde_json::from_str(r#"{"mic_muted":true}"#).unwrap();
        let p = c.audio_prefs();
        assert_eq!(
            (p.input, p.noise, p.master),
            (None, NoiseLevel::Rnnoise, 71)
        );
        assert!(p.volumes.is_empty());
    }

    #[test]
    fn volume_de_transmissao_em_zero_nao_vira_mudo_de_voz() {
        let mut c = Config::default();
        let tela = Config::screen_key("Ana");
        assert_eq!(tela, "Ana · tela");
        c.set_volume(&tela, 0);
        c.set_volume("Bia", 0);
        assert_eq!(c.muted_people(), ["Bia"]);
        assert_eq!(c.volume_of("Ana"), VOLUME_PADRAO, "a voz da Ana não mudou");
    }

    #[test]
    fn config_antiga_vem_em_automatico_com_o_manual_padrao() {
        let c: Config = serde_json::from_str(r#"{"mic_muted":true}"#).unwrap();
        assert_eq!(c.sensitivity, Sensitivity::Auto);
        assert_eq!(c.manual_db, MANUAL_DEFAULT_DB);
        assert_eq!(c.audio_prefs().sensitivity, Sensitivity::Auto);
    }

    #[test]
    fn qualidade_da_tela_vem_alta_e_volta_do_arquivo() {
        let c: Config = serde_json::from_str(r#"{"mic_muted":true}"#).unwrap();
        assert_eq!(c.share_quality, Quality::Alta);
        let c = Config {
            share_quality: Quality::Economica,
            ..Config::default()
        };
        let c: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(c.share_quality, Quality::Economica);
    }

    #[test]
    fn audio_da_tela_vem_ligado_e_volta_do_arquivo() {
        let c: Config = serde_json::from_str(r#"{"mic_muted":true}"#).unwrap();
        assert!(c.share_audio);
        let c = Config {
            share_audio: false,
            ..Config::default()
        };
        let c: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert!(!c.share_audio);
    }

    #[test]
    fn sensibilidade_vai_e_volta_do_arquivo() {
        let mut c = Config::default();
        c.set_auto(false);
        c.set_threshold(-55.0);
        let c: Config = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(c.audio_prefs().sensitivity, Sensitivity::Manual(-55.0));
    }

    #[test]
    fn automatico_guarda_o_ultimo_manual() {
        let mut c = Config::default();
        c.set_auto(false);
        assert_eq!(c.sensitivity, Sensitivity::Manual(MANUAL_DEFAULT_DB));
        c.set_threshold(-62.0);
        c.set_auto(true);
        c.set_threshold(-10.0);
        assert_eq!((c.sensitivity, c.manual_db), (Sensitivity::Auto, -62.0));
        c.set_auto(false);
        assert_eq!(c.sensitivity, Sensitivity::Manual(-62.0));
    }

    #[test]
    fn limiar_respeita_a_faixa() {
        let mut c = Config::default();
        c.set_auto(false);
        c.set_threshold(-500.0);
        assert_eq!(c.sensitivity, Sensitivity::Manual(MANUAL_MIN_DB));
        c.set_threshold(12.0);
        assert_eq!(c.sensitivity, Sensitivity::Manual(MANUAL_MAX_DB));
        c.set_threshold(f32::NAN);
        assert_eq!(c.sensitivity, Sensitivity::Manual(MANUAL_MIN_DB));
    }

    #[test]
    fn campo_novo_nao_derruba_config_antiga() {
        let c: Config = serde_json::from_str(r#"{"server":"https://a.b"}"#).unwrap();
        assert_eq!(c.server.as_deref(), Some("https://a.b"));
        assert_eq!(c.sidebar_width, SIDEBAR_PADRAO);
    }
}
