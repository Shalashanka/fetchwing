//! Archive extraction via the 7-Zip CLI and hash verification.
//! JD2 bundles sevenzipjbinding (native JNI, per-platform jars) and its own multi-volume logic
//! (97 classes). Fetchwing shells out to 7z (always current, handles rar5/7z/zip/split/tar.*) at
//! below-normal priority so games and video stay smooth.

use crate::model::HashAlgo;
use crate::util::http::re;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};

pub struct ArchiveSet {
    pub first: PathBuf,
    pub parts: Vec<PathBuf>,
    pub name: String,
}

fn fname(p: &Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Find archive sets among the finished files of a package.
pub fn find_archives(files: &[PathBuf]) -> Vec<ArchiveSet> {
    let mut sets = vec![];
    let mut used = std::collections::HashSet::new();
    for f in files {
        let b = fname(f);
        let bl = b.to_ascii_lowercase();
        if used.contains(f) {
            continue;
        }
        if re(r"(?i)\.part0*(?:[2-9]|\d{2,})\.rar$").is_match(&b) && !re(r"(?i)\.part0*1\.rar$").is_match(&b) {
            continue;
        }
        if re(r"\.\d{3}$").is_match(&b) && !bl.ends_with(".001") {
            continue;
        }
        if bl.ends_with(".iso") {
            continue;
        }
        let is_archive = re(r"(?i)\.part0*1\.rar$|\.001$|\.(?:rar|7z|zip|tar|tgz|tar\.gz|tar\.bz2|tar\.xz|tar\.zst)$").is_match(&b);
        if !is_archive {
            continue;
        }
        let (base, parts): (String, Vec<PathBuf>) = if let Some(m) = re(r"(?i)\.part0*1\.rar$").find(&b) {
            let base = b[..m.start()].to_string();
            let pre = format!("{}.part", base.to_ascii_lowercase());
            (base, files.iter().filter(|x| { let n = fname(x).to_ascii_lowercase(); n.starts_with(&pre) && n.ends_with(".rar") }).cloned().collect())
        } else if bl.ends_with(".001") {
            let base = b[..b.len() - 4].to_string();
            let pre = format!("{}.", base.to_ascii_lowercase());
            (base, files.iter().filter(|x| { let n = fname(x).to_ascii_lowercase(); n.starts_with(&pre) && re(r"\.\d{3}$").is_match(&n) }).cloned().collect())
        } else if bl.ends_with(".rar") {
            let base = b[..b.len() - 4].to_string();
            let r = re(r"(?i)\.r\d{2,3}$");
            let mut p = vec![f.clone()];
            p.extend(files.iter().filter(|x| { let n = fname(x); n.len() > base.len() && n[..base.len()].eq_ignore_ascii_case(&base) && r.is_match(&n[base.len()..]) }).cloned());
            (base, p)
        } else {
            (re(r"(?i)\.(?:tar\.\w+|\w+)$").replace(&b, "").into_owned(), vec![f.clone()])
        };
        for p in &parts {
            used.insert(p.clone());
        }
        sets.push(ArchiveSet { first: f.clone(), parts, name: base });
    }
    sets
}

pub struct ExtractResult {
    pub ok: bool,
    pub password: Option<String>,
    pub message: String,
}

pub async fn seven_zip_ok(bin: &str) -> bool {
    let mut c = tokio::process::Command::new(bin);
    c.arg("i").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    crate::plugins::ytdlp::hide(&mut c);
    c.status().await.map(|s| s.success()).unwrap_or(false)
}

pub async fn extract(bin: &str, archive: &Path, dest: &Path, passwords: &[String], low_priority: bool, progress: &(dyn Fn(u32) + Send + Sync)) -> ExtractResult {
    let mut tries: Vec<String> = vec![String::new()];
    for p in passwords {
        if !tries.contains(p) {
            tries.push(p.clone());
        }
    }
    let mut last = String::new();
    for pw in tries {
        let (ok, wrong_pw, msg) = run_7z(bin, archive, dest, &pw, low_priority, progress).await;
        if ok {
            return ExtractResult { ok: true, password: (!pw.is_empty()).then_some(pw), message: "Extracted".into() };
        }
        last = msg;
        if !wrong_pw {
            break;
        }
    }
    ExtractResult { ok: false, password: None, message: if last.is_empty() { "Extraction failed".into() } else { last } }
}

async fn run_7z(bin: &str, archive: &Path, dest: &Path, pw: &str, low: bool, progress: &(dyn Fn(u32) + Send + Sync)) -> (bool, bool, String) {
    let _ = std::fs::create_dir_all(dest);
    let mut c = tokio::process::Command::new(bin);
    c.arg("x").arg("-y").arg("-bsp1").arg("-bb1").arg(format!("-p{pw}")).arg(format!("-o{}", dest.display())).arg("-aou").arg(archive);
    c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
        c.creation_flags(0x0800_0000 | if low { 0x0000_4000 } else { 0 });
    }
    #[cfg(not(windows))]
    let _ = low;
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return (false, false, format!("7-Zip not runnable ({bin}): {e}")),
    };
    let mut out = String::new();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let mut l = BufReader::new(stderr).lines();
        while let Ok(Some(x)) = l.next_line().await {
            s.push_str(&x);
            s.push('\n');
        }
        s
    });
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    // 7z rewrites the progress line with \r and backspaces; split on both
    loop {
        buf.clear();
        match reader.read_until(b'\r', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let s = String::from_utf8_lossy(&buf).to_string();
                if let Some(c) = re(r"(\d{1,3})%").captures_iter(&s).last() {
                    progress(c[1].parse().unwrap_or(0));
                }
                out.push_str(&s);
                if out.len() > 16000 {
                    out = out[out.len() - 8000..].to_string();
                }
            }
        }
    }
    let status = child.wait().await;
    let all = format!("{out}\n{}", err_task.await.unwrap_or_default());
    let wrong = re(r"(?i)Wrong password|Can not open encrypted archive|Data Error in encrypted file").is_match(&all);
    if status.map(|s| s.success()).unwrap_or(false) {
        return (true, false, String::new());
    }
    let msg: Vec<&str> = all.lines().filter(|l| re(r"(?i)ERROR|Wrong password|Unexpected end|CRC Failed").is_match(l)).take(3).collect();
    (false, wrong, if msg.is_empty() { "7-Zip reported an error".into() } else { msg.join(" | ") })
}

pub fn hash_file(path: &Path, algo: HashAlgo) -> std::io::Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    macro_rules! digest {
        ($h:expr) => {{
            let mut h = $h;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            hex::encode(h.finalize())
        }};
    }
    Ok(match algo {
        HashAlgo::Md5 => digest!(md5::Md5::new()),
        HashAlgo::Sha1 => digest!(sha1::Sha1::new()),
        HashAlgo::Sha256 => digest!(sha2::Sha256::new()),
        HashAlgo::Crc32 => {
            let mut h = crc32fast::Hasher::new();
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
            }
            format!("{:08x}", h.finalize())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archives() {
        let f: Vec<PathBuf> = ["a.part1.rar", "a.part2.rar", "a.part10.rar", "b.7z.001", "b.7z.002", "c.rar", "c.r00", "c.r01", "d.zip", "e.txt", "f.iso"].iter().map(PathBuf::from).collect();
        let s = find_archives(&f);
        let names: Vec<&str> = s.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b.7z", "c", "d"]);
        assert_eq!(s[0].parts.len(), 3);
        assert_eq!(s[1].parts.len(), 2);
        assert_eq!(s[2].parts.len(), 3);
    }
}
