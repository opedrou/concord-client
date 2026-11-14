//! Instalação no Windows, toda no usuário e sem admin: o programa em
//! `%LOCALAPPDATA%\Programs\Concord`, atalho no Menu Iniciar (o Windows+S acha por ele) e na
//! Área de Trabalho (este só na instalação) e entrada em "Aplicativos instalados", que
//! desinstala com `--uninstall`.
use std::{
    env,
    ffi::{OsString, c_void},
    fs, io,
    os::windows::{ffi::OsStringExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};

use windows::{
    Win32::{
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize, IPersistFile, STGM_READ,
            },
            Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
                RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
            },
        },
        UI::{
            Shell::{
                FOLDERID_Desktop, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, SLGP_RAWPATH,
                ShellLink,
            },
            WindowsAndMessaging::{IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_YESNO, MessageBoxW},
        },
    },
    core::{HSTRING, Interface, PCWSTR, Result, w},
};

const UNINSTALL_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Concord");
/// Processo filho sem console: nada pisca na tela.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn folder(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `%LOCALAPPDATA%\Programs\Concord\Concord.exe`
pub fn exe() -> Option<PathBuf> {
    Some(folder("LOCALAPPDATA")?.join(r"Programs\Concord\Concord.exe"))
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Concord.lnk`
fn link() -> Option<PathBuf> {
    Some(folder("APPDATA")?.join(r"Microsoft\Windows\Start Menu\Programs\Concord.lnk"))
}

/// `Concord.lnk` na Área de Trabalho. Pergunta ao Windows onde ela fica: com o OneDrive
/// ligado ela mora em outra pasta, e `%USERPROFILE%\Desktop` não existe.
fn desktop_link() -> Option<PathBuf> {
    // SAFETY: o Windows devolve o texto alocado com `CoTaskMemAlloc`; copiamos e soltamos.
    unsafe {
        let text = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None).ok()?;
        let dir = PathBuf::from(OsString::from_wide(text.as_wide()));
        CoTaskMemFree(Some(text.as_ptr() as *const c_void));
        Some(dir.join("Concord.lnk"))
    }
}

/// Só na instalação (a primeira cópia): quem apagar o atalho da Área de Trabalho não o vê
/// voltar, ao contrário do Menu Iniciar, que [`register`] refaz a cada abertura.
pub fn desktop_shortcut(exe: &Path) {
    if let Some(link) = desktop_link() {
        let _ = shortcut(&link, exe);
    }
}

/// Copia por um `.new` ao lado: queda no meio não deixa um `Concord.exe` pela metade. Lê
/// e grava em vez de `fs::copy`, que levaria junto a marca de "baixado da internet" e o
/// aviso do SmartScreen a cada abertura pelo Menu Iniciar.
pub fn copy(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    let new = to.with_extension("new");
    fs::write(&new, fs::read(from)?)?;
    fs::rename(&new, to)
}

/// Abre a cópia instalada, avisando que veio daqui.
pub fn open(exe: &Path) -> bool {
    Command::new(exe)
        .env(super::RELAUNCHED, "1")
        .spawn()
        .is_ok()
}

/// A cada abertura da cópia instalada: refaz o atalho se sumiu e põe a versão atual em
/// "Aplicativos instalados", que muda depois de atualizar.
pub fn register(exe: &Path) {
    if let Some(link) = link() {
        let _ = shortcut(&link, exe);
    }
    let _ = uninstall_entry(exe);
}

/// `.lnk` pelo COM do Shell. Só grava se faltar ou apontar pra outro lugar: regravar toda
/// vez faria o Menu Iniciar listar o Concord como adicionado recentemente.
fn shortcut(link: &Path, exe: &Path) -> Result<()> {
    // SAFETY: COM aberto e fechado nesta thread, antes de a janela existir; as interfaces
    // de `write_shortcut` são soltas antes do `CoUninitialize`.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = write_shortcut(link, exe);
        if init.is_ok() {
            CoUninitialize();
        }
        result
    }
}

unsafe fn write_shortcut(link: &Path, exe: &Path) -> Result<()> {
    let (link, exe) = (HSTRING::from(link), HSTRING::from(exe));
    // SAFETY: chamadas COM numa thread com COM aberto; o buffer vai com o tamanho dele.
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = shell.cast()?;
        if file.Load(&link, STGM_READ).is_ok() {
            let mut path = [0u16; 1024];
            shell.GetPath(&mut path, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)?;
            let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
            let current = String::from_utf16_lossy(&path[..len]);
            if current.eq_ignore_ascii_case(&exe.to_string_lossy()) {
                return Ok(());
            }
        }
        shell.SetPath(&exe)?;
        shell.SetArguments(w!(""))?;
        shell.SetIconLocation(&exe, 0)?;
        shell.SetDescription(w!("Voz e transmissão de tela"))?;
        file.Save(&link, true)
    }
}

/// Chave de "Aplicativos instalados" do usuário: nome, ícone, versão e o desinstalar.
fn uninstall_entry(exe: &Path) -> Result<()> {
    let exe = exe.display().to_string();
    let dir = Path::new(&exe).parent().map(|d| d.display().to_string());
    let text = [
        (w!("DisplayName"), "Concord".to_owned()),
        (w!("DisplayIcon"), exe.clone()),
        (w!("DisplayVersion"), env!("CARGO_PKG_VERSION").to_owned()),
        (w!("Publisher"), "Concord".to_owned()),
        (w!("InstallLocation"), dir.unwrap_or_default()),
        (w!("UninstallString"), format!("\"{exe}\" --uninstall")),
    ];
    let mut key = HKEY::default();
    // SAFETY: chave do Concord no usuário, aberta e fechada aqui; os dados vivem até cada
    // chamada voltar.
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            UNINSTALL_KEY,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        for (name, value) in text {
            let bytes: Vec<u8> = value
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect();
            let _ = RegSetValueExW(key, name, None, REG_SZ, Some(&bytes));
        }
        for name in [w!("NoModify"), w!("NoRepair")] {
            let one = 1u32.to_le_bytes();
            let _ = RegSetValueExW(key, name, None, REG_DWORD, Some(one.as_slice()));
        }
        let _ = RegCloseKey(key);
    }
    Ok(())
}

/// Sim/Não, com Não de padrão: apagar o login e a config não se desfaz.
fn ask_wipe() -> bool {
    // SAFETY: textos literais com terminador e sem janela dona; a caixa fecha antes de voltar.
    unsafe {
        MessageBoxW(
            None,
            w!("Apagar também as configurações e o login salvo do Concord?"),
            w!("Desinstalar o Concord"),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) == IDYES
    }
}

/// `Concord.exe --uninstall`, que "Aplicativos instalados" chama. Tira atalhos, chave e o
/// programa. Pergunta antes se apaga também a config em `%APPDATA%\Concord` e o login no
/// cofre do Windows; sem "Sim" eles ficam.
pub fn uninstall() {
    let Some(exe) = exe() else { return };
    let wipe = ask_wipe();
    for link in [link(), desktop_link()].into_iter().flatten() {
        let _ = fs::remove_file(link);
    }
    if let Some(dir) = super::config_dir() {
        super::wipe_data(&dir, wipe, crate::session::delete_blocking);
    }
    // SAFETY: só apaga a chave do Concord no usuário.
    let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, UNINSTALL_KEY) };
    // Exe rodando não se apaga: um cmd sem janela espera uns 2 s este processo fechar e
    // apaga o exe, o `.old` e o `.new` da atualização e a pasta, se ficou vazia.
    let Some(dir) = exe.parent() else { return };
    let script = format!(
        r#"/d /c ping -n 3 127.0.0.1 >nul & del /f /q "{}" "{}" "{}" & rmdir "{}""#,
        exe.display(),
        super::old_of(&exe).display(),
        exe.with_extension("new").display(),
        dir.display()
    );
    let _ = Command::new("cmd.exe")
        .raw_arg(script)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}
