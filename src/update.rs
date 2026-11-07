//! Atualização automática pelas releases do GitHub. Ao abrir, confere a última release;
//! se for mais nova, baixa o binário em segundo plano, confere o SHA-256 que o GitHub
//! publica e troca o executável no lugar. Quem está rodando segue no arquivo antigo, que
//! vira `.old` e é apagado na próxima abertura: a versão nova vale da próxima vez.
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[cfg(windows)]
mod win;

const LATEST: &str = "https://api.github.com/repos/opedrou/concord-client/releases/latest";

/// Nome do binário de cada sistema na release. O CI de release sobe com estes nomes.
#[cfg(windows)]
const ASSET: &str = "concord-client-windows-x86_64.exe";
#[cfg(not(windows))]
const ASSET: &str = "concord-client-linux-x86_64";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// `sha256:<hex>`. Release sem ele não é instalada.
    digest: Option<String>,
}

/// Versão `v1.2.3` ou `1.2.3` em números, pra comparar.
fn parse(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse().ok();
    Some((next()?, next()?, next()?))
}

fn is_newer(tag: &str, current: &str) -> bool {
    matches!((parse(tag), parse(current)), (Some(t), Some(c)) if t > c)
}

/// Id da janela no Wayland. O GNOME casa a janela com o `.desktop` de mesmo nome pra mostrar
/// o ícone; sem ele aparece a engrenagem genérica.
pub const APP_ID: &str = "concord";

/// No Linux, põe o app no menu: `.desktop` apontando pra este executável, e o ícone. Só na
/// cópia instalada, e só reescreve o que mudou (o caminho muda se a pessoa mover o arquivo).
#[cfg(target_os = "linux")]
pub fn register() {
    let Some(exe) = installed() else { return };
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let Some(data) = data else { return };
    let icon = data.join("concord/icon.png");
    let entry = data.join(format!("applications/{APP_ID}.desktop"));
    let _ = write_if_changed(&icon, crate::icon::PNG);
    let _ = write_if_changed(&entry, desktop_entry(&exe, &icon).as_bytes());
}

#[cfg(target_os = "linux")]
fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)
}

/// `Exec` entre aspas, porque o caminho pode ter espaço. Escapes da especificação: dentro
/// das aspas `"`, `` ` `` e `$` levam `\\`, a barra vira quatro, e `%` vira `%%`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_entry(exe: &Path, icon: &Path) -> String {
    let mut quoted = String::new();
    for c in exe.to_string_lossy().chars() {
        match c {
            '"' | '`' | '$' => quoted.extend(['\\', '\\', c]),
            '\\' => quoted.push_str(r"\\\\"),
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    format!(
        "[Desktop Entry]\nType=Application\nName=Concord\nComment=Voz e transmissão de tela\n\
         Exec=\"{quoted}\"\nIcon={}\nTerminal=false\nCategories=Network;Chat;\n\
         StartupWMClass={APP_ID}\n",
        icon.display()
    )
}

/// No Windows, vira programa instalado ([`win`]). O exe baixado se copia pra pasta fixa se
/// ainda não tem cópia lá, abre a cópia e sai; se já tem, só abre ela, que se atualiza
/// sozinha. Devolve `true` quando este processo deve sair: abriu a instalada ou desinstalou.
#[cfg(windows)]
pub fn setup() -> bool {
    let Some(target) = win::exe() else {
        return false;
    };
    if std::env::args_os().any(|a| a == "--uninstall") {
        win::uninstall();
        return true;
    }
    let Some(exe) = installed() else {
        return false;
    };
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned());
    let relaunched = std::env::var_os(RELAUNCHED).is_some();
    match launch(&canon(&exe), &canon(&target), target.exists(), relaunched) {
        Launch::Installed => {
            win::register(&target);
            false
        }
        Launch::Copy => win::copy(&exe, &target).is_ok() && win::open(&target),
        Launch::Open => win::open(&target),
    }
}

/// Variável que a cópia de fora passa ao abrir a instalada.
#[cfg_attr(not(windows), allow(dead_code))]
const RELAUNCHED: &str = "CONCORD_INSTALLED";

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, PartialEq)]
enum Launch {
    /// É a cópia instalada: confere atalho e registro, e segue.
    Installed,
    /// Não tem cópia instalada: copia, abre ela e sai.
    Copy,
    /// Já tem cópia instalada: só abre ela e sai. Nunca sobrescreve, nem com versão mais
    /// nova, que ela baixa sozinha.
    Open,
}

/// O que o Windows faz ao abrir, pelo caminho deste exe e o da instalação. `relaunched`
/// conta como instalada: se a comparação errar, abre a cópia uma vez só, sem laço.
#[cfg_attr(not(windows), allow(dead_code))]
fn launch(exe: &Path, target: &Path, exists: bool, relaunched: bool) -> Launch {
    // O Windows ignora maiúscula, e o `canonicalize` põe `\\?\` na frente.
    let plain = |p: &Path| {
        let p = p.to_string_lossy();
        p.trim_start_matches(r"\\?\").to_lowercase()
    };
    if relaunched || plain(exe) == plain(target) {
        Launch::Installed
    } else if exists {
        Launch::Open
    } else {
        Launch::Copy
    }
}

/// Apaga o `.old` da troca anterior. Chamar ao abrir.
pub fn cleanup() {
    if let Some(exe) = installed() {
        let _ = std::fs::remove_file(old_of(&exe));
    }
}

/// Executável que se atualiza, ou `None` em desenvolvimento: build de teste e binário
/// rodando de dentro de `target/` não são trocados pela release.
///
/// Lido uma vez, ao abrir: depois da troca, no Linux o `current_exe` passa a apontar pro
/// `.old` (o arquivo renomeado), e reiniciar por ele abria a versão velha.
fn installed() -> Option<PathBuf> {
    static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXE.get_or_init(|| {
        if cfg!(debug_assertions) || std::env::var_os("CONCORD_NO_UPDATE").is_some() {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        let dev = exe.components().any(|c| c.as_os_str() == "target");
        (!dev).then_some(exe)
    })
    .clone()
}

fn old_of(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Confere e, se tiver versão nova, deixa ela instalada pra próxima abertura. Devolve a
/// versão instalada. Qualquer falha é silenciosa: tenta de novo na próxima vez.
pub async fn run() -> Option<String> {
    // Roda ao abrir e de tempos em tempos; uma conferência por vez.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::Relaxed) {
        return None;
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            RUNNING.store(false, Ordering::Relaxed);
        }
    }
    let _done = Done;
    let exe = installed()?;
    let client = reqwest::Client::builder()
        .user_agent("concord-client")
        .build()
        .ok()?;
    let release: Release = client
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    // Compara com a última já instalada nesta sessão, pra não baixar a mesma de novo.
    let current = INSTALLED.lock().ok()?.clone();
    if !is_newer(
        &release.tag_name,
        current.as_deref().unwrap_or(env!("CARGO_PKG_VERSION")),
    ) {
        return None;
    }
    let asset = release.assets.into_iter().find(|a| a.name == ASSET)?;
    let expected = asset.digest.as_deref()?.strip_prefix("sha256:")?.to_owned();
    let bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .bytes()
        .await
        .ok()?;
    if bytes.len() as u64 != asset.size || hex(&Sha256::digest(&bytes)) != expected {
        return None;
    }
    let version = release.tag_name.trim_start_matches('v').to_owned();
    tokio::task::spawn_blocking(move || install(&exe, &bytes))
        .await
        .ok()?
        .ok()?;
    *INSTALLED.lock().ok()? = Some(version.clone());
    Some(version)
}

/// Versão trocada nesta sessão, que já está no disco esperando reiniciar.
static INSTALLED: Mutex<Option<String>> = Mutex::new(None);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Grava ao lado, tira o atual do caminho (o Windows deixa renomear quem está rodando) e
/// põe o novo no lugar. Se a última troca falhar, devolve o antigo.
fn install(exe: &Path, bytes: &[u8]) -> io::Result<()> {
    let new = exe.with_extension("new");
    std::fs::write(&new, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o755))?;
    }
    let old = old_of(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(e);
    }
    Ok(())
}

/// Abre a versão nova e fecha esta. Só fora de call.
/// Abre a versão nova pelo caminho instalado. `false` se não abriu: aí esta segue aberta.
pub fn restart() -> bool {
    installed().is_some_and(|exe| std::process::Command::new(exe).spawn().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compara_versoes_pelos_numeros() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.10", "0.1.9"), "10 é maior que 9, não menor");
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0", "0.1.0"), "mesma versão não baixa");
        assert!(!is_newer("v0.0.9", "0.1.0"), "mais velha não baixa");
        assert!(!is_newer("nightly", "0.1.0"), "tag estranha não baixa");
    }

    #[test]
    fn troca_o_executavel_e_guarda_o_antigo() {
        let pasta = std::env::temp_dir().join(format!("concord-update-{}", std::process::id()));
        std::fs::create_dir_all(&pasta).unwrap();
        let exe = pasta.join("concord-client.exe");
        std::fs::write(&exe, b"velho").unwrap();
        install(&exe, b"novo").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"novo");
        assert_eq!(std::fs::read(old_of(&exe)).unwrap(), b"velho");
        assert!(!exe.with_extension("new").exists());
        std::fs::remove_dir_all(&pasta).unwrap();
    }

    #[test]
    fn entrada_do_menu_aguenta_caminho_estranho() {
        let e = desktop_entry(
            Path::new("/home/a b/50% $x/concord-client"),
            Path::new("/d/concord/icon.png"),
        );
        assert!(
            e.contains(r#"Exec="/home/a b/50%% \\$x/concord-client""#),
            "{e}"
        );
        assert!(e.contains("Icon=/d/concord/icon.png\n"));
        assert!(e.contains("StartupWMClass=concord\n"));
    }

    #[test]
    fn windows_copia_abre_ou_segue_conforme_o_caminho() {
        let alvo = Path::new(r"C:\Users\Ana\AppData\Local\Programs\Concord\Concord.exe");
        let baixado = Path::new(r"C:\Users\Ana\Downloads\concord-client-windows-x86_64.exe");
        assert_eq!(launch(baixado, alvo, false, false), Launch::Copy);
        assert_eq!(
            launch(baixado, alvo, true, false),
            Launch::Open,
            "nunca sobrescreve"
        );
        assert_eq!(launch(alvo, alvo, true, false), Launch::Installed);
        let canonico = Path::new(r"\\?\c:\users\ana\appdata\local\programs\concord\CONCORD.EXE");
        assert_eq!(launch(canonico, alvo, true, false), Launch::Installed);
        assert_eq!(
            launch(baixado, alvo, true, true),
            Launch::Installed,
            "aberto pela cópia de fora não abre de novo"
        );
    }

    #[test]
    fn sha256_em_hex_minusculo() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
