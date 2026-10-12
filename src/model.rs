//! Core domain model.
//!
//! JD2 keeps two separate trees (LinkCollector "CrawledLink/CrawledPackage" and DownloadController
//! "DownloadLink/FilePackage") and copies objects between them on confirmation - the root of the
//! "I added links, why is nothing downloading?" confusion. Fetchwing keeps ONE tree; a link moves
//! through stages (Inbox -> Queue -> Done) and the UI shows stages as views.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Inbox,
    Queue,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    New,
    Checking,
    Online,
    Offline,
    Unknown,
    Queued,
    Waiting,
    Captcha,
    Downloading,
    Paused,
    Postprocessing,
    Finished,
    Failed,
    Skipped,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::New => "new",
            Status::Checking => "checking",
            Status::Online => "online",
            Status::Offline => "offline",
            Status::Unknown => "unknown",
            Status::Queued => "queued",
            Status::Waiting => "waiting",
            Status::Captcha => "captcha",
            Status::Downloading => "downloading",
            Status::Paused => "paused",
            Status::Postprocessing => "processing",
            Status::Finished => "finished",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub start: u64,
    /// inclusive end; `None` = unknown length (single stream)
    pub end: Option<u64>,
    /// next byte to write (absolute offset)
    pub pos: u64,
}

impl Segment {
    pub fn done(&self) -> bool {
        matches!(self.end, Some(e) if self.pos > e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
    Crc32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HashInfo {
    pub algo: HashAlgo,
    pub value: String,
    #[serde(default)]
    pub verified: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LinkError {
    pub code: String,
    pub message: String,
    /// One actionable sentence for the user (fixes JD2's opaque "Plugin defect" errors).
    pub hint: Option<String>,
    pub retryable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Link {
    pub id: String,
    pub package_id: String,
    pub url: String,
    pub origin: Option<String>,
    pub host: String,
    pub plugin_id: String,
    pub name: String,
    /// -1 = unknown
    pub size: i64,
    pub bytes_done: u64,
    pub stage: Stage,
    pub status: Status,
    pub message: Option<String>,
    pub error: Option<LinkError>,
    pub wait_until: Option<i64>,
    pub enabled: bool,
    pub priority: i32,
    pub segments: Vec<Segment>,
    pub resumable: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub hash: Option<HashInfo>,
    pub passwords: Vec<String>,
    pub attempts: u32,
    pub added_at: i64,
    pub finished_at: Option<i64>,
    /// plugin-private persisted state (direct-link cache, tokens, keys...)
    pub props: Map<String, Value>,
    #[serde(default)]
    pub speed: u64,
}

impl Link {
    pub fn prop_str(&self, k: &str) -> Option<&str> {
        self.props.get(k).and_then(|v| v.as_str())
    }
    pub fn progress(&self) -> f32 {
        if self.size > 0 {
            (self.bytes_done as f64 / self.size as f64).min(1.0) as f32
        } else if self.status == Status::Finished {
            1.0
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub dir: String,
    pub stage: Stage,
    pub order: i64,
    pub priority: i32,
    pub expanded: bool,
    pub comment: Option<String>,
    pub passwords: Vec<String>,
    pub auto_extract: bool,
    pub added_at: i64,
    pub source: String,
    /// name was guessed from a URL; replace once the online check knows the real filename
    #[serde(default)]
    pub auto_name: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AccountStatus {
    Unchecked,
    Ok,
    Invalid,
    Expired,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub host: String,
    pub user: String,
    /// AES-256-GCM encrypted (see accounts.rs); never sent to clients
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret: String,
    pub enabled: bool,
    pub status: AccountStatus,
    pub premium: Option<bool>,
    pub traffic_left: Option<i64>,
    pub valid_until: Option<i64>,
    pub message: Option<String>,
    pub last_check: Option<i64>,
    #[serde(default)]
    pub supported_hosts: Vec<String>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// time-sortable id: base36 millis + random suffix
pub fn new_id() -> String {
    use rand::Rng;
    let mut n = now_ms() as u64;
    let mut s = Vec::new();
    while n > 0 {
        let d = (n % 36) as u8;
        s.push(if d < 10 { b'0' + d } else { b'a' + d - 10 });
        n /= 36;
    }
    s.reverse();
    let mut out = String::from_utf8(s).unwrap_or_default();
    let mut rng = rand::thread_rng();
    for _ in 0..6 {
        let d: u8 = rng.gen_range(0..36);
        out.push(if d < 10 { (b'0' + d) as char } else { (b'a' + d - 10) as char });
    }
    out
}
