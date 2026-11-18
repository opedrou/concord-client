//! "Iniciar com o computador": abre a cópia instalada com [`BACKGROUND`], sem janela, pra
//! receber chamados. Linux: `concord.desktop` na pasta `autostart` da config do usuário.
//! Windows: valor `Concord` na chave `Run` do usuário.
use std::path::Path;

/// Abre sem janela. Vem do autostart e do reinício pra atualizar com a janela fechada.
pub const BACKGROUND: &str = "--background";

pub fn background() -> bool {
    std::env::args_os().any(|a| a == BACKGROUND)
}

/// Grava ou apaga conforme `on`: ao abrir, seguindo a config, e pelo interruptor. Só a
/// cópia instalada mexe: o build de desenvolvimento não pode apontar o autostart pra ele,
/// nem apagar o da instalada.
pub fn set(on: bool) {
    let Some(exe) = crate::update::launcher() else {
        return;
    };
    if !on {
        return remove();
    }
    if let Err(e) = write(&exe) {
        log!("autostart: não gravou: {e}");
    }
}

#[cfg(target_os = "linux")]
fn path() -> Option<std::path::PathBuf> {
    let config = crate::config::dir()?;
    let file = format!("autostart/{}.desktop", crate::update::APP_ID);
    Some(config.parent()?.join(file))
}

#[cfg(target_os = "linux")]
fn write(exe: &Path) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("sem pasta de config"))?;
    let icon = crate::update::icon_path().unwrap_or_default();
    crate::update::write_if_changed(&path, entry(exe, &icon).as_bytes())
}

#[cfg(target_os = "linux")]
fn entry(exe: &Path, icon: &Path) -> String {
    crate::update::desktop_entry(exe, icon, &format!(" {BACKGROUND}"))
}

/// Também pelo `--uninstall`, que apaga mesmo sem saber de qual cópia é.
#[cfg(target_os = "linux")]
pub fn remove() {
    if let Some(path) = path() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(windows)]
const RUN: windows::core::PCWSTR =
    windows::core::w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
#[cfg(windows)]
const NAME: windows::core::PCWSTR = windows::core::w!("Concord");

#[cfg(windows)]
fn write(exe: &Path) -> std::io::Result<()> {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    let value = format!("\"{}\" {BACKGROUND}", exe.display());
    let bytes: Vec<u8> = value
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: valor do Concord na chave do usuário; os bytes vivem até a chamada voltar.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN,
            NAME,
            REG_SZ.0,
            Some(bytes.as_ptr().cast()),
            bytes.len() as u32,
        )
    }
    .ok()?;
    Ok(())
}

/// Também pelo `--uninstall`, que apaga mesmo sem saber de qual cópia é.
#[cfg(windows)]
pub fn remove() {
    // SAFETY: só apaga o valor do Concord na chave do usuário.
    let _ = unsafe {
        windows::Win32::System::Registry::RegDeleteKeyValueW(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            RUN,
            NAME,
        )
    };
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn entrada_do_autostart_abre_em_segundo_plano() {
        let e = entry(
            Path::new("/home/a b/bin/concord-client"),
            Path::new("/d/concord/icon.png"),
        );
        assert!(e.starts_with("[Desktop Entry]\nType=Application\nName=Concord\n"));
        assert!(
            e.contains("\nExec=\"/home/a b/bin/concord-client\" --background\n"),
            "{e}"
        );
        assert!(e.contains("Icon=/d/concord/icon.png\n"));
    }
}
