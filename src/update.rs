//! Atualização automática pelas releases do GitHub. Ao abrir, confere a última release;
//! se for mais nova, baixa o binário em segundo plano, confere o SHA-256 que o GitHub
//! publica e troca o executável no lugar. Quem está rodando segue no arquivo antigo, que
//! vira `.old` e é apagado na próxima abertura: a versão nova vale da próxima vez.
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

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

/// Apaga o `.old` da troca anterior. Chamar ao abrir.
pub fn cleanup() {
    if let Some(exe) = installed() {
        let _ = std::fs::remove_file(old_of(&exe));
    }
}

/// Executável que se atualiza, ou `None` em desenvolvimento: build de teste e binário
/// rodando de dentro de `target/` não são trocados pela release.
fn installed() -> Option<PathBuf> {
    if cfg!(debug_assertions) || std::env::var_os("CONCORD_NO_UPDATE").is_some() {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let dev = exe.components().any(|c| c.as_os_str() == "target");
    (!dev).then_some(exe)
}

fn old_of(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Confere e, se tiver versão nova, deixa ela instalada pra próxima abertura. Devolve a
/// versão instalada. Qualquer falha é silenciosa: tenta de novo na próxima vez.
pub async fn run() -> Option<String> {
    // Uma vez por processo: logar de novo não baixa outra vez por cima da troca feita.
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::Relaxed) {
        return None;
    }
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
    if !is_newer(&release.tag_name, env!("CARGO_PKG_VERSION")) {
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
    Some(version)
}

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
pub fn restart() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
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
    fn sha256_em_hex_minusculo() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
