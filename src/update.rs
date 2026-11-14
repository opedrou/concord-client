//! Atualização automática pelas releases do GitHub. Confere a última release; se for mais
//! nova, baixa o binário em segundo plano, confere o SHA-256 que o GitHub publica, abre ele
//! com `--version` pra ver se roda neste computador e troca o executável no lugar. Quem está
//! rodando segue no arquivo antigo, que vira `.old`: a versão nova vale da próxima vez.
//!
//! O `.old` fica guardado até a nova chegar na tela principal e ficar de pé ([`healthy`]).
//! Se ela cair antes disso [`TENTATIVAS`] vezes seguidas, a próxima abertura põe o `.old` de
//! volta, abre ele e marca a versão pra não baixar de novo ([`start`]).
//!
//! Tetos: versão que cai só depois de ficar de pé (ao entrar na call, por exemplo) não volta
//! sozinha. Versão que nem carrega (biblioteca faltando) não roda código nenhum, então só é
//! pega pelo teste antes da troca; a 0.1.8 e as anteriores trocam sem esse teste.
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
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

/// Pasta de dados do usuário: `$XDG_DATA_HOME`, senão `~/.local/share`.
#[cfg(target_os = "linux")]
fn data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

/// No Linux, põe o app no menu: `.desktop` apontando pra este executável, e o ícone. Só na
/// cópia instalada, e só reescreve o que mudou (o caminho muda se a pessoa mover o arquivo).
#[cfg(target_os = "linux")]
pub fn register() {
    let Some(exe) = installed() else { return };
    let Some(data) = data_dir() else { return };
    let icon = data.join("concord/icon.png");
    let entry = data.join(format!("applications/{APP_ID}.desktop"));
    let _ = write_if_changed(&icon, crate::icon::PNG);
    let _ = write_if_changed(&entry, desktop_entry(&exe, &icon).as_bytes());
}

/// Flags de linha de comando do Linux que não abrem janela: `--register` (usado pelo
/// `install.sh`) põe no menu e `--uninstall` tira tudo. Devolve `true` quando o processo deve sair.
#[cfg(target_os = "linux")]
pub fn cli() -> bool {
    let arg = std::env::args_os().nth(1);
    match arg.as_deref().and_then(|a| a.to_str()) {
        Some("--register") => register(),
        Some("--uninstall") => match (installed(), data_dir()) {
            (Some(exe), Some(data)) => {
                uninstall(&exe, &data);
                ask_wipe();
            }
            _ => println!(
                "Este não é o Concord instalado (build de desenvolvimento): nada removido."
            ),
        },
        _ => return false,
    }
    true
}

/// `concord-client --uninstall`: tira o `.desktop`, o ícone (e a pasta, se ficar vazia), o
/// binário e os `.old`/`.new` da atualização. A config e o login ficam, a não ser que a
/// pessoa responda que sim a [`ask_wipe`]. O binário rodando se apaga normal: o Linux só
/// solta o inode quando o processo sai.
#[cfg(target_os = "linux")]
fn uninstall(exe: &Path, data: &Path) {
    let pasta = data.join("concord");
    let alvos = [
        data.join(format!("applications/{APP_ID}.desktop")),
        pasta.join("icon.png"),
        exe.to_owned(),
        old_of(exe),
        exe.with_extension("new"),
    ];
    for alvo in alvos {
        if std::fs::remove_file(&alvo).is_ok() {
            println!("Removido: {}", alvo.display());
        }
    }
    // `remove_dir` só apaga pasta vazia; se a pessoa guardou algo lá, fica.
    if std::fs::remove_dir(&pasta).is_ok() {
        println!("Removido: {}", pasta.display());
    }
}

/// Pergunta no terminal se apaga também a config e o login. Sem terminal (script, pipe)
/// não pergunta e mantém.
#[cfg(target_os = "linux")]
fn ask_wipe() {
    use std::io::IsTerminal;
    let Some(dir) = config_dir().filter(|d| d.exists()) else {
        return;
    };
    let stdin = io::stdin();
    let wipe = wants_wipe(stdin.is_terminal(), &mut stdin.lock());
    if wipe_data(&dir, wipe, crate::session::delete_blocking) {
        println!("Removido: {} e o login salvo", dir.display());
    } else {
        println!(
            "Mantido: {} (config e log). Apague à mão se quiser.",
            dir.display()
        );
    }
}

/// Lê a resposta do `[s/N]`: só "s" ou "sim" apagam. Sem terminal nem chega a perguntar.
#[cfg(target_os = "linux")]
fn wants_wipe(terminal: bool, input: &mut impl io::BufRead) -> bool {
    use io::Write;
    if !terminal {
        return false;
    }
    print!("Apagar também as configurações e o login salvo do Concord? [s/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    input.read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_lowercase().as_str(), "s" | "sim")
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
        Launch::Copy => {
            let copied = win::copy(&exe, &target).is_ok();
            if copied {
                win::desktop_shortcut(&target);
            }
            copied && win::open(&target)
        }
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

/// Aberturas seguidas da versão nova que não ficaram de pé; na seguinte, volta pra anterior.
const TENTATIVAS: u32 = 2;
/// Quanto o teste da versão baixada espera o `--version` voltar. O antivírus do Windows
/// confere o exe novo antes de deixar abrir.
const ESPERA_TESTE: Duration = Duration::from_secs(20);

/// O que a atualização guarda entre aberturas, em `update.json` na pasta da config.
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct State {
    /// Aberturas da versão em teste que ainda não ficaram de pé nem fecharam direito.
    tries: u32,
    /// Versão que não abriu neste computador: não baixa de novo, espera a próxima.
    skip: Option<String>,
}

impl State {
    fn read(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(path, serde_json::to_vec(self)?)
    }
}

/// Zera as aberturas que falharam: esta ficou de pé ou fechou direito.
fn reset(state: &Path) -> io::Result<()> {
    let mut s = State::read(state);
    if s.tries == 0 {
        return Ok(());
    }
    s.tries = 0;
    s.write(state)
}

/// Executável e arquivo de estado. `None` em desenvolvimento, como [`installed`].
fn paths() -> Option<(PathBuf, PathBuf)> {
    Some((installed()?, crate::config::update_path()?))
}

/// Ao abrir, antes da janela. Com versão nova em teste, conta a abertura; se ela já caiu
/// demais, volta pra anterior e abre ela. `true`: abriu a anterior, este processo sai.
pub fn start() -> bool {
    let Some((exe, state)) = paths() else {
        return false;
    };
    match rollback(&exe, &state, env!("CARGO_PKG_VERSION")) {
        Ok(false) => false,
        Ok(true) => {
            log!("atualização: esta versão não ficou de pé, voltou pra anterior");
            restart()
        }
        Err(e) => {
            log!("atualização: não deu pra conferir a versão em teste: {e}");
            false
        }
    }
}

/// Com `.old` ao lado, o exe é versão em teste. Conta esta abertura; se as últimas
/// [`TENTATIVAS`] não ficaram de pé, ou se esta versão já falhou aqui, tira ela do caminho e
/// põe a anterior no lugar. `Ok(true)` quando voltou.
fn rollback(exe: &Path, state: &Path, version: &str) -> io::Result<bool> {
    // Sobra de troca: download pela metade ou a versão que falhou, tirada do caminho.
    let aside = exe.with_extension("new");
    let _ = fs::remove_file(&aside);
    let old = old_of(exe);
    if !old.exists() {
        return Ok(false);
    }
    let mut s = State::read(state);
    if s.tries < TENTATIVAS && s.skip.as_deref() != Some(version) {
        s.tries += 1;
        s.write(state)?;
        return Ok(false);
    }
    // Marca antes de trocar: sem a marca, a anterior baixaria esta de novo.
    State {
        tries: 0,
        skip: Some(version.to_owned()),
    }
    .write(state)?;
    // O Windows não apaga exe rodando, mas deixa renomear; o `.new` sai na próxima abertura.
    fs::rename(exe, &aside)?;
    if let Err(e) = fs::rename(&old, exe) {
        let _ = fs::rename(&aside, exe);
        return Err(e);
    }
    Ok(true)
}

/// Quem já trocou o exe nesta sessão não é a versão em teste: a de teste é a do disco.
fn candidate() -> Option<(PathBuf, PathBuf)> {
    if ready().is_some() { None } else { paths() }
}

/// A versão aberta chegou na tela principal e ficou de pé: a anterior não precisa mais
/// ficar guardada.
pub fn healthy() {
    if let Some((exe, state)) = candidate() {
        settle(&exe, &state);
    }
}

fn settle(exe: &Path, state: &Path) {
    let _ = reset(state);
    // No Windows falha se outra janela ainda roda a anterior: fica pra próxima abertura.
    if fs::remove_file(old_of(exe)).is_ok() {
        log!(
            "atualização: {} ficou de pé, a anterior saiu",
            env!("CARGO_PKG_VERSION")
        );
    }
}

/// Fechou pela janela, sem cair: esta abertura não conta como falha, mesmo sem chegar na
/// tela principal (sem rede, por exemplo).
pub fn closed() {
    if let Some((_, state)) = candidate() {
        let _ = reset(&state);
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
        if cfg!(debug_assertions) {
            // Só no build de debug: o teste do `--uninstall` aponta um exe de mentira.
            return std::env::var_os("CONCORD_TESTE_INSTALADO").map(PathBuf::from);
        }
        if std::env::var_os("CONCORD_NO_UPDATE").is_some() {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        let dev = exe.components().any(|c| c.as_os_str() == "target");
        (!dev).then_some(exe)
    })
    .clone()
}

/// Pasta da config (`~/.config/concord`, `%APPDATA%\Concord`): a mesma onde mora o
/// `update.json`, pra seguir o que `crate::config` decide.
fn config_dir() -> Option<PathBuf> {
    crate::config::update_path()?.parent().map(Path::to_owned)
}

/// Com `wipe`, apaga a sessão do cofre (da `server` do `config.json`, via `vault`) e a pasta
/// da config. Sem, não toca em nada. O cofre vai primeiro: se a pasta não apagar, a próxima
/// tentativa ainda sabe o servidor. Devolve se apagou.
fn wipe_data(dir: &Path, wipe: bool, vault: impl FnOnce(&str)) -> bool {
    if !wipe {
        return false;
    }
    let server = fs::read(dir.join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|c| c["server"].as_str().map(str::to_owned));
    if let Some(server) = server {
        vault(&server);
    }
    fs::remove_dir_all(dir).is_ok()
}

fn old_of(exe: &Path) -> PathBuf {
    exe.with_extension("old")
}

/// Resultado de uma conferência, pra linha do Sobre.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// Conferindo agora. Só a interface põe este.
    Searching,
    Latest,
    Downloading(String),
    /// Já no disco, vale ao reiniciar.
    Ready(String),
    /// Outra conferência já está rodando; o resultado vem por ela.
    Busy,
    Failed(String),
}

const SEM_REDE: &str = "Não deu pra falar com o GitHub. Tente de novo.";
const SEM_GRAVAR: &str = "Não deu pra gravar a versão nova.";

fn not_opened(version: &str) -> String {
    format!("A versão {version} não abriu neste computador. Fica esta até sair a próxima.")
}

/// Versão trocada nesta sessão, que já está no disco esperando reiniciar.
fn ready() -> Option<String> {
    INSTALLED.lock().ok()?.clone()
}

/// Confere e, se tiver versão nova, deixa ela instalada pra próxima abertura. Avisa
/// `downloading` com a versão quando começa a baixar.
pub async fn run(mut downloading: impl FnMut(String)) -> Check {
    // Roda ao abrir, de tempos em tempos e pelo botão; uma conferência por vez.
    static RUNNING: AtomicBool = AtomicBool::new(false);
    if RUNNING.swap(true, Ordering::Relaxed) {
        return Check::Busy;
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            RUNNING.store(false, Ordering::Relaxed);
        }
    }
    let _done = Done;
    check(&mut downloading).await.unwrap_or_else(Check::Failed)
}

async fn check(downloading: &mut impl FnMut(String)) -> Result<Check, String> {
    let (exe, state) = paths().ok_or("Esta cópia não se atualiza sozinha.")?;
    let client = reqwest::Client::builder()
        .user_agent("concord-client")
        .build()
        .map_err(|_| SEM_REDE)?;
    let latest = client
        .get(LATEST)
        .header("Accept", "application/vnd.github+json");
    let release: Release = send(latest).await?.json().await.map_err(|_| SEM_REDE)?;
    // Compara com a última já instalada nesta sessão, pra não baixar a mesma de novo.
    let pending = ready();
    if !is_newer(
        &release.tag_name,
        pending.as_deref().unwrap_or(env!("CARGO_PKG_VERSION")),
    ) {
        return Ok(pending.map_or(Check::Latest, Check::Ready));
    }
    let version = release.tag_name.trim_start_matches('v').to_owned();
    if State::read(&state).skip.as_deref() == Some(version.as_str()) {
        return Err(not_opened(&version));
    }
    let missing = "A versão nova não tem o arquivo deste sistema.";
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == ASSET)
        .ok_or(missing)?;
    let expected = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or(missing)?
        .to_owned();
    downloading(version.clone());
    let bytes = send(client.get(&asset.browser_download_url))
        .await?
        .bytes()
        .await
        .map_err(|_| SEM_REDE)?;
    if bytes.len() as u64 != asset.size || hex(&Sha256::digest(&bytes)) != expected {
        return Err("O download veio corrompido. Tente de novo.".into());
    }
    let v = version.clone();
    let opened = tokio::task::spawn_blocking(move || install(&exe, &bytes, &state, &v))
        .await
        .map_err(|_| SEM_GRAVAR)?
        .map_err(|e| {
            log!("atualização: não gravou a {version}: {e}");
            SEM_GRAVAR
        })?;
    if !opened {
        return Err(not_opened(&version));
    }
    if let Ok(mut installed) = INSTALLED.lock() {
        *installed = Some(version.clone());
    }
    log!("atualização: {version} instalada, vale ao reiniciar");
    Ok(Check::Ready(version))
}

/// Manda o pedido; erro de rede ou resposta que não é 2xx vira o texto da linha do Sobre.
async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
    let response = request.send().await.map_err(|_| SEM_REDE)?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "O GitHub respondeu com erro ({}). Tente de novo.",
            status.as_u16()
        ));
    }
    Ok(response)
}

/// Ver [`ready`].
static INSTALLED: Mutex<Option<String>> = Mutex::new(None);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Grava ao lado e abre com `--version`: só troca se o novo roda neste computador. Se não
/// rodar, marca a versão pra não baixar de novo e devolve `Ok(false)`, sem mexer no exe.
fn install(exe: &Path, bytes: &[u8], state: &Path, version: &str) -> io::Result<bool> {
    let new = exe.with_extension("new");
    fs::write(&new, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new, fs::Permissions::from_mode(0o755))?;
    }
    if !opens(&new) {
        log!("atualização: a {version} não abriu no teste e fica de fora");
        let _ = fs::remove_file(&new);
        let mut s = State::read(state);
        s.skip = Some(version.to_owned());
        s.write(state)?;
        return Ok(false);
    }
    swap(exe, state)?;
    Ok(true)
}

/// Roda o binário com `--version`, que sai antes de abrir janela. Biblioteca faltando ou
/// glibc velha derrubam ele antes do `main`, e aí sai com erro. Qualquer falha conta, até
/// não conseguir rodar: tentar de novo baixaria tudo outra vez a cada conferência.
fn opens(exe: &Path) -> bool {
    let child = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            log!("atualização: o teste não rodou: {e}");
            return false;
        }
    };
    let begin = Instant::now();
    while begin.elapsed() < ESPERA_TESTE {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

/// Põe o `.new` no lugar. Sem `.old`, o atual (o Windows deixa renomear quem está rodando)
/// vira ele; com `.old`, ele fica, porque é a última versão que abriu bem e o exe da vez
/// ainda está em teste. Se a última troca falhar, devolve o antigo.
fn swap(exe: &Path, state: &Path) -> io::Result<()> {
    let new = exe.with_extension("new");
    // A versão nova começa o teste do zero.
    reset(state)?;
    let old = old_of(exe);
    if old.exists() {
        return fs::rename(&new, exe);
    }
    fs::rename(exe, &old)?;
    if let Err(e) = fs::rename(&new, exe) {
        let _ = fs::rename(&old, exe);
        return Err(e);
    }
    Ok(())
}

/// Abre a versão nova e fecha esta. Só fora de call.
/// Abre a versão nova pelo caminho instalado. `false` se não abriu: aí esta segue aberta.
pub fn restart() -> bool {
    installed().is_some_and(|exe| Command::new(exe).spawn().is_ok())
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

    #[cfg(target_os = "linux")]
    #[test]
    fn desinstala_so_o_que_o_app_instalou() {
        let raiz = std::env::temp_dir().join(format!("concord-uninstall-{}", std::process::id()));
        let (data, bin) = (raiz.join("share"), raiz.join("bin"));
        let exe = bin.join("concord-client");
        std::fs::create_dir_all(data.join("applications")).unwrap();
        std::fs::create_dir_all(data.join("concord")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let outro = data.join("applications/outro.desktop");
        let binarios = [&exe, &old_of(&exe), &exe.with_extension("new")];
        let dados = [
            data.join("applications/concord.desktop"),
            data.join("concord/icon.png"),
            outro.clone(),
        ];
        for f in binarios
            .iter()
            .map(|p| p.as_path())
            .chain(dados.iter().map(|p| p.as_path()))
        {
            std::fs::write(f, b"x").unwrap();
        }
        uninstall(&exe, &data);
        assert!(binarios.iter().all(|f| !f.exists()));
        assert!(!dados[0].exists() && !dados[1].exists());
        assert!(!data.join("concord").exists(), "pasta vazia some");
        assert!(outro.exists(), "outros apps não são tocados");
        // Com algo de fora na pasta, ela fica.
        std::fs::create_dir_all(data.join("concord")).unwrap();
        std::fs::write(data.join("concord/meu.txt"), b"x").unwrap();
        uninstall(&exe, &data);
        assert!(data.join("concord/meu.txt").exists());
        std::fs::remove_dir_all(&raiz).unwrap();
    }

    #[test]
    fn apagar_dados_so_com_sim_e_limpa_o_cofre() {
        let dir = std::env::temp_dir().join(format!("concord-wipe-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), br#"{"server":"https://a.b"}"#).unwrap();
        let chamados = std::cell::RefCell::new(Vec::new());
        let cofre = |s: &str| chamados.borrow_mut().push(s.to_owned());
        assert!(!wipe_data(&dir, false, cofre));
        assert!(
            dir.exists() && chamados.borrow().is_empty(),
            "Não mantém tudo"
        );
        assert!(wipe_data(&dir, true, cofre));
        assert!(!dir.exists(), "Sim apaga a pasta");
        assert_eq!(*chamados.borrow(), ["https://a.b"]);
        // Sem config.json não há servidor: pasta some, cofre não é chamado.
        fs::create_dir_all(&dir).unwrap();
        assert!(wipe_data(&dir, true, |_| panic!("sem servidor")));
        assert!(!dir.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pergunta_do_terminal() {
        let ask = |terminal, texto: &str| wants_wipe(terminal, &mut texto.as_bytes());
        assert!(ask(true, "s\n") && ask(true, "Sim\n"));
        assert!(!ask(true, "n\n") && !ask(true, "\n") && !ask(true, ""));
        assert!(
            !ask(false, "s\n"),
            "sem terminal mantém, mesmo com 's' na entrada"
        );
    }

    /// Pasta temporária só deste teste, com o exe `velho` e o caminho do estado.
    fn pasta(nome: &str) -> (PathBuf, PathBuf, PathBuf) {
        let pasta =
            std::env::temp_dir().join(format!("concord-update-{}-{nome}", std::process::id()));
        let _ = fs::remove_dir_all(&pasta);
        fs::create_dir_all(&pasta).unwrap();
        let exe = pasta.join("concord-client.exe");
        fs::write(&exe, b"velho").unwrap();
        let estado = pasta.join("config/update.json");
        (pasta, exe, estado)
    }

    fn ler(path: &Path) -> Vec<u8> {
        fs::read(path).unwrap()
    }

    #[test]
    fn troca_o_executavel_e_guarda_o_antigo() {
        let (pasta, exe, estado) = pasta("troca");
        fs::write(exe.with_extension("new"), b"novo").unwrap();
        swap(&exe, &estado).unwrap();
        assert_eq!(ler(&exe), b"novo");
        assert_eq!(ler(&old_of(&exe)), b"velho");
        assert!(!exe.with_extension("new").exists());

        // Outra versão antes de reiniciar: a guardada segue sendo a que abriu bem.
        fs::write(exe.with_extension("new"), b"mais nova").unwrap();
        swap(&exe, &estado).unwrap();
        assert_eq!(ler(&exe), b"mais nova");
        assert_eq!(ler(&old_of(&exe)), b"velho", "não guarda a que nunca rodou");
        fs::remove_dir_all(&pasta).unwrap();
    }

    #[test]
    fn versao_que_nao_abre_nem_entra_e_fica_marcada() {
        let (pasta, exe, estado) = pasta("nao-abre");
        assert!(!install(&exe, b"lixo", &estado, "0.2.0").unwrap());
        assert_eq!(ler(&exe), b"velho");
        assert!(!old_of(&exe).exists());
        assert!(!exe.with_extension("new").exists());
        assert_eq!(State::read(&estado).skip.as_deref(), Some("0.2.0"));
        fs::remove_dir_all(&pasta).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn teste_da_versao_baixada_pelo_codigo_de_saida() {
        let (pasta, exe, estado) = pasta("abre");
        assert!(!install(&exe, b"#!/bin/sh\nexit 3\n", &estado, "0.2.0").unwrap());
        assert_eq!(ler(&exe), b"velho", "saiu com erro: fica a atual");
        let novo = b"#!/bin/sh\n[ \"$1\" = --version ]\n";
        assert!(install(&exe, novo, &estado, "0.2.1").unwrap());
        assert_eq!(ler(&exe), novo);
        assert_eq!(ler(&old_of(&exe)), b"velho");
        fs::remove_dir_all(&pasta).unwrap();
    }

    #[test]
    fn sem_versao_em_teste_abrir_nao_mexe_em_nada() {
        let (pasta, exe, estado) = pasta("normal");
        fs::write(exe.with_extension("new"), b"sobra").unwrap();
        for _ in 0..5 {
            assert!(!rollback(&exe, &estado, "0.2.0").unwrap());
        }
        assert_eq!(ler(&exe), b"velho");
        assert!(!estado.exists(), "nem cria o arquivo de estado");
        assert!(!exe.with_extension("new").exists(), "a sobra sai");
        fs::remove_dir_all(&pasta).unwrap();
    }

    #[test]
    fn versao_que_cai_duas_vezes_volta_pra_anterior_na_terceira() {
        let (pasta, exe, estado) = pasta("cai");
        fs::write(exe.with_extension("new"), b"novo").unwrap();
        swap(&exe, &estado).unwrap();
        // Duas aberturas que caem antes de ficar de pé: segue na nova.
        assert!(!rollback(&exe, &estado, "0.2.0").unwrap());
        assert!(!rollback(&exe, &estado, "0.2.0").unwrap());
        assert_eq!(ler(&exe), b"novo");
        assert_eq!(State::read(&estado).tries, 2);
        // A terceira devolve a anterior e marca a nova.
        assert!(rollback(&exe, &estado, "0.2.0").unwrap());
        assert_eq!(ler(&exe), b"velho");
        assert!(!old_of(&exe).exists());
        assert_eq!(
            ler(&exe.with_extension("new")),
            b"novo",
            "tirada do caminho"
        );
        let s = State::read(&estado);
        assert_eq!((s.tries, s.skip.as_deref()), (0, Some("0.2.0")));
        // A anterior abre normal e apaga a sobra.
        assert!(!rollback(&exe, &estado, "0.1.9").unwrap());
        assert!(!exe.with_extension("new").exists());
        fs::remove_dir_all(&pasta).unwrap();
    }

    #[test]
    fn versao_que_ja_falhou_aqui_volta_na_primeira() {
        // Instalada de novo por uma versão que não sabe pular (a 0.1.8).
        let (pasta, exe, estado) = pasta("ja-falhou");
        State {
            tries: 0,
            skip: Some("0.2.0".into()),
        }
        .write(&estado)
        .unwrap();
        fs::write(exe.with_extension("new"), b"novo").unwrap();
        swap(&exe, &estado).unwrap();
        assert!(rollback(&exe, &estado, "0.2.0").unwrap());
        assert_eq!(ler(&exe), b"velho");
        fs::remove_dir_all(&pasta).unwrap();
    }

    #[test]
    fn versao_de_pe_solta_a_anterior() {
        let (pasta, exe, estado) = pasta("de-pe");
        fs::write(exe.with_extension("new"), b"novo").unwrap();
        swap(&exe, &estado).unwrap();
        assert!(!rollback(&exe, &estado, "0.2.0").unwrap());
        settle(&exe, &estado);
        assert!(!old_of(&exe).exists());
        assert_eq!(State::read(&estado).tries, 0);
        for _ in 0..5 {
            assert!(
                !rollback(&exe, &estado, "0.2.0").unwrap(),
                "não tem mais volta"
            );
        }
        assert_eq!(ler(&exe), b"novo");
        fs::remove_dir_all(&pasta).unwrap();
    }

    #[test]
    fn fechar_direito_zera_as_falhas_mas_segue_em_teste() {
        // Sem rede não chega na tela principal: fechar a janela não conta como queda.
        let (pasta, exe, estado) = pasta("fechou");
        fs::write(exe.with_extension("new"), b"novo").unwrap();
        swap(&exe, &estado).unwrap();
        for _ in 0..5 {
            assert!(!rollback(&exe, &estado, "0.2.0").unwrap());
            reset(&estado).unwrap();
        }
        assert_eq!(ler(&exe), b"novo");
        assert!(old_of(&exe).exists(), "a anterior segue guardada");
        fs::remove_dir_all(&pasta).unwrap();
    }

    #[test]
    fn versao_nova_comeca_o_teste_do_zero() {
        let (pasta, exe, estado) = pasta("zero");
        fs::write(exe.with_extension("new"), b"novo").unwrap();
        swap(&exe, &estado).unwrap();
        assert!(!rollback(&exe, &estado, "0.2.0").unwrap());
        assert!(!rollback(&exe, &estado, "0.2.0").unwrap());
        fs::write(exe.with_extension("new"), b"mais nova").unwrap();
        swap(&exe, &estado).unwrap();
        assert_eq!(State::read(&estado).tries, 0);
        fs::remove_dir_all(&pasta).unwrap();
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
