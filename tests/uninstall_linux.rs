//! `concord-client --uninstall` de ponta a ponta, com HOME e XDG_* temporários. A entrada
//! passa por um pty (`script`), porque o programa só pergunta quando a entrada é terminal.
//! A config de teste não tem `server`, então o cofre do sistema nunca é tocado.
#![cfg(target_os = "linux")]
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const BIN: &str = env!("CARGO_BIN_EXE_concord-client");

/// Monta uma instalação falsa; devolve a raiz, o exe falso e a pasta da config.
fn instalacao(nome: &str) -> (PathBuf, PathBuf, PathBuf) {
    let raiz = std::env::temp_dir().join(format!("concord-it-{}-{nome}", std::process::id()));
    let _ = fs::remove_dir_all(&raiz);
    let (exe, config) = (raiz.join("bin/concord-client"), raiz.join("config/concord"));
    for dir in [
        raiz.join("bin"),
        config.clone(),
        raiz.join("share/applications"),
    ] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(&exe, b"x").unwrap();
    fs::write(config.join("config.json"), b"{}").unwrap();
    fs::write(raiz.join("share/applications/concord.desktop"), b"x").unwrap();
    (raiz, exe, config)
}

fn ambiente(c: &mut Command, raiz: &Path, exe: &Path) {
    c.env("HOME", raiz)
        .env("XDG_CONFIG_HOME", raiz.join("config"))
        .env("XDG_DATA_HOME", raiz.join("share"))
        .env("CONCORD_TESTE_INSTALADO", exe);
}

/// Roda com a entrada num terminal de mentira e devolve o que foi escrito.
fn com_terminal(raiz: &Path, exe: &Path, resposta: &str) -> String {
    let mut c = Command::new("script");
    c.args(["-qec", "\"$BIN\" --uninstall", "/dev/null"])
        .env("BIN", BIN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    ambiente(&mut c, raiz, exe);
    let mut filho = c.spawn().expect("precisa do `script` (util-linux)");
    let mut entrada = filho.stdin.take().unwrap();
    entrada.write_all(resposta.as_bytes()).unwrap();
    let saida = filho.wait_with_output().unwrap();
    drop(entrada);
    String::from_utf8_lossy(&saida.stdout).into_owned()
}

#[test]
fn sem_terminal_mantem_a_config_sem_perguntar() {
    let (raiz, exe, config) = instalacao("pipe");
    let mut c = Command::new(BIN);
    c.arg("--uninstall").stdin(Stdio::null());
    ambiente(&mut c, &raiz, &exe);
    let saida = c.output().unwrap();
    let texto = String::from_utf8_lossy(&saida.stdout);
    assert!(!texto.contains("[s/N]"), "{texto}");
    assert!(!exe.exists(), "o app sai");
    assert!(config.join("config.json").exists());
    fs::remove_dir_all(&raiz).unwrap();
}

#[test]
fn no_terminal_n_mantem_e_s_apaga() {
    let (raiz, exe, config) = instalacao("n");
    let texto = com_terminal(&raiz, &exe, "n\n");
    assert!(texto.contains("[s/N]"), "{texto}");
    assert!(
        !exe.exists() && config.join("config.json").exists(),
        "{texto}"
    );
    fs::remove_dir_all(&raiz).unwrap();

    let (raiz, exe, config) = instalacao("s");
    let texto = com_terminal(&raiz, &exe, "s\n");
    assert!(texto.contains("[s/N]"), "{texto}");
    assert!(!exe.exists() && !config.exists(), "{texto}");
    fs::remove_dir_all(&raiz).unwrap();
}
