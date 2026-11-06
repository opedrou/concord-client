//! Config e preferências em `~/.config/concord/` ou `%APPDATA%\Concord\`.
use std::{env, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SIDEBAR_MIN: f32 = 200.0;
pub const SIDEBAR_MAX: f32 = 480.0;
const SIDEBAR_PADRAO: f32 = 288.0;

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
    fn campo_novo_nao_derruba_config_antiga() {
        let c: Config = serde_json::from_str(r#"{"server":"https://a.b"}"#).unwrap();
        assert_eq!(c.server.as_deref(), Some("https://a.b"));
        assert_eq!(c.sidebar_width, SIDEBAR_PADRAO);
    }
}
