//! Uma instância só por usuário. A primeira (a primária) escuta numa porta qualquer de
//! `127.0.0.1` e grava o número em `instance.port`, na pasta da config. Quem abre depois
//! conecta nela, confere o cumprimento [`HELLO`], passa o pedido ([`Command`]) e sai.
//!
//! Porta velha no arquivo (o app caiu, ou outro programa pegou a porta depois) não responde
//! o cumprimento: quem abriu vira a primária.
// ponytail: duas aberturas no mesmo instante viram duas primárias, e a última a gravar
// fica com o arquivo. Trava de arquivo se isso aparecer em log.
use std::{
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use iced::futures::Stream;

/// Primeira linha que a primária manda: o que separa o Concord de outro programa na porta.
const HELLO: &str = "concord";
/// Teto de cada passo da conversa. É tudo local: passou disso, quem responde não é o Concord.
const ESPERA: Duration = Duration::from_secs(1);
/// Quanto o `--uninstall` espera a instância aberta sair antes de apagar o programa.
const ESPERA_SAIR: Duration = Duration::from_secs(5);

/// Pedido de quem abriu depois pra instância aberta.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Abertura normal: mostra a janela, ou abre se não tem.
    Show,
    /// Desinstalar: sai da call e fecha.
    Quit,
}

impl Command {
    fn word(self) -> &'static str {
        match self {
            Command::Show => "show",
            Command::Quit => "quit",
        }
    }
}

/// Porta da primária, guardada por [`bind`] até a interface pedir [`commands`].
static LISTENER: Mutex<Option<TcpListener>> = Mutex::new(None);
/// Número dela, pra [`release`] só apagar o arquivo se ainda for desta instância.
static PORT: OnceLock<u16> = OnceLock::new();

/// Se outra instância responde, passa `cmd` (nada, com `None`) e devolve `true`: este
/// processo sai. Roda antes do primeiro `log!`, pra não gastar um arquivo de log.
pub fn hand_off(cmd: Option<Command>) -> bool {
    crate::config::instance_path().is_some_and(|file| tell(&file, cmd))
}

/// Pro `--uninstall`: pede pra instância aberta sair e espera ela fechar, até
/// [`ESPERA_SAIR`]. No Windows, exe rodando não se apaga.
pub fn quit() {
    let Some(file) = crate::config::instance_path() else {
        return;
    };
    if !tell(&file, Some(Command::Quit)) {
        return;
    }
    let begin = Instant::now();
    while begin.elapsed() < ESPERA_SAIR && reach(&file).is_some() {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Vira a primária: abre a porta e grava o número. Sem porta, o app abre igual, só sem
/// receber pedidos.
pub fn bind() {
    let Some(file) = crate::config::instance_path() else {
        return;
    };
    match listen(&file) {
        Ok(listener) => {
            if let Ok(addr) = listener.local_addr() {
                let _ = PORT.set(addr.port());
            }
            if let Ok(mut slot) = LISTENER.lock() {
                *slot = Some(listener);
            }
        }
        Err(e) => log!("instância: não abriu a porta: {e}"),
    }
}

/// Ao sair: a próxima abertura não precisa testar a porta. No Windows, conectar numa porta
/// fechada de `127.0.0.1` demora até o tempo esgotar. Se a versão nova de uma atualização já
/// gravou a dela, fica.
pub fn release() {
    let (Some(file), Some(port)) = (crate::config::instance_path(), PORT.get()) else {
        return;
    };
    if fs::read_to_string(&file).is_ok_and(|p| p.trim() == port.to_string()) {
        let _ = fs::remove_file(file);
    }
}

/// Antes de reiniciar pra atualizar: a versão nova não pode achar esta, que ainda está
/// fechando, e passar o pedido pra ela.
pub fn forget() {
    if let Some(file) = crate::config::instance_path() {
        let _ = fs::remove_file(file);
    }
}

/// Pedidos que chegam na primária, pra assinatura da interface. Uma thread aceita as
/// conexões: cada uma é curta, e quem não fala em [`ESPERA`] é largado.
pub fn commands() -> impl Stream<Item = Command> {
    iced::stream::channel(4, async |mut out| {
        let Some(listener) = LISTENER.lock().ok().and_then(|mut l| l.take()) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("concord-instancia".into())
            .spawn(move || {
                for conn in listener.incoming().flatten() {
                    if let Some(cmd) = answer(conn) {
                        log!("instância: pedido {}", cmd.word());
                        let _ = out.try_send(cmd);
                    }
                }
            });
    })
}

fn listen(file: &Path) -> io::Result<TcpListener> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(file, listener.local_addr()?.port().to_string())?;
    Ok(listener)
}

/// Lado da primária: cumprimenta e lê o pedido.
fn answer(conn: TcpStream) -> Option<Command> {
    conn.set_read_timeout(Some(ESPERA)).ok()?;
    conn.set_write_timeout(Some(ESPERA)).ok()?;
    (&conn).write_all(format!("{HELLO}\n").as_bytes()).ok()?;
    match line(&conn)?.as_str() {
        "show" => Some(Command::Show),
        "quit" => Some(Command::Quit),
        _ => None,
    }
}

/// Lado de quem abriu depois: conecta e passa o pedido. `false` se ninguém respondeu como
/// o Concord.
fn tell(file: &Path, cmd: Option<Command>) -> bool {
    let Some(mut conn) = reach(file) else {
        return false;
    };
    if let Some(cmd) = cmd {
        // Janela de quem está em segundo plano não vem pra frente sozinha no Windows: quem
        // a pessoa acabou de abrir deixa.
        #[cfg(windows)]
        // SAFETY: só libera o foco pra outro processo; sem ponteiro.
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
                windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
            )
        };
        let _ = conn.write_all(format!("{}\n", cmd.word()).as_bytes());
    }
    true
}

/// Conecta na porta do arquivo e confere o cumprimento.
fn reach(file: &Path) -> Option<TcpStream> {
    let port: u16 = fs::read_to_string(file).ok()?.trim().parse().ok()?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let conn = TcpStream::connect_timeout(&addr, ESPERA).ok()?;
    conn.set_read_timeout(Some(ESPERA)).ok()?;
    conn.set_write_timeout(Some(ESPERA)).ok()?;
    (line(&conn)? == HELLO).then_some(conn)
}

/// Uma linha curta, sem o `\n`. Outro programa na porta pode mandar qualquer coisa.
fn line(conn: &TcpStream) -> Option<String> {
    let mut text = String::new();
    BufReader::new(conn.take(64)).read_line(&mut text).ok()?;
    Some(text.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn arquivo(nome: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("concord-instancia-{}-{nome}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("instance.port")
    }

    #[test]
    fn segunda_abertura_passa_o_pedido_pra_primaria() {
        let file = arquivo("pedido");
        let listener = listen(&file).unwrap();
        for cmd in [Command::Show, Command::Quit] {
            let primaria = std::thread::spawn({
                let listener = listener.try_clone().unwrap();
                move || answer(listener.accept().unwrap().0)
            });
            assert!(tell(&file, Some(cmd)));
            assert_eq!(primaria.join().unwrap(), Some(cmd));
        }
        // Só conferir se está viva não vira pedido.
        let primaria = std::thread::spawn(move || answer(listener.accept().unwrap().0));
        assert!(tell(&file, None));
        assert_eq!(primaria.join().unwrap(), None);
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn porta_velha_de_outro_programa_vira_primaria() {
        let file = arquivo("velha");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        // Outro programa na porta: responde outra coisa, ou não diz nada.
        for resposta in [&b"HTTP/1.1 400 Bad Request\r\n\r\n"[..], b""] {
            let outro = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            fs::write(&file, outro.local_addr().unwrap().port().to_string()).unwrap();
            let servidor = std::thread::spawn(move || {
                let (mut conn, _) = outro.accept().unwrap();
                conn.write_all(resposta).unwrap();
                std::thread::sleep(ESPERA + Duration::from_millis(200));
            });
            assert!(!tell(&file, Some(Command::Show)));
            servidor.join().unwrap();
        }
        // Porta que ninguém escuta mais: o app caiu.
        let porta = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        fs::write(&file, porta.to_string()).unwrap();
        assert!(!tell(&file, Some(Command::Show)));
        // E a primária nova grava a porta dela por cima.
        let listener = listen(&file).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            listener.local_addr().unwrap().port().to_string()
        );
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn sem_arquivo_vira_primaria() {
        let file = arquivo("sem");
        assert!(!tell(&file, Some(Command::Show)));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "lixo").unwrap();
        assert!(!tell(&file, Some(Command::Show)), "arquivo ilegível também");
        let _ = fs::remove_dir_all(file.parent().unwrap());
    }
}
