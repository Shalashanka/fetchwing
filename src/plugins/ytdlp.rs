//! Media sites via yt-dlp. JD2 ships its own YouTube implementation (YoutubeHelper 4.5k lines +
//! a 19k-line variants enum) and per-site media plugins that lag behind yt-dlp. Fetchwing delegates
//! check, playlist crawling and the download to yt-dlp (managed & auto-updated, see tools.rs).

use super::*;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

pub const MEDIA: &[&str] = &[
    "youtube.com", "youtu.be", "music.youtube.com", "vimeo.com", "dailymotion.com", "twitch.tv", "twitter.com", "x.com", "tiktok.com",
    "instagram.com", "facebook.com", "fb.watch", "reddit.com", "v.redd.it", "soundcloud.com", "bandcamp.com", "bilibili.com", "nicovideo.jp",
    "rumble.com", "odysee.com", "streamable.com", "bitchute.com", "kick.com", "mixcloud.com", "vk.com", "ok.ru", "rutube.ru", "arte.tv",
    "ardmediathek.de", "zdf.de", "bbc.co.uk", "cnn.com", "ted.com", "imgur.com", "tumblr.com", "pinterest.com", "threads.net", "bsky.app",
    "loom.com", "vidio.com", "kaltura.com", "spotify.com",
];

fn media_pattern() -> String {
    format!(r"(?i)^https?://(?:[\w-]+\.)*(?:{})(?:[/:?#]|$)", MEDIA.iter().map(|d| regex::escape(d)).collect::<Vec<_>>().join("|"))
}
const PLAYLIST: &str = r"(?i)[?&]list=|/playlist|/channel/|/@[\w.-]+/?(?:videos|streams|shorts)?$|/c/|/user/|/sets/|/album/|/showcase/|/videos/?$";

pub fn all() -> Vec<P> {
    let mp = media_pattern();
    vec![
        Arc::new(YtdlpPlaylist { meta: Meta::crawler("ytdlp-playlist", "Media playlists (yt-dlp)", MEDIA, &[mp.as_str()]).prio(-10) }),
        Arc::new(Ytdlp::new("ytdlp", "Media (yt-dlp)", MEDIA.iter().map(|s| s.to_string()).collect(), &mp, -10)),
    ]
}

pub struct Ytdlp {
    meta: Meta,
}
impl Ytdlp {
    pub fn new(id: &str, name: &str, domains: Vec<String>, pattern: &str, prio: i32) -> Self {
        let mut meta = Meta::host(id, name, &[], &[pattern]).prio(prio).desc("Video/audio from 1,800+ sites through yt-dlp.").limits(Limits { max_downloads: Some(3), ..Default::default() });
        meta.domains = domains;
        Ytdlp { meta }
    }
}
struct YtdlpPlaylist {
    meta: Meta,
}

pub fn bin(ctx: &Ctx) -> String {
    ctx.setting("links.ytdlpPath").as_str().unwrap_or("yt-dlp").to_string()
}

pub fn hide(cmd: &mut Command) {
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd;
}

/// Split a command-line string into arguments (double/single quotes group words).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has = true;
            }
            None if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                has = false;
            }
            None => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub const IMPERSONATE: [&str; 2] = ["--extractor-args", "generic:impersonate"];

/// User arguments from settings + per-link learned flags (impersonation after a Cloudflare 403).
pub fn link_args(ctx: &Ctx) -> Vec<String> {
    let mut a = split_args(ctx.setting("links.ytdlpArgs").as_str().unwrap_or(""));
    if ctx.prop("ytdlpImpersonate") == Some(Value::Bool(true)) && !a.iter().any(|x| x.contains("impersonate")) {
        a.extend(IMPERSONATE.iter().map(|s| s.to_string()));
    }
    a
}

/// yt-dlp tells us when a Cloudflare challenge needs browser impersonation.
pub fn wants_impersonation(err: &str) -> bool {
    let l = err.to_lowercase();
    l.contains("impersonate") && !l.contains("not available") && !l.contains("no impersonate target")
}

fn classify(msg: &str) -> PluginError {
    let low = msg.to_lowercase();
    if low.contains("impersonat") && (low.contains("not available") || low.contains("no impersonate target")) {
        return PluginError::new(ErrCode::Unsupported, msg).hint("This yt-dlp build cannot impersonate browsers (curl_cffi missing). Leave Settings > yt-dlp executable as \"yt-dlp\" to use the managed copy.");
    }
    if low.contains("cloudflare") {
        return PluginError::new(ErrCode::Retry, msg).hint("Cloudflare blocked yt-dlp even with impersonation. Try browser cookies: Settings > Extra yt-dlp arguments: --cookies-from-browser firefox");
    }
    if ["video unavailable", "has been removed", "private video", "does not exist", "http error 404", "deleted"].iter().any(|k| low.contains(k)) {
        return offline(msg);
    }
    if low.contains("unsupported url") {
        return unsupported("yt-dlp does not support this URL");
    }
    if low.contains("sign in") || low.contains("login") || low.contains("cookies") {
        return PluginError::new(ErrCode::PremiumOnly, msg).hint("The site wants you to be logged in: add --cookies-from-browser firefox (or chrome/edge) in Settings > Extra yt-dlp arguments.");
    }
    PluginError::new(ErrCode::Retry, msg)
}

async fn run_json_raw(ctx: &Ctx, extra: &[&str], user: &[String]) -> PResult<Value> {
    let mut cmd = Command::new(bin(ctx));
    cmd.args(user).args(extra).args(["--no-warnings", "-J", &ctx.url]).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    hide(&mut cmd);
    let child = cmd.spawn().map_err(|e| unsupported(&format!("yt-dlp not runnable: {e}")).hint("Fetchwing downloads yt-dlp automatically; check Settings > Link handling."))?;
    let out = tokio::select! {
        o = child.wait_with_output() => o.map_err(|e| PluginError::new(ErrCode::Retry, e.to_string()))?,
        _ = ctx.cancel.cancelled() => return Err(PluginError::new(ErrCode::Cancelled, "cancelled")),
        _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => return Err(PluginError::new(ErrCode::Retry, "yt-dlp timed out")),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg: String = err.lines().filter(|l| l.contains("ERROR")).collect::<Vec<_>>().join(" ");
        let msg: String = if msg.is_empty() { err.chars().take(400).collect() } else { msg.chars().take(400).collect() };
        return Err(classify(&msg));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| PluginError::new(ErrCode::Retry, format!("yt-dlp json: {e}")))
}

/// Runs yt-dlp -J; on a Cloudflare challenge retries once with impersonation and remembers it for the link.
async fn run_json(ctx: &Ctx, extra: &[&str]) -> PResult<Value> {
    let user = link_args(ctx);
    match run_json_raw(ctx, extra, &user).await {
        Err(e) if wants_impersonation(&e.msg) && !user.iter().any(|a| a.contains("impersonate")) => {
            crate::log::info("ytdlp", &format!("{}: Cloudflare challenge - retrying with generic:impersonate", ctx.url));
            ctx.set_prop("ytdlpImpersonate", Value::Bool(true));
            let mut u2 = user.clone();
            u2.extend(IMPERSONATE.iter().map(|s| s.to_string()));
            run_json_raw(ctx, extra, &u2).await
        }
        r => r,
    }
}

pub fn clean_title(s: &str) -> String {
    let t: String = s.chars().map(|c| if "\\/:*?\"<>|".contains(c) { ' ' } else { c }).collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(150).collect()
}

#[async_trait]
impl Plugin for YtdlpPlaylist {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn matches(&self, url: &str) -> bool {
        self.meta.patterns.iter().any(|r| r.is_match(url)) && crate::util::http::re(PLAYLIST).is_match(url)
    }
    async fn crawl(&self, ctx: &mut Ctx) -> PResult<CrawlResult> {
        if ctx.setting("links.ytdlp") != Value::Bool(true) {
            return Err(unsupported("yt-dlp integration disabled"));
        }
        let j = run_json(ctx, &["--flat-playlist"]).await?;
        if j["_type"] != "playlist" {
            return Ok(CrawlResult { items: vec![CrawledItem::url(ctx.url.clone())], ..Default::default() });
        }
        let items = j["entries"].as_array().into_iter().flatten().filter_map(|e| {
            let u = j_str(e, "webpage_url").or_else(|| j_str(e, "url"))?;
            let mut it = CrawledItem::url(u);
            it.name = j_str(e, "title");
            Some(it)
        }).collect();
        Ok(CrawlResult { items, package_name: j_str(&j, "title").or(Some("Playlist".into())), ..Default::default() })
    }
}

#[async_trait]
impl Plugin for Ytdlp {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        if ctx.setting("links.ytdlp") != Value::Bool(true) {
            return Ok(Some(LinkInfo::unknown()));
        }
        let fmt = ctx.setting("links.ytdlpFormat").as_str().unwrap_or("bv*+ba/b").to_string();
        let j = run_json(ctx, &["--no-playlist", "-f", &fmt]).await?;
        let ext = j_str(&j, "ext").unwrap_or("mp4".into());
        let size = j_i64(&j, "filesize").or_else(|| j_i64(&j, "filesize_approx")).filter(|s| *s > 0);
        let title = clean_title(&j_str(&j, "title").or_else(|| j_str(&j, "id")).unwrap_or_default());
        let mut li = LinkInfo::online(Some(format!("{title} [{}].{ext}", j_str(&j, "id").unwrap_or_default())), size);
        li.canonical = j_str(&j, "webpage_url");
        Ok(Some(li))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        if ctx.setting("links.ytdlp") != Value::Bool(true) {
            return Err(unsupported("yt-dlp integration disabled").hint("Enable it in Settings → Link handling."));
        }
        Ok(Resolution::Ytdlp { url: ctx.url.clone(), args: link_args(ctx) })
    }
}

/// Run a yt-dlp download, reporting (done, total, speed). Returns the final file path.
pub async fn run_download(binary: &str, url: &str, format: &str, out_template: &str, extra: &[String], cancel: &CancellationToken, progress: &(dyn Fn(u64, u64, u64) + Send + Sync)) -> PResult<std::path::PathBuf> {
    match run_download_once(binary, url, format, out_template, extra, cancel, progress).await {
        Err(e) if wants_impersonation(&e.msg) && !extra.iter().any(|a| a.contains("impersonate")) => {
            crate::log::info("ytdlp", &format!("{url}: Cloudflare challenge - retrying download with generic:impersonate"));
            let mut x = extra.to_vec();
            x.extend(IMPERSONATE.iter().map(|s| s.to_string()));
            run_download_once(binary, url, format, out_template, &x, cancel, progress).await
        }
        r => r,
    }
}

async fn run_download_once(binary: &str, url: &str, format: &str, out_template: &str, extra: &[String], cancel: &CancellationToken, progress: &(dyn Fn(u64, u64, u64) + Send + Sync)) -> PResult<std::path::PathBuf> {
    let mut cmd = Command::new(binary);
    cmd.args(extra).args(["-f", format, "--no-playlist", "--newline", "--no-warnings", "--no-part", "--continue",
        "--progress-template", "download:FWP %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s",
        "--print", "after_move:FWF %(filepath)s", "-o", out_template, url])
        .stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    hide(&mut cmd);
    crate::log::info("ytdlp", &format!("{binary} {} -f {format} {url}", extra.join(" ")));
    let mut child = cmd.spawn().map_err(|e| unsupported(&format!("yt-dlp not runnable: {e}")))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let err_task = tokio::spawn(async move {
        let mut tail = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            tail.push_str(&l);
            tail.push('\n');
            if tail.len() > 4000 {
                tail = tail[tail.len() - 2000..].to_string();
            }
        }
        tail
    });
    let mut final_path = None;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = tokio::select! {
            l = lines.next_line() => l,
            _ = cancel.cancelled() => { let _ = child.kill().await; return Err(PluginError::new(ErrCode::Cancelled, "cancelled")); }
        };
        let Ok(Some(l)) = line else { break };
        if let Some(rest) = l.strip_prefix("FWP ") {
            let n: Vec<u64> = rest.split_whitespace().map(|x| x.parse::<f64>().map(|f| f as u64).unwrap_or(0)).collect();
            if n.len() == 4 {
                progress(n[0], if n[1] > 0 { n[1] } else { n[2] }, n[3]);
            }
        } else if let Some(p) = l.strip_prefix("FWF ") {
            final_path = Some(std::path::PathBuf::from(p.trim()));
        }
    }
    let status = child.wait().await.map_err(|e| PluginError::new(ErrCode::Retry, e.to_string()))?;
    let tail = err_task.await.unwrap_or_default();
    match (status.success(), final_path) {
        (true, Some(p)) => Ok(p),
        _ => {
            let msg: String = tail.lines().filter(|l| l.contains("ERROR")).collect::<Vec<_>>().join(" ");
            let m = format!("yt-dlp failed: {}", if msg.is_empty() { tail.chars().take(400).collect() } else { msg });
            let mut e = classify(&m);
            e.msg = m;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn args_and_detection() {
        assert_eq!(split_args(r#"--extractor-args "generic:impersonate" --cookies-from-browser firefox"#), vec!["--extractor-args", "generic:impersonate", "--cookies-from-browser", "firefox"]);
        assert_eq!(split_args("  "), Vec::<String>::new());
        assert!(wants_impersonation(r#"ERROR: [generic] Got HTTP Error 403 caused by Cloudflare anti-bot challenge; try again with  --extractor-args "generic:impersonate""#));
        assert!(!wants_impersonation("ERROR: Impersonate target \"chrome\" is not available"));
    }
}
