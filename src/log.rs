//! Process-wide logger: rotating file + ring buffer (UI log view) + event fan-out.

use crate::events::{Event, LogLine};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::OnceLock;
use tokio::sync::broadcast;

struct Sink {
    file: Option<std::fs::File>,
    ring: VecDeque<LogLine>,
    events: Option<broadcast::Sender<Event>>,
    echo: bool,
}

fn sink() -> &'static Mutex<Sink> {
    static S: OnceLock<Mutex<Sink>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Sink { file: None, ring: VecDeque::new(), events: None, echo: false }))
}

pub fn init(dir: &std::path::Path, echo: bool, events: broadcast::Sender<Event>) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("fetchwing.log");
    if std::fs::metadata(&path).map(|m| m.len() > 10 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("fetchwing.log.1"));
    }
    let mut s = sink().lock();
    s.file = std::fs::OpenOptions::new().create(true).append(true).open(path).ok();
    s.events = Some(events);
    s.echo = echo;
}

pub fn write(level: &'static str, scope: &str, msg: &str) {
    let line = LogLine { t: crate::model::now_ms(), level, scope: scope.to_string(), msg: msg.to_string() };
    let text = format!("{} {:5} [{}] {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), level.to_uppercase(), scope, msg);
    let mut s = sink().lock();
    if let Some(f) = s.file.as_mut() {
        let _ = writeln!(f, "{text}");
    }
    if s.echo {
        eprintln!("{text}");
    }
    if level != "debug" {
        s.ring.push_back(line.clone());
        if s.ring.len() > 1000 {
            s.ring.pop_front();
        }
        if let Some(e) = &s.events {
            let _ = e.send(Event::Log(line));
        }
    }
}

pub fn recent() -> Vec<LogLine> {
    sink().lock().ring.iter().cloned().collect()
}

pub fn info(scope: &str, msg: &str) {
    write("info", scope, msg)
}
pub fn warn(scope: &str, msg: &str) {
    write("warn", scope, msg)
}
pub fn error(scope: &str, msg: &str) {
    write("error", scope, msg)
}
pub fn debug(scope: &str, msg: &str) {
    write("debug", scope, msg)
}
