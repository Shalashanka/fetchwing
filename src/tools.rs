//! External helper tools. Media extractors rot within weeks when sites change: during this
//! project the system yt-dlp (8 months old) failed with YouTube 403s while the latest release
//! worked. Fetchwing keeps its own copy current, downloaded with its own engine.

use crate::engine::{download, DirectRequest, DlJob, DlState};
use crate::util::http::{Browser, NetOpts};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub fn managed_ytdlp(data: &Path) -> PathBuf {
    data.join("tools").join(if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" })
}

fn asset() -> Option<&'static str> {
    if cfg!(windows) {
        Some("yt-dlp.exe")
    } else if cfg!(target_os = "macos") {
        Some("yt-dlp_macos")
    } else if cfg!(target_os = "linux") {
        Some(if cfg!(target_arch = "aarch64") { "yt-dlp_linux_aarch64" } else { "yt-dlp_linux" })
    } else {
        None
    }
}

pub async fn version(bin: &Path) -> Option<String> {
    let mut c = tokio::process::Command::new(bin);
    c.arg("--version");
    crate::plugins::ytdlp::hide(&mut c);
    let o = c.output().await.ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Returns (path, version, updated)
pub async fn ensure_ytdlp(data: &Path, last_check: i64, force: bool, ua: &str) -> Result<(PathBuf, String, bool), String> {
    let target = managed_ytdlp(data);
    let asset = asset().ok_or("unsupported platform")?;
    let local = if target.exists() { version(&target).await } else { None };
    if !force && local.is_some() && crate::model::now_ms() - last_check < 24 * 3_600_000 {
        return Ok((target, local.unwrap_or_default(), false));
    }
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let mut b = Browser::new(jar, "tools", NetOpts { user_agent: ua.into(), timeout_s: 30, proxy: String::new() });
    let rel = b.get_with("https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest", &crate::util::http::Req { headers: vec![("accept", "application/vnd.github+json".into())], ..Default::default() }).await.map_err(|e| e.to_string())?;
    let j = rel.json()?;
    let tag = j["tag_name"].as_str().unwrap_or_default().to_string();
    if local.as_deref() == Some(tag.as_str()) {
        return Ok((target, tag, false));
    }
    let a = j["assets"].as_array().into_iter().flatten().find(|a| a["name"] == asset).ok_or(format!("asset {asset} missing in {tag}"))?;
    let url = a["browser_download_url"].as_str().unwrap_or_default().to_string();
    let size = a["size"].as_i64().unwrap_or(-1);
    crate::log::info("tools", &format!("updating managed yt-dlp {} -> {tag}", local.as_deref().unwrap_or("(none)")));
    std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
    let part = target.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let state = Arc::new(Mutex::new(DlState::new(vec![], -1, None, None, false)));
    let job = DlJob {
        req: DirectRequest::get(url),
        browser: b,
        part_path: part.clone(),
        state: state.clone(),
        connections: 4,
        min_segment: 1 << 20,
        preallocate: false,
        limiters: vec![],
        cancel: CancellationToken::new(),
        on_progress: Arc::new(|_| {}),
        on_meta: Arc::new(|_| {}),
        persist: Arc::new(|_| {}),
    };
    let got = download(&job).await.map_err(|e| e.to_string())?;
    if size > 0 && got as i64 != size {
        return Err(format!("size mismatch {got} != {size}"));
    }
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&part, &target).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    let v = version(&target).await.unwrap_or(tag);
    crate::log::info("tools", &format!("managed yt-dlp ready: {v}"));
    Ok((target, v, true))
}
