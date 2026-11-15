//! Saúde do processo no log: a máquina na partida; a cada minuto a RAM, a memória de vídeo
//! e o tamanho do que pode crescer; e a thread que parou de responder. Sem isso, um
//! vazamento relatado de outro computador não tem como ser provado.
use std::{
    collections::BTreeMap,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// De quanto em quanto a RAM e os contadores vão pro log.
const SAMPLE: Duration = Duration::from_secs(60);
/// De quanto em quanto as threads vigiadas são conferidas.
const WATCH: Duration = Duration::from_secs(1);

/// Contadores do que pode crescer, pelo nome. Nome terminado em `_bytes` sai em MB.
static GAUGES: Mutex<BTreeMap<&'static str, i64>> = Mutex::new(BTreeMap::new());

pub fn set(name: &'static str, value: i64) {
    let mut g = GAUGES.lock().unwrap_or_else(|e| e.into_inner());
    g.insert(name, value);
}

pub fn add(name: &'static str, delta: i64) {
    let mut g = GAUGES.lock().unwrap_or_else(|e| e.into_inner());
    *g.entry(name).or_insert(0) += delta;
}

/// Conta um no contador enquanto vive: uma tarefa de vídeo, uma faixa de áudio.
pub struct Alive(&'static str);

impl Alive {
    pub fn new(name: &'static str) -> Self {
        add(name, 1);
        Alive(name)
    }
}

impl Drop for Alive {
    fn drop(&mut self) {
        add(self.0, -1);
    }
}

/// Threads vigiadas. Cada uma chama [`beat`] enquanto anda e [`idle`] quando para de
/// propósito.
#[derive(Clone, Copy)]
pub enum Beat {
    Audio,
    Ui,
}

impl Beat {
    const ALL: [Beat; 2] = [Beat::Audio, Beat::Ui];

    fn name(self) -> &'static str {
        match self {
            Beat::Audio => "thread de áudio",
            Beat::Ui => "interface",
        }
    }

    /// Mais que isso sem sinal conta como travada. A de áudio anda a cada 2 ms; a
    /// interface só pulsa no relógio de 4 s.
    fn limit(self) -> u64 {
        match self {
            Beat::Audio => 2_000,
            Beat::Ui => 10_000,
        }
    }
}

/// Último sinal de cada [`Beat`], em ms desde o começo; 0 é parada de propósito.
static BEATS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    // +1: o 0 quer dizer parada.
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

pub fn beat(b: Beat) {
    BEATS[b as usize].store(now_ms(), Ordering::Relaxed);
}

pub fn idle(b: Beat) {
    BEATS[b as usize].store(0, Ordering::Relaxed);
}

/// Grava a máquina no topo do log e sobe a thread que mede.
pub fn start() {
    context();
    let _ = thread::Builder::new()
        .name("concord-saude".into())
        .spawn(run);
}

fn run() {
    // Desde quando cada thread está travada: o último sinal antes de parar.
    let mut stuck: [Option<u64>; 2] = [None; 2];
    let mut next = Instant::now();
    loop {
        if Instant::now() >= next {
            next += SAMPLE;
            let counters = gauges();
            let sep = if counters.is_empty() { "" } else { " | " };
            log!("saúde: {}{sep}{counters}", memory());
        }
        for b in Beat::ALL {
            let last = BEATS[b as usize].load(Ordering::Relaxed);
            stuck[b as usize] = watch(b, last, now_ms(), stuck[b as usize]);
        }
        thread::sleep(WATCH);
    }
}

/// Avisa quando `b` passa do limite sem sinal e quando volta, com quanto tempo ficou
/// parada. Devolve o novo estado de travada.
fn watch(b: Beat, last: u64, now: u64, stuck: Option<u64>) -> Option<u64> {
    let late = now.saturating_sub(last);
    match stuck {
        None if last != 0 && late > b.limit() => {
            log!(
                "travou: {} sem responder há {:.1} s",
                b.name(),
                late as f64 / 1000.0
            );
            Some(last)
        }
        Some(since) if last != since => {
            if last == 0 {
                log!("travou: {} parou sem voltar a responder", b.name());
            } else {
                log!(
                    "voltou: {} depois de {:.1} s parada",
                    b.name(),
                    (last - since) as f64 / 1000.0
                );
            }
            None
        }
        _ => stuck,
    }
}

fn gauges() -> String {
    let g = GAUGES.lock().unwrap_or_else(|e| e.into_inner());
    let parts: Vec<String> = g
        .iter()
        .map(|(name, v)| match name.strip_suffix("_bytes") {
            Some(n) => format!("{n} {:.1} MB", *v as f64 / 1048576.0),
            None => format!("{name} {v}"),
        })
        .collect();
    parts.join(", ")
}

/// Versão, sistema, processador, RAM e fuso. Fica no topo mesmo se o arquivo recomeçar.
fn context() {
    let header = |args: std::fmt::Arguments| crate::log::header(args);
    header(format_args!(
        "Concord {} começou, PID {}",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
    header(format_args!("sistema: {}", system()));
    let (total, free) = ram_total();
    header(format_args!(
        "máquina: {}, {} threads, {} MB de RAM ({} MB livres)",
        cpu(),
        thread::available_parallelism().map_or(0, |n| n.get()),
        total >> 20,
        free >> 20
    ));
    header(format_args!(
        "fuso: UTC{}",
        chrono::Local::now().format("%:z")
    ));
}

// --- Linux -------------------------------------------------------------------

#[cfg(not(windows))]
fn proc_field(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap_or(0)
}

#[cfg(not(windows))]
fn memory() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |k| proc_field(&status, k) >> 10;
    let files = std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count());
    format!(
        "ram {} MB (anônima {} MB), gpu {}, threads {}, arquivos abertos {files}",
        kb("VmRSS:"),
        kb("RssAnon:"),
        gpu(),
        proc_field(&status, "Threads:")
    )
}

/// Memória de vídeo deste processo pelo `fdinfo` do DRM (amdgpu, i915, xe, nouveau). Cada
/// cliente aparece em todo fd aberto nele: conta uma vez por `drm-client-id`.
#[cfg(not(windows))]
fn gpu() -> String {
    let Ok(dir) = std::fs::read_dir("/proc/self/fdinfo") else {
        return "?".into();
    };
    let mut seen = std::collections::HashSet::new();
    let (mut vram, mut gtt) = (0u64, 0u64);
    for entry in dir.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)).map(str::trim);
        let Some(client) = get("drm-client-id:") else {
            continue;
        };
        if !seen.insert((get("drm-pdev:").map(String::from), client.to_owned())) {
            continue;
        }
        vram += drm_kib(get("drm-total-vram:").or(get("drm-memory-vram:")));
        gtt += drm_kib(get("drm-total-gtt:").or(get("drm-memory-gtt:")));
    }
    if seen.is_empty() {
        return "?".into();
    }
    format!("{} MB vram, {} MB gtt", vram >> 10, gtt >> 10)
}

/// `677520 KiB` em KiB. Sem unidade é byte.
#[cfg(not(windows))]
fn drm_kib(value: Option<&str>) -> u64 {
    let mut parts = value.unwrap_or("").split_whitespace();
    let n: u64 = parts.next().and_then(|n| n.parse().ok()).unwrap_or(0);
    match parts.next() {
        Some("KiB") => n,
        Some("MiB") => n << 10,
        Some("GiB") => n << 20,
        _ => n >> 10,
    }
}

#[cfg(not(windows))]
fn system() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let name = release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map_or("Linux", |n| n.trim_matches('"'));
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let env = |k| std::env::var(k).unwrap_or_else(|_| "?".into());
    format!(
        "{name}, kernel {}, sessão {} em {}",
        kernel.trim(),
        env("XDG_SESSION_TYPE"),
        env("XDG_CURRENT_DESKTOP")
    )
}

#[cfg(not(windows))]
fn cpu() -> String {
    let info = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    info.lines()
        .find_map(|l| l.strip_prefix("model name"))
        .map(|v| v.trim_start_matches([' ', '\t', ':']).to_owned())
        .unwrap_or_else(|| "?".into())
}

#[cfg(not(windows))]
fn ram_total() -> (u64, u64) {
    let info = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    (
        proc_field(&info, "MemTotal:") << 10,
        proc_field(&info, "MemAvailable:") << 10,
    )
}

// --- Windows -----------------------------------------------------------------

#[cfg(windows)]
fn memory() -> String {
    use windows::Win32::System::{
        ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::{GetCurrentProcess, GetProcessHandleCount},
    };
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let mut handles = 0u32;
    // SAFETY: o pseudo-handle do próprio processo não precisa fechar, e os ponteiros são de
    // variáveis locais do tamanho declarado.
    unsafe {
        let process = GetCurrentProcess();
        let _ = GetProcessMemoryInfo(
            process,
            (&raw mut c).cast::<PROCESS_MEMORY_COUNTERS>(),
            c.cb,
        );
        let _ = GetProcessHandleCount(process, &mut handles);
    }
    format!(
        "ram {} MB (privada {} MB), gpu {}, threads {}, handles {handles}",
        c.WorkingSetSize >> 20,
        c.PrivateUsage >> 20,
        gpu(),
        threads()
    )
}

/// Quantas threads este processo tem, pela foto do Toolhelp.
#[cfg(windows)]
fn threads() -> usize {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
    };
    let pid = std::process::id();
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut count = 0;
    // SAFETY: a foto é fechada no fim e a entrada tem o `dwSize` certo.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else {
            return 0;
        };
        let mut more = Thread32First(snapshot, &mut entry).is_ok();
        while more {
            if entry.th32OwnerProcessID == pid {
                count += 1;
            }
            more = Thread32Next(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    count
}

/// Memória de vídeo deste processo, somada em todas as placas: dedicada e compartilhada.
/// É onde um vazamento de textura aparece, fora da RAM do Gerenciador de Tarefas.
#[cfg(windows)]
fn gpu() -> String {
    use windows::{
        Win32::Graphics::Dxgi::{
            CreateDXGIFactory1, DXGI_MEMORY_SEGMENT_GROUP, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
            DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, IDXGIAdapter3,
            IDXGIFactory1,
        },
        core::Interface,
    };
    // SAFETY: chamadas COM comuns, com saídas em variáveis locais.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else {
            return "?".into();
        };
        let usage = |a: &IDXGIAdapter3, group: DXGI_MEMORY_SEGMENT_GROUP| {
            let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            a.QueryVideoMemoryInfo(0, group, &mut info)
                .map_or(0, |()| info.CurrentUsage)
        };
        let (mut local, mut shared) = (0u64, 0u64);
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            if let Ok(a) = adapter.cast::<IDXGIAdapter3>() {
                local += usage(&a, DXGI_MEMORY_SEGMENT_GROUP_LOCAL);
                shared += usage(&a, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL);
            }
        }
        format!(
            "{} MB dedicada, {} MB compartilhada",
            local >> 20,
            shared >> 20
        )
    }
}

/// Texto de `HKLM\<key>\<name>`.
#[cfg(windows)]
fn reg_text(key: &str, name: &str) -> Option<String> {
    use windows::{
        Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
        core::HSTRING,
    };
    let mut buf = [0u16; 256];
    let mut len = size_of_val(&buf) as u32;
    // SAFETY: o buffer e o tamanho em bytes batem.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(key),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let chars = (len as usize / 2).saturating_sub(1).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..chars]))
}

#[cfg(windows)]
fn reg_number(key: &str, name: &str) -> Option<u32> {
    use windows::{
        Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW},
        core::HSTRING,
    };
    let mut value = 0u32;
    let mut len = size_of::<u32>() as u32;
    // SAFETY: o valor e o tamanho em bytes batem.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(key),
            &HSTRING::from(name),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    Some(value)
}

/// O registro diz "Windows 10" também no 11: o build decide.
#[cfg(windows)]
fn system() -> String {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build = reg_text(KEY, "CurrentBuildNumber").unwrap_or_default();
    let name = reg_text(KEY, "ProductName").unwrap_or_else(|| "Windows".into());
    let name = if build.parse::<u32>().is_ok_and(|b| b >= 22_000) {
        name.replace("Windows 10", "Windows 11")
    } else {
        name
    };
    format!(
        "{name} {}, build {build}.{}",
        reg_text(KEY, "DisplayVersion").unwrap_or_default(),
        reg_number(KEY, "UBR").unwrap_or(0)
    )
}

#[cfg(windows)]
fn cpu() -> String {
    reg_text(
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
    .map_or_else(|| "?".into(), |n| n.trim().to_owned())
}

#[cfg(windows)]
fn ram_total() -> (u64, u64) {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: a estrutura tem o `dwLength` certo.
    let _ = unsafe { GlobalMemoryStatusEx(&mut status) };
    (status.ullTotalPhys, status.ullAvailPhys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travada_avisa_uma_vez_e_conta_a_volta() {
        let b = Beat::Audio;
        assert_eq!(watch(b, 1_000, 2_500, None), None, "dentro do limite");
        assert_eq!(watch(b, 0, 99_000, None), None, "parada de propósito");
        let stuck = watch(b, 1_000, 3_500, None);
        assert_eq!(stuck, Some(1_000));
        assert_eq!(watch(b, 1_000, 9_000, stuck), stuck, "não repete");
        assert_eq!(watch(b, 8_000, 8_100, stuck), None, "voltou depois de 7 s");
        assert_eq!(watch(b, 0, 9_000, stuck), None, "parou travada");
    }

    #[test]
    fn contador_vivo_sobe_e_desce() {
        let before = GAUGES.lock().unwrap().get("teste_vivo").copied();
        let a = Alive::new("teste_vivo");
        let b = Alive::new("teste_vivo");
        assert_eq!(
            GAUGES.lock().unwrap()["teste_vivo"],
            before.unwrap_or(0) + 2
        );
        drop((a, b));
        assert_eq!(GAUGES.lock().unwrap()["teste_vivo"], before.unwrap_or(0));
        add("teste_bytes", 3 << 20);
        assert!(gauges().contains("teste 3.0 MB"), "{}", gauges());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mede_este_processo() {
        let m = memory();
        let rss: u64 = m
            .strip_prefix("ram ")
            .and_then(|r| r.split(' ').next()?.parse().ok())
            .unwrap();
        assert!(rss > 1, "{m}");
        assert!(m.contains("threads ") && !m.contains("threads 0,"), "{m}");
        assert_eq!(drm_kib(Some("677520 KiB")), 677_520);
        assert_eq!(drm_kib(Some("2 MiB")), 2048);
        assert_eq!(drm_kib(None), 0);
        let (total, free) = ram_total();
        assert!(total > free && free > 0);
        assert!(!cpu().is_empty() && !system().is_empty());
    }
}
