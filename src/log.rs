//! `concord.log` na pasta da config: uma linha por evento, com a hora em UTC. É a evidência
//! de quando algo deu errado e a pessoa não sabe dizer quando. Nunca senha, cookie nem token
//! aqui.
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Passou disso, o arquivo começa de novo.
const MAX_BYTES: u64 = 1 << 20;

/// Uma escrita por vez: linhas de threads diferentes não se misturam.
static WRITING: Mutex<()> = Mutex::new(());

/// Use pelo `log!`. Nos testes vai pro stderr, pra não sujar o arquivo de verdade.
pub fn line(args: fmt::Arguments) {
    let text = format!("{} {args}\n", stamp(SystemTime::now()));
    if cfg!(test) {
        eprint!("{text}");
    } else if let Some(path) = crate::config::log_path() {
        let _one = WRITING.lock();
        let _ = append(&path, &text);
    }
}

fn append(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = if fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        File::create(path)?
    } else {
        OpenOptions::new().create(true).append(true).open(path)?
    };
    file.write_all(text.as_bytes())
}

/// `2025-11-07 21:33:12.345Z`. A std não sabe o fuso, então UTC.
fn stamp(at: SystemTime) -> String {
    let t = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (days, secs) = ((t.as_secs() / 86_400) as i64, t.as_secs() % 86_400);
    // Dias desde 1970 pra data, pelo `civil_from_days` do Howard Hinnant.
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        t.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn hora_em_utc() {
        let at = |s: u64, ms: u64| UNIX_EPOCH + Duration::from_millis(s * 1000 + ms);
        assert_eq!(stamp(at(0, 0)), "1970-01-01 00:00:00.000Z");
        assert_eq!(stamp(at(1_762_551_192, 345)), "2025-11-07 21:33:12.345Z");
        assert_eq!(stamp(at(951_782_400, 7)), "2000-02-29 00:00:00.007Z");
    }

    #[test]
    fn passa_do_teto_e_comeca_de_novo() {
        let dir = std::env::temp_dir().join(format!("concord-log-{}", std::process::id()));
        let path = dir.join("sub").join("concord.log");
        append(&path, "a\n").unwrap();
        append(&path, "b\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\n");
        fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        append(&path, "c\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "c\n");
        fs::remove_dir_all(dir).unwrap();
    }
}
