pub mod http;
pub mod rate;

use std::path::{Path, PathBuf};

/// Make a filename safe on every OS (JD2: FilePathChecker / IfFilenameTooLongDialog).
pub fn sanitize_filename(name: &str) -> String {
    sanitize_len(name, 200)
}
pub fn sanitize_dir(name: &str) -> String {
    sanitize_len(name, 120)
}
fn sanitize_len(name: &str, max: usize) -> String {
    let mut n: String = name
        .chars()
        .map(|c| if c < ' ' || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    while n.ends_with('.') || n.ends_with(' ') {
        n.pop();
    }
    let mut n = n.trim().to_string();
    if n.is_empty() {
        n = "download".into();
    }
    let stem = n.split('.').next().unwrap_or("").to_ascii_lowercase();
    if ["con", "prn", "aux", "nul"].contains(&stem.as_str()) || ((stem.starts_with("com") || stem.starts_with("lpt")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit()) {
        n.insert(0, '_');
    }
    if n.len() > max {
        let ext = multi_ext(&n);
        let ext = if ext.len() < 20 { ext } else { String::new() };
        let mut base = n[..n.len() - ext.len()].to_string();
        while base.len() + ext.len() > max {
            base.pop();
        }
        n = base + &ext;
    }
    n
}

/// ".part1.rar", ".tar.gz", ".7z.001" stay together when renaming.
pub fn multi_ext(name: &str) -> String {
    let l = name.to_ascii_lowercase();
    let re = crate::util::http::re(r"(\.part\d+\.rar|\.tar\.(gz|bz2|xz|zst)|\.(7z|zip|rar)\.\d{3}|\.[^.]{1,10})$");
    re.find(&l).map(|m| name[m.start()..].to_string()).unwrap_or_default()
}

/// "file.zip" -> "file (2).zip" until free (also avoids in-progress .part files).
pub fn unique_path(p: &Path, taken: impl Fn(&Path) -> bool) -> PathBuf {
    let free = |c: &Path| !c.exists() && !PathBuf::from(format!("{}.part", c.display())).exists() && !taken(c);
    if free(p) {
        return p.to_path_buf();
    }
    let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = multi_ext(&name);
    let base = &name[..name.len() - ext.len()];
    for i in 2..10000 {
        let c = dir.join(format!("{base} ({i}){ext}"));
        if free(&c) {
            return c;
        }
    }
    p.to_path_buf()
}

pub fn fmt_bytes(n: i64) -> String {
    if n < 0 {
        return "?".into();
    }
    let u = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < u.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", n)
    } else if v < 10.0 {
        format!("{v:.2} {}", u[i])
    } else {
        format!("{v:.1} {}", u[i])
    }
}

pub fn fmt_duration(secs: i64) -> String {
    if secs < 0 {
        return "–".into();
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// "2MB", "1.5 GiB", "500k" -> bytes
pub fn parse_bytes(s: &str) -> Option<u64> {
    let caps = crate::util::http::re(r"(?i)^\s*([\d.,]+)\s*([kmgt]?)i?b?(?:ytes)?\s*$").captures(s)?;
    let v: f64 = caps[1].replace(',', ".").parse().ok()?;
    let mul = match caps[2].to_ascii_lowercase().as_str() {
        "k" => 1024f64,
        "m" => 1024f64.powi(2),
        "g" => 1024f64.powi(3),
        "t" => 1024f64.powi(4),
        _ => 1.0,
    };
    Some((v * mul).round() as u64)
}

pub fn free_space(dir: &Path) -> u64 {
    let mut d = dir.to_path_buf();
    while !d.exists() {
        match d.parent() {
            Some(p) => d = p.to_path_buf(),
            None => break,
        }
    }
    fs2::available_space(&d).unwrap_or(u64::MAX)
}

// ---- speed schedule ------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ScheduleRule {
    pub days: [bool; 7], // 0 = Sunday
    pub from: u32,
    pub to: u32,
    pub limit: Option<u64>, // None = pause
    pub raw: String,
}

/// "Mon-Fri 08:00-18:00 2MB" / "* 01:00-07:00 0" / "Sat,Sun 10:00-12:00 pause"
pub fn parse_rule(raw: &str) -> Result<ScheduleRule, String> {
    let re = http::re(r"^\s*(\S+)\s+(\d{1,2}):(\d{2})\s*-\s*(\d{1,2}):(\d{2})\s+(\S+)\s*$");
    let c = re.captures(raw).ok_or_else(|| format!("Bad schedule rule \"{raw}\" (expected e.g. \"Mon-Fri 08:00-18:00 2MB\")"))?;
    const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
    let mut days = [false; 7];
    if &c[1] == "*" {
        days = [true; 7];
    } else {
        for part in c[1].to_ascii_lowercase().split(',') {
            let mut it = part.split('-');
            let a = it.next().and_then(|d| DAYS.iter().position(|x| d.starts_with(x))).ok_or("bad day")?;
            match it.next() {
                None => days[a] = true,
                Some(b) => {
                    let b = DAYS.iter().position(|x| b.starts_with(x)).ok_or("bad day")?;
                    let mut i = a;
                    loop {
                        days[i] = true;
                        if i == b {
                            break;
                        }
                        i = (i + 1) % 7;
                    }
                }
            }
        }
    }
    let n = |i: usize| c[i].parse::<u32>().unwrap_or(0);
    let lim = c[6].to_ascii_lowercase();
    let limit = if lim == "pause" { None } else { Some(parse_bytes(&lim).ok_or("bad limit")?) };
    Ok(ScheduleRule { days, from: n(2) * 60 + n(3), to: n(4) * 60 + n(5), limit, raw: raw.to_string() })
}

pub fn active_rule(rules: &[ScheduleRule]) -> Option<&ScheduleRule> {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let mins = now.hour() * 60 + now.minute();
    let wd = now.weekday().num_days_from_sunday() as usize;
    rules.iter().find(|r| {
        r.days[wd] && if r.from <= r.to { mins >= r.from && mins < r.to } else { mins >= r.from || mins < r.to }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(sanitize_filename("a:b?.txt"), "a_b_.txt");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(multi_ext("x.part01.rar"), ".part01.rar");
        assert_eq!(parse_bytes("2MB"), Some(2 * 1024 * 1024));
        assert_eq!(parse_bytes("1.5 GiB"), Some(1610612736));
    }
    #[test]
    fn rules() {
        let r = parse_rule("Mon-Fri 08:00-18:00 2MB").unwrap();
        assert!(r.days[1] && r.days[5] && !r.days[0]);
        assert_eq!(r.limit, Some(2 * 1024 * 1024));
        assert!(parse_rule("Sat,Sun 10:00-12:00 pause").unwrap().limit.is_none());
    }
}
