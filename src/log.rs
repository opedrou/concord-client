//! Um arquivo por execução em `logs/` na pasta da config, com a data e a hora local de
//! quando abriu no nome. É a evidência de quando algo deu errado e a pessoa não sabe dizer
//! quando. Nunca senha, cookie, token nem texto do chat aqui.
use std::{
    borrow::Cow,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use chrono::Local;

/// Passou disso, o arquivo da execução recomeça, com o cabeçalho de novo no topo.
const MAX_BYTES: u64 = 3 << 20;
/// Arquivos guardados, contando o da execução atual. O mais antigo sai primeiro.
const KEEP: usize = 10;
/// Teto de linhas por minuto vindas das bibliotecas, pra um aviso em laço não lotar o
/// arquivo.
const CRATE_LINES: u32 = 60;

/// Arquivo desta execução. Só abre na primeira linha: execução que não loga nada (a
/// instalação que só reabre a cópia instalada) não ocupa vaga.
struct Run {
    file: File,
    written: u64,
    /// Linhas de [`header`], reescritas quando o arquivo recomeça.
    header: Vec<String>,
}

static RUN: Mutex<Option<Run>> = Mutex::new(None);

/// Use pelo `log!`. Nos testes vai pro stderr, pra não sujar o arquivo de verdade.
pub fn line(args: fmt::Arguments) {
    write(&format_line(args), false);
}

/// Como [`line`], e a linha volta pro topo se o arquivo recomeçar: a máquina e a versão.
pub fn header(args: fmt::Arguments) {
    write(&format_line(args), true);
}

fn format_line(args: fmt::Arguments) -> String {
    let thread = std::thread::current();
    let name = match thread.name() {
        Some(n) => Cow::Borrowed(n),
        None => Cow::Owned(format!("{:?}", thread.id())),
    };
    let text = args.to_string();
    format!(
        "{} [{name}] {}\n",
        Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        hresult(&text)
    )
}

fn write(text: &str, is_header: bool) {
    if cfg!(test) {
        eprint!("{text}");
        return;
    }
    let mut run = RUN.lock().unwrap_or_else(|e| e.into_inner());
    if run.is_none() {
        let Some(dir) = crate::config::log_dir() else {
            return;
        };
        *run = open(&dir).ok();
    }
    if let Some(run) = run.as_mut() {
        if is_header {
            run.header.push(text.to_owned());
        }
        let _ = run.append(text);
    }
}

impl Run {
    fn append(&mut self, text: &str) -> io::Result<()> {
        if self.written + text.len() as u64 > MAX_BYTES {
            // Aberto em append: depois de zerar, a escrita volta pro começo.
            self.file.set_len(0)?;
            self.written = 0;
            let restart = format!(
                "{} log passou de {} MB e recomeçou\n",
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                MAX_BYTES >> 20
            );
            for line in self.header.iter().chain([&restart]) {
                self.file.write_all(line.as_bytes())?;
                self.written += line.len() as u64;
            }
        }
        self.file.write_all(text.as_bytes())?;
        self.written += text.len() as u64;
        Ok(())
    }
}

/// Cria o arquivo desta execução e apaga os mais antigos, deixando [`KEEP`] com ele. O PID
/// no nome separa duas aberturas no mesmo segundo.
fn open(dir: &Path) -> io::Result<Run> {
    fs::create_dir_all(dir)?;
    prune(dir, KEEP - 1);
    let name = format!(
        "concord-{}-{}.log",
        Local::now().format("%Y-%m-%d_%H-%M-%S"),
        std::process::id()
    );
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))?;
    Ok(Run {
        file,
        written: 0,
        header: Vec::new(),
    })
}

/// Deixa só os `keep` logs mais novos. A data no começo do nome põe em ordem de abertura.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("concord-") && n.ends_with(".log"))
        })
        .collect();
    logs.sort();
    let extra = logs.len().saturating_sub(keep);
    for old in &logs[..extra] {
        let _ = fs::remove_file(old);
    }
}

/// O cpal escreve o HRESULT do Windows em decimal, com a mensagem que o sistema não achou:
/// `OS Error -2004287484 (FormatMessageW() returned error 317) (os error -2004287484)`.
/// Vira `0x88890004 AUDCLNT_E_DEVICE_INVALIDATED`, que dá pra procurar.
fn hresult(text: &str) -> Cow<'_, str> {
    const MARK: &str = "OS Error -";
    let Some(at) = text.find(MARK) else {
        return Cow::Borrowed(text);
    };
    let start = at + MARK.len() - 1;
    let digits = text[start + 1..]
        .find(|c: char| !c.is_ascii_digit())
        .map_or(text.len(), |n| start + 1 + n);
    let Ok(code) = text[start..digits].parse::<i32>() else {
        return Cow::Borrowed(text);
    };
    let mut end = digits;
    for tail in [" (FormatMessageW() returned error ", " (os error "] {
        if text[end..].starts_with(tail)
            && let Some(close) = text[end + tail.len()..].find(')')
        {
            end += tail.len() + close + 1;
        }
    }
    let hex = code as u32;
    let name = match hex {
        0x8889_0004 => " AUDCLNT_E_DEVICE_INVALIDATED",
        0x8889_0008 => " AUDCLNT_E_UNSUPPORTED_FORMAT",
        0x8889_000A => " AUDCLNT_E_DEVICE_IN_USE",
        0x8889_000F => " AUDCLNT_E_ENDPOINT_CREATE_FAILED",
        0x8889_0010 => " AUDCLNT_E_SERVICE_NOT_RUNNING",
        0x8889_0026 => " AUDCLNT_E_RESOURCES_INVALIDATED",
        0x8001_0106 => " RPC_E_CHANGED_MODE",
        0x8000_4005 => " E_FAIL",
        0x8007_0005 => " E_ACCESSDENIED",
        0x8007_000E => " E_OUTOFMEMORY",
        0x8007_0057 => " E_INVALIDARG",
        0x8007_0490 => " E_NOTFOUND",
        _ => "",
    };
    Cow::Owned(format!(
        "{}0x{hex:08X}{name}{}",
        &text[..at],
        hresult(&text[end..])
    ))
}

/// Avisos e erros das bibliotecas (livekit, wgpu, iced) no mesmo arquivo. Do iced vem
/// também a GPU e o backend que ele escolheu, que só sai em nível `info`.
struct Bridge {
    /// Começo do minuto atual, linhas nele e linhas descartadas.
    budget: Mutex<(Option<Instant>, u32, u32)>,
}

static BRIDGE: Bridge = Bridge {
    budget: Mutex::new((None, 0, 0)),
};

/// Liga o [`Bridge`]. Chamar uma vez, no começo.
pub fn capture_crates() {
    if ::log::set_logger(&BRIDGE).is_ok() {
        ::log::set_max_level(::log::LevelFilter::Info);
    }
}

fn wanted(level: ::log::Level, target: &str) -> bool {
    level <= ::log::Level::Warn
        || (level <= ::log::Level::Info && target.starts_with("iced_wgpu::window::compositor"))
}

impl Bridge {
    /// Dentro do teto de [`CRATE_LINES`] por minuto. Ao virar o minuto, avisa quantas
    /// ficaram de fora.
    fn spend(&self) -> bool {
        let mut b = self.budget.lock().unwrap_or_else(|e| e.into_inner());
        if b.0.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
            if b.2 > 0 {
                log!("biblioteca: {} linhas descartadas no último minuto", b.2);
            }
            *b = (Some(Instant::now()), 0, 0);
        }
        b.1 += 1;
        if b.1 > CRATE_LINES {
            b.2 += 1;
        }
        b.1 <= CRATE_LINES
    }
}

impl ::log::Log for Bridge {
    fn enabled(&self, metadata: &::log::Metadata) -> bool {
        wanted(metadata.level(), metadata.target())
    }

    fn log(&self, record: &::log::Record) {
        if !self.enabled(record.metadata()) || !self.spend() {
            return;
        }
        // O `{:#?}` do iced vem em várias linhas: uma só, pra caber no grep.
        let text = record.args().to_string();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        log!(
            "biblioteca {} {}: {text}",
            record.level().as_str().to_lowercase(),
            record.target()
        );
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("concord-log-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn only_file(dir: &Path) -> String {
        let mut names = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name());
        let name = names.next().unwrap().into_string().unwrap();
        assert!(names.next().is_none());
        name
    }

    #[test]
    fn hresult_vira_hex_com_nome() {
        let cpal = "áudio: erro no stream de saída: Failed to get current padding: OS Error \
                    -2004287484 (FormatMessageW() returned error 317) (os error -2004287484)";
        assert_eq!(
            hresult(cpal),
            "áudio: erro no stream de saída: Failed to get current padding: 0x88890004 \
             AUDCLNT_E_DEVICE_INVALIDATED"
        );
        assert_eq!(
            hresult("a OS Error -2147024891 b OS Error -1 c"),
            "a 0x80070005 E_ACCESSDENIED b 0xFFFFFFFF c"
        );
        assert_eq!(
            hresult("OS Error 5 (os error 5)"),
            "OS Error 5 (os error 5)"
        );
        assert!(matches!(hresult("nada"), Cow::Borrowed(_)));
    }

    #[test]
    fn guarda_so_os_mais_novos() {
        let dir = temp("prune");
        fs::create_dir_all(&dir).unwrap();
        for d in 1..=12 {
            fs::write(
                dir.join(format!("concord-2026-11-{d:02}_10-00-00-1.log")),
                "",
            )
            .unwrap();
        }
        fs::write(dir.join("outro.txt"), "").unwrap();
        prune(&dir, KEEP - 1);
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left.len(), KEEP - 1 + 1, "9 logs e o outro arquivo");
        assert_eq!(left[0], "concord-2026-11-04_10-00-00-1.log");
        assert_eq!(left.last().unwrap(), "outro.txt");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn abre_um_arquivo_por_execucao() {
        let dir = temp("open");
        open(&dir).unwrap();
        let name = only_file(&dir);
        assert!(
            name.starts_with("concord-20") && name.ends_with(".log"),
            "{name}"
        );
        let pid = format!("-{}.log", std::process::id());
        assert!(name.ends_with(&pid), "{name}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn passa_do_teto_e_recomeca_com_o_cabecalho() {
        let dir = temp("teto");
        let mut run = open(&dir).unwrap();
        run.header.push("versão 1\n".into());
        run.append("versão 1\n").unwrap();
        run.append("a\n").unwrap();
        run.written = MAX_BYTES;
        run.append("b\n").unwrap();
        let text = fs::read_to_string(dir.join(only_file(&dir))).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "versão 1");
        assert!(
            lines[1].ends_with("log passou de 3 MB e recomeçou"),
            "{text}"
        );
        assert_eq!(lines[2], "b");
        assert_eq!(run.written, text.len() as u64);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn biblioteca_so_avisos_e_a_gpu_do_iced() {
        use ::log::Level::*;
        assert!(wanted(Warn, "livekit::room"));
        assert!(wanted(Error, "wgpu_core"));
        assert!(!wanted(Info, "livekit::room"));
        assert!(wanted(Info, "iced_wgpu::window::compositor"));
        assert!(!wanted(Debug, "iced_wgpu::window::compositor"));
    }
}
