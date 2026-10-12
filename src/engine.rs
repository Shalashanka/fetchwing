//! Segmented HTTP downloader.
//!
//! JD2 (jd/plugins/download/raf/HTTPDownloader + MaxJumpStrategy + FileBytesMap) plans chunks from
//! "unmarked areas" with a fixed chunk count. Fetchwing does dynamic work stealing: whenever a
//! connection finishes it splits the largest remaining segment in half, so all connections stay
//! busy until the end (no slow tail chunk) and the connection count adapts to what the server allows.

use crate::model::Segment;
use crate::util::http::{filename_from_disposition, Browser};
use crate::util::rate::TokenBucket;
use futures_util::StreamExt;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const COALESCE: usize = 512 * 1024;
const SEGMENT_RETRIES: u32 = 4;

#[derive(Clone, Debug, Default)]
pub struct DirectRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// upper bound imposed by the hoster (free users often get 1)
    pub max_connections: Option<usize>,
    /// Some(false) = known not to support Range; None = probe
    pub resumable: Option<bool>,
    /// fail if the server sends an HTML page instead of the file
    pub reject_html: bool,
}

impl DirectRequest {
    pub fn get(url: impl Into<String>) -> Self {
        DirectRequest { url: url.into(), method: "GET".into(), ..Default::default() }
    }
    pub fn html_guard(mut self) -> Self {
        self.reject_html = true;
        self
    }
    pub fn conns(mut self, n: usize) -> Self {
        self.max_connections = Some(n);
        self
    }
    pub fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_string(), v.into()));
        self
    }
}

#[derive(Debug, Default)]
pub struct DlState {
    pub segments: Vec<Segment>,
    pub size: i64,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub resumable: bool,
    owned: HashSet<usize>,
    inflight: HashMap<usize, u64>,
    max_conn: usize,
    /// Non-sparse files only: new segments must start within this distance of the furthest
    /// written byte. Writing far beyond it makes NTFS/exFAT zero-fill the whole gap first.
    max_jump: Option<u64>,
}

impl DlState {
    pub fn new(segments: Vec<Segment>, size: i64, etag: Option<String>, last_modified: Option<String>, resumable: bool) -> Self {
        DlState { segments, size, etag, last_modified, resumable, ..Default::default() }
    }
    pub fn bytes_done(&self) -> u64 {
        self.segments.iter().map(|s| s.pos - s.start).sum()
    }
    fn open(&self, i: usize) -> bool {
        let s = &self.segments[i];
        match s.end {
            Some(e) => s.pos <= e,
            None => true,
        }
    }
    fn unowned_open(&self) -> Option<usize> {
        (0..self.segments.len()).find(|&i| !self.owned.contains(&i) && self.segments[i].end.is_some() && self.open(i))
    }
    /// split the segment with the most bytes left; `busy` = workers that stay alive
    fn split(&mut self, busy: usize, min_seg: u64) -> Option<usize> {
        if busy >= self.max_conn {
            return None;
        }
        let mut best: Option<(usize, u64)> = None;
        for (i, s) in self.segments.iter().enumerate() {
            let Some(e) = s.end else { continue };
            let eff = s.pos + self.inflight.get(&i).copied().unwrap_or(0);
            let rem = (e + 1).saturating_sub(eff);
            if best.is_none_or(|b| rem > b.1) {
                best = Some((i, rem));
            }
        }
        let (i, rem) = best?;
        if rem < 2 * min_seg {
            return None;
        }
        let s = self.segments[i];
        let eff = s.pos + self.inflight.get(&i).copied().unwrap_or(0);
        let mut mid = eff + rem / 2;
        if let Some(jump) = self.max_jump {
            let frontier = self.segments.iter().enumerate().map(|(j, x)| x.pos + self.inflight.get(&j).copied().unwrap_or(0)).max().unwrap_or(0);
            mid = mid.min(frontier + jump);
            if mid < eff + min_seg {
                return None;
            }
        }
        self.segments.push(Segment { start: mid, end: s.end, pos: mid });
        self.segments[i].end = Some(mid - 1);
        Some(self.segments.len() - 1)
    }
}

pub struct Meta {
    pub size: i64,
    pub filename: Option<String>,
    pub resumable: bool,
    pub content_type: String,
}

pub struct DlJob {
    pub req: DirectRequest,
    pub browser: Browser,
    pub part_path: PathBuf,
    pub state: Arc<Mutex<DlState>>,
    pub connections: usize,
    pub min_segment: u64,
    pub preallocate: bool,
    pub limiters: Vec<Arc<TokenBucket>>,
    pub cancel: CancellationToken,
    pub on_progress: Arc<dyn Fn(u64) + Send + Sync>,
    pub on_meta: Arc<dyn Fn(Meta) + Send + Sync>,
    pub persist: Arc<dyn Fn(&DlState) + Send + Sync>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlKind {
    Network,
    Http,
    Disk,
    Cancelled,
    Expired,
    Html,
    Size,
}

#[derive(Debug, Clone)]
pub struct DlError {
    pub kind: DlKind,
    pub msg: String,
    pub retryable: bool,
    pub status: u16,
}

impl DlError {
    fn new(kind: DlKind, msg: impl Into<String>, retryable: bool) -> Self {
        DlError { kind, msg: msg.into(), retryable, status: 0 }
    }
    fn cancelled() -> Self {
        Self::new(DlKind::Cancelled, "Cancelled", false)
    }
    fn http(status: u16) -> Self {
        let kind = if status == 403 || status == 410 { DlKind::Expired } else { DlKind::Http };
        DlError { kind, msg: format!("HTTP {status}"), retryable: status >= 500 || matches!(status, 403 | 408 | 410 | 429), status }
    }
}
impl std::fmt::Display for DlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

enum End {
    Done,
    Released,
}

struct Shared {
    job_req: DirectRequest,
    browser: Browser,
    state: Arc<Mutex<DlState>>,
    file: Arc<File>,
    limiters: Vec<Arc<TokenBucket>>,
    cancel: CancellationToken,
    on_progress: Arc<dyn Fn(u64) + Send + Sync>,
}

#[cfg(windows)]
fn make_sparse(f: &File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_SPARSE;
    use windows_sys::Win32::System::IO::DeviceIoControl;
    let mut ret = 0u32;
    unsafe { DeviceIoControl(f.as_raw_handle() as _, FSCTL_SET_SPARSE, std::ptr::null(), 0, std::ptr::null_mut(), 0, &mut ret, std::ptr::null_mut()) != 0 }
}
/// ext4/xfs/btrfs/APFS create sparse holes automatically.
#[cfg(not(windows))]
fn make_sparse(_f: &File) -> bool {
    true
}

fn write_at(f: &File, buf: &[u8], off: u64) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_write(&buf[done..], off + done as u64)?;
            if n == 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "write returned 0"));
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.write_all_at(buf, off)
    }
}

async fn open_range(sh: &Shared, headers: Vec<(String, String)>) -> Result<reqwest::Response, DlError> {
    let mut all: Vec<(&str, String)> = sh.job_req.headers.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    all.extend(headers.iter().map(|(k, v)| (k.as_str(), v.clone())));
    let method = if sh.job_req.method.is_empty() { "GET" } else { sh.job_req.method.as_str() };
    tokio::select! {
        _ = sh.cancel.cancelled() => Err(DlError::cancelled()),
        r = sh.browser.open(&sh.job_req.url, &all, method, sh.job_req.body.clone()) => r.map_err(|e| DlError::new(DlKind::Network, e.to_string(), true)),
    }
}

async fn flush(sh: &Shared, idx: usize, buf: &mut Vec<u8>) -> Result<(), DlError> {
    if buf.is_empty() {
        return Ok(());
    }
    let (off, n) = {
        let mut st = sh.state.lock();
        let s = st.segments[idx];
        let mut n = buf.len() as u64;
        if let Some(e) = s.end {
            n = n.min((e + 1).saturating_sub(s.pos)); // segment may have been shortened by a split
        }
        st.inflight.insert(idx, n); // still owned until pos moves past it
        (s.pos, n as usize)
    };
    let data: Vec<u8> = buf[..n].to_vec();
    buf.clear();
    let f = sh.file.clone();
    let res = tokio::task::spawn_blocking(move || write_at(&f, &data, off)).await;
    let mut st = sh.state.lock();
    st.inflight.remove(&idx);
    match res {
        Ok(Ok(())) => {
            st.segments[idx].pos += n as u64;
            drop(st);
            (sh.on_progress)(n as u64);
            Ok(())
        }
        Ok(Err(e)) => Err(DlError::new(DlKind::Disk, format!("Write failed: {e}"), false)),
        Err(e) => Err(DlError::new(DlKind::Disk, format!("Write task failed: {e}"), false)),
    }
}

async fn pump(sh: &Shared, idx: usize, resp: reqwest::Response) -> Result<(), DlError> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::with_capacity(COALESCE + 65536);
    let result: Result<(), DlError> = async {
        loop {
            let next = tokio::select! {
                _ = sh.cancel.cancelled() => return Err(DlError::cancelled()),
                c = stream.next() => c,
            };
            let chunk = match next {
                None => return Ok(()),
                Some(Err(e)) => return Err(DlError::new(DlKind::Network, crate::util::http::HttpErr::from(e).to_string(), true)),
                Some(Ok(c)) => c,
            };
            for l in &sh.limiters {
                l.take(chunk.len()).await;
            }
            let limit = {
                let mut st = sh.state.lock();
                let s = st.segments[idx];
                let lim = match s.end {
                    Some(e) => (e + 1).saturating_sub(s.pos + buf.len() as u64),
                    None => u64::MAX,
                };
                if lim > 0 {
                    let take = (chunk.len() as u64).min(lim) as usize;
                    buf.extend_from_slice(&chunk[..take]);
                    st.inflight.insert(idx, buf.len() as u64);
                }
                lim
            };
            if limit == 0 {
                return Ok(());
            }
            if buf.len() >= COALESCE {
                flush(sh, idx, &mut buf).await?;
            }
            if limit <= chunk.len() as u64 {
                return Ok(()); // reached the (possibly shrunk) end
            }
        }
    }
    .await;
    let f = flush(sh, idx, &mut buf).await;
    result.and(f)
}

async fn worker(sh: Arc<Shared>, idx: usize, mut pre: Option<reqwest::Response>, min_seg: u64) -> (usize, Result<End, DlError>) {
    let _ = min_seg;
    let mut attempt = 0u32;
    loop {
        let (seg, resumable) = {
            let st = sh.state.lock();
            (st.segments[idx], st.resumable)
        };
        if seg.done() {
            return (idx, Ok(End::Done));
        }
        if sh.cancel.is_cancelled() {
            return (idx, Err(DlError::cancelled()));
        }
        let r = match pre.take() {
            Some(r) => Ok(r),
            None => {
                let range = match seg.end {
                    Some(e) => format!("bytes={}-{}", seg.pos, e),
                    None => format!("bytes={}-", seg.pos),
                };
                match open_range(&sh, vec![("range".into(), range)]).await {
                    Ok(r) => {
                        let st = r.status().as_u16();
                        if st == 200 && seg.pos > 0 {
                            Err(DlError { kind: DlKind::Http, msg: "Range not honoured".into(), retryable: false, status: 200 })
                        } else if st != 206 && st != 200 {
                            Err(DlError::http(st))
                        } else {
                            Ok(r)
                        }
                    }
                    Err(e) => Err(e),
                }
            }
        };
        let outcome = match r {
            Ok(resp) => pump(&sh, idx, resp).await,
            Err(e) => Err(e),
        };
        match outcome {
            Ok(()) => {
                let mut st = sh.state.lock();
                let s = st.segments[idx];
                match s.end {
                    None => {
                        st.segments[idx].end = Some(s.pos.saturating_sub(1)); // stream end == done (unknown length)
                        if s.pos == 0 {
                            st.segments[idx].end = None;
                            st.segments[idx].pos = 0;
                            return (idx, Ok(End::Done));
                        }
                        return (idx, Ok(End::Done));
                    }
                    Some(e) if s.pos > e => return (idx, Ok(End::Done)),
                    Some(_) => {
                        drop(st);
                        // premature close -> retry below
                        if attempt >= SEGMENT_RETRIES || !resumable {
                            return (idx, Err(DlError::new(DlKind::Network, "Connection closed early", true)));
                        }
                        attempt += 1;
                    }
                }
            }
            Err(e) => {
                if e.kind == DlKind::Cancelled || sh.cancel.is_cancelled() {
                    return (idx, Err(DlError::cancelled()));
                }
                let others = sh.state.lock().owned.len() > 1;
                if others && matches!(e.status, 200 | 403 | 416 | 429 | 503) {
                    let mut st = sh.state.lock();
                    let n = st.owned.len().saturating_sub(1).max(1);
                    st.max_conn = st.max_conn.min(n);
                    return (idx, Ok(End::Released)); // too many connections for this server
                }
                if !e.retryable || attempt >= SEGMENT_RETRIES || !resumable {
                    return (idx, Err(e));
                }
                attempt += 1;
            }
        }
        let backoff = Duration::from_millis(1000 * 2u64.pow(attempt.saturating_sub(1)));
        tokio::select! {
            _ = sh.cancel.cancelled() => return (idx, Err(DlError::cancelled())),
            _ = tokio::time::sleep(backoff) => {}
        }
    }
}

/// Download `job.req` into `job.part_path`, resuming from `job.state` when possible.
pub async fn download(job: &DlJob) -> Result<u64, DlError> {
    if job.cancel.is_cancelled() {
        return Err(DlError::cancelled());
    }
    let part_exists = job.part_path.exists();
    let (resuming, range_start, range_end, etag, lastmod) = {
        let mut st = job.state.lock();
        if !part_exists {
            st.segments.clear();
        }
        let mut resuming = st.segments.iter().any(|s| s.pos > s.start);
        if resuming && (!st.resumable || job.req.resumable == Some(false)) {
            st.segments.clear();
            resuming = false;
        }
        let first = st.segments.iter().find(|s| !s.done()).copied();
        (resuming, first.map(|s| s.pos).unwrap_or(0), first.and_then(|s| s.end), st.etag.clone(), st.last_modified.clone())
    };

    let sh_tmp_browser = job.browser.clone();
    let probe = |headers: Vec<(String, String)>| {
        let b = sh_tmp_browser.clone();
        let req = job.req.clone();
        let cancel = job.cancel.clone();
        async move {
            let mut all: Vec<(&str, String)> = req.headers.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            all.extend(headers.iter().map(|(k, v)| (k.as_str(), v.clone())));
            let method = if req.method.is_empty() { "GET".to_string() } else { req.method.clone() };
            tokio::select! {
                _ = cancel.cancelled() => Err(DlError::cancelled()),
                r = b.open(&req.url, &all, &method, req.body.clone()) => r.map_err(|e| DlError::new(DlKind::Network, e.to_string(), true)),
            }
        }
    };
    let mut first_headers: Vec<(String, String)> = vec![];
    if job.req.resumable != Some(false) {
        first_headers.push(("range".into(), match range_end {
            Some(e) => format!("bytes={range_start}-{e}"),
            None => format!("bytes={range_start}-"),
        }));
        if resuming {
            if let Some(v) = etag.clone().or(lastmod.clone()) {
                first_headers.push(("if-range".into(), v));
            }
        }
    }
    let mut resp = probe(first_headers.clone()).await?;
    if resp.status().as_u16() == 416 && !resuming {
        resp = probe(vec![]).await?;
    }
    let status = resp.status().as_u16();
    let hdr = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    let ctype = hdr("content-type").unwrap_or_default();
    if job.req.reject_html && ctype.to_ascii_lowercase().contains("text/html") {
        return Err(DlError::new(DlKind::Html, "Server returned a web page instead of the file (link expired or hoster error page)", true));
    }
    let new_etag = hdr("etag");
    let new_lastmod = hdr("last-modified");
    let disposition = hdr("content-disposition");
    let total: i64;
    let mut range_ok = false;
    let mut restart = false;
    match status {
        206 => {
            let cr = hdr("content-range").unwrap_or_default();
            let c = crate::util::http::re(r"(?i)bytes\s+(\d+)-(\d+)/(\d+|\*)").captures(&cr);
            match c {
                Some(c) if c[1].parse::<u64>().ok() == Some(range_start) => {
                    range_ok = true;
                    total = c[3].parse::<i64>().unwrap_or(-1);
                }
                _ => return Err(DlError::new(DlKind::Http, format!("Bad Content-Range \"{cr}\""), true)),
            }
        }
        200 => {
            total = resp.content_length().map(|n| n as i64).unwrap_or(-1);
            if resuming {
                restart = true; // server ignored the range: file changed or no range support
            }
        }
        s => return Err(DlError::http(s)),
    }
    {
        let mut st = job.state.lock();
        if resuming && st.size > 0 && total > 0 && total != st.size {
            st.segments.clear();
            st.etag = None;
            st.last_modified = None;
            return Err(DlError::new(DlKind::Size, format!("Remote file changed size ({} -> {total}); restarting", st.size), true));
        }
        if restart {
            st.segments.clear();
        }
        st.size = total;
        st.etag = new_etag.or(st.etag.take());
        st.last_modified = new_lastmod.or(st.last_modified.take());
        st.resumable = range_ok;
        st.max_conn = if range_ok { job.connections.min(job.req.max_connections.unwrap_or(32)).max(1) } else { 1 };
    }
    (job.on_meta)(Meta { size: total, filename: filename_from_disposition(disposition.as_deref()), resumable: range_ok, content_type: ctype });

    // ---- file ----
    let fresh = job.state.lock().segments.is_empty();
    if let Some(dir) = job.part_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| DlError::new(DlKind::Disk, format!("Cannot create folder: {e}"), false))?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(fresh)
        .open(&job.part_path)
        .map_err(|e| DlError::new(DlKind::Disk, format!("Cannot open {}: {e}", job.part_path.display()), false))?;
    // Sparse file: segments far into the file (e.g. 105 GB into a 120 GB download) can be written
    // immediately. Without it Windows zero-fills the whole gap before the write returns, which
    // stalls every connection for minutes and briefly occupies huge amounts of disk.
    if !make_sparse(&file) && total > 256 * 1024 * 1024 {
        job.state.lock().max_jump = Some(256 * 1024 * 1024);
    }
    if fresh {
        if total > 0 && job.preallocate {
            file.set_len(total as u64).map_err(|e| DlError::new(DlKind::Disk, format!("Pre-allocation failed: {e}"), false))?;
        }
        let mut st = job.state.lock();
        st.segments = vec![Segment { start: 0, end: if total > 0 && range_ok { Some(total as u64 - 1) } else { None }, pos: 0 }];
    }
    (job.persist)(&job.state.lock());

    let child = job.cancel.child_token();
    let sh = Arc::new(Shared {
        job_req: job.req.clone(),
        browser: job.browser.clone(),
        state: job.state.clone(),
        file: Arc::new(file),
        limiters: job.limiters.clone(),
        cancel: child.clone(),
        on_progress: job.on_progress.clone(),
    });
    let mut set: JoinSet<(usize, Result<End, DlError>)> = JoinSet::new();
    let spawn = |set: &mut JoinSet<_>, idx: usize, pre: Option<reqwest::Response>| {
        sh.state.lock().owned.insert(idx);
        set.spawn(worker(sh.clone(), idx, pre, job.min_segment));
    };
    let first_idx = {
        let st = job.state.lock();
        (0..st.segments.len()).find(|&i| st.open(i)).unwrap_or(0)
    };
    spawn(&mut set, first_idx, Some(resp));
    // resume: other unfinished segments, then fill the remaining slots by splitting
    loop {
        let next = {
            let mut st = job.state.lock();
            if !st.resumable || set.len() >= st.max_conn {
                None
            } else {
                st.unowned_open().or_else(|| st.split(set.len(), job.min_segment))
            }
        };
        match next {
            Some(i) => spawn(&mut set, i, None),
            None => break,
        }
    }

    let mut fatal: Option<DlError> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            res = set.join_next() => {
                let Some(res) = res else { break };
                let (idx, out) = res.unwrap_or_else(|e| (usize::MAX, Err(DlError::new(DlKind::Network, format!("worker panicked: {e}"), true))));
                job.state.lock().owned.remove(&idx);
                if let Err(e) = out {
                    if e.kind != DlKind::Cancelled && fatal.is_none() {
                        fatal = Some(e);
                        child.cancel();
                    }
                }
                if fatal.is_none() && !child.is_cancelled() {
                    loop {
                        let next = {
                            let mut st = job.state.lock();
                            if !st.resumable && !set.is_empty() { None }
                            else if set.len() >= st.max_conn { None }
                            else { st.unowned_open().or_else(|| if st.resumable { st.split(set.len(), job.min_segment) } else { None }) }
                        };
                        match next { Some(i) => spawn(&mut set, i, None), None => break }
                    }
                }
            }
            _ = tick.tick() => { (job.persist)(&job.state.lock()); }
        }
    }
    (job.persist)(&job.state.lock());
    if job.cancel.is_cancelled() {
        return Err(DlError::cancelled());
    }
    if let Some(e) = fatal {
        return Err(e);
    }
    let mut st = job.state.lock();
    if (0..st.segments.len()).any(|i| st.segments[i].end.is_some() && st.open(i)) {
        return Err(DlError::new(DlKind::Network, "Download incomplete", true));
    }
    let got = st.bytes_done();
    if st.size < 0 {
        st.size = got as i64;
    }
    if got as i64 != st.size {
        return Err(DlError::new(DlKind::Size, format!("Size mismatch: expected {}, got {got}", st.size), true));
    }
    let _ = sh.file.sync_all();
    Ok(got)
}
