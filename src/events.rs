//! Everything the core emits. The web API forwards these 1:1 to clients over SSE; the GUI reads
//! state directly and only uses notices.

use crate::captcha::CaptchaJob;
use crate::model::{new_id, Link, Package};
use serde::Serialize;

#[derive(Clone, Debug, Serialize, Default)]
pub struct Stats {
    pub speed: u64,
    pub active: usize,
    pub queued: usize,
    pub waiting: usize,
    pub inbox: usize,
    pub finished: usize,
    pub failed: usize,
    pub bytes_session: u64,
    pub eta: i64,
    pub limit: u64,
    pub paused: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Notice {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub link_id: Option<String>,
    pub t: i64,
}
impl Notice {
    pub fn new(kind: &str, title: &str, body: &str, link_id: Option<String>) -> Self {
        Notice { id: new_id(), kind: kind.into(), title: title.into(), body: body.into(), link_id, t: crate::model::now_ms() }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LogLine {
    pub t: i64,
    pub level: &'static str,
    pub scope: String,
    pub msg: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    Links(Vec<Link>),
    LinksRemoved(Vec<String>),
    Packages(Vec<Package>),
    PackagesRemoved(Vec<String>),
    Stats(Stats),
    Notify(Notice),
    CaptchaNew(CaptchaJob),
    CaptchaDone { id: String, ok: bool },
    Log(LogLine),
    Accounts,
    Settings,
}
