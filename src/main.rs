//! Fetchwing - a modern download manager (research rebuild of the JDownloader 2 idea in Rust).
//!
//!   fetchwing                 desktop app (+ local web UI/API)
//!   fetchwing <url>...        add links to the running instance (or start it with them)
//!   fetchwing --headless      daemon only: web UI + API (servers, NAS, Docker)
//!   fetchwing add|ls|start|pause|limit|confirm|retry|remove|captchas|solve|plugins|match|accounts|account-add|get|set|log|update-ytdlp
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![allow(dead_code)]

mod accounts;
mod api;
mod captcha;
mod cli;
mod cnl;
mod core;
mod crawler;
mod engine;
#[cfg(test)]
mod engine_tests;
mod events;
mod gui;
mod log;
mod model;
mod plugins;
mod postprocess;
mod settings;
mod tools;
mod util;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(name = "fetchwing", version, about = "Fetchwing - modern download manager", long_about = None)]
struct Args {
    /// Run without a window: web UI + API only
    #[arg(long)]
    headless: bool,
    /// Data folder (default: per-user app data)
    #[arg(long, env = "FETCHWING_DATA")]
    data: Option<PathBuf>,
    /// Web UI / API port (overrides the setting)
    #[arg(long)]
    port: Option<u16>,
    /// Listen address for the web UI / API, e.g. 0.0.0.0 for LAN access
    #[arg(long)]
    bind: Option<String>,
    /// Download folder (overrides the setting)
    #[arg(long)]
    download_dir: Option<PathBuf>,
    /// Remote instance for CLI commands, e.g. http://nas:7979
    #[arg(long)]
    url: Option<String>,
    /// API token for CLI commands against a remote instance
    #[arg(long)]
    token: Option<String>,
    /// add: keep links in the Inbox instead of starting them
    #[arg(long)]
    inbox: bool,
    /// add: package name
    #[arg(long)]
    package: Option<String>,
    /// add: archive password
    #[arg(long)]
    password: Option<String>,
    /// Command (add, ls, start, pause, …) or links to add
    rest: Vec<String>,
}

const COMMANDS: &[&str] = &["add", "ls", "list", "status", "start", "pause", "limit", "confirm", "retry", "remove", "reset", "pause-link", "captchas", "solve", "plugins", "match", "accounts", "account-add", "get", "set", "log", "update-ytdlp"];

fn default_data_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local").join("share")).join(if cfg!(windows) || cfg!(target_os = "macos") { "Fetchwing" } else { "fetchwing" })
}

#[cfg(windows)]
fn attach_console() {
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
}
#[cfg(not(windows))]
fn attach_console() {}

fn main() {
    let args = Args::parse();
    let data = args.data.clone().unwrap_or_else(default_data_dir);
    let is_cmd = args.rest.first().is_some_and(|c| COMMANDS.contains(&c.as_str()));
    if is_cmd || args.headless {
        attach_console();
    }
    // `fetchwing ls | head` must end quietly, not panic on a closed pipe
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("fetchwing").build().expect("tokio runtime");

    // ---- CLI client mode ----
    if is_cmd {
        let c = cli::Client::new(&data, args.url.clone(), args.token.clone());
        let cmd = args.rest[0].clone();
        let rest = args.rest[1..].to_vec();
        if let Err(e) = rt.block_on(cli::run(&c, &cmd, &rest, args.inbox, args.package.clone(), args.password.clone())) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }

    // ---- single instance: hand links to a running Fetchwing ----
    let client = cli::Client::new(&data, None, None);
    if rt.block_on(client.alive()) {
        if !args.rest.is_empty() {
            let text = args.rest.join("\n");
            let _ = rt.block_on(client.call("POST", "/links", Some(serde_json::json!({"text": text, "source": "cli"}))));
        }
        if !args.headless {
            // already running: nothing else to do (its window is open)
        }
        return;
    }

    // ---- start the core ----
    let core = core::Core::new(data.clone());
    if let Some(p) = args.port {
        let _ = core.settings.set("remote.port", serde_json::json!(p));
    }
    if let Some(b) = &args.bind {
        let _ = core.settings.set("remote.bind", serde_json::json!(b));
    }
    if let Some(d) = &args.download_dir {
        let _ = core.settings.set("general.downloadDir", serde_json::json!(d.to_string_lossy()));
    }
    let api_url = rt.block_on(async {
        core.start();
        let url = match api::serve(core.clone()).await {
            Ok(u) => u,
            Err(e) => {
                crate::log::error("api", &e);
                String::new()
            }
        };
        let cnl = core.settings.bool("links.cnl") && cnl::serve(core.clone()).await;
        {
            let mut s = core.sys.lock();
            s.api_url = url.clone();
            s.cnl = cnl;
        }
        if !args.rest.is_empty() {
            core.add_links(core::AddRequest::text(&args.rest.join("\n"), "cli"));
        }
        spawn_clipboard_watch(core.clone(), tokio::runtime::Handle::current());
        url
    });

    if args.headless {
        println!("Fetchwing {} running headless", env!("CARGO_PKG_VERSION"));
        println!("  Web UI / API : {api_url}");
        println!("  Access token : {}", core.settings.str("remote.token"));
        println!("  Data folder  : {}", data.display());
        rt.block_on(async {
            let _ = tokio::signal::ctrl_c().await;
            println!("shutting down…");
            core.shutdown().await;
        });
        return;
    }

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).ok();
    let mut vp = eframe::egui::ViewportBuilder::default().with_title("Fetchwing").with_inner_size([1280.0, 800.0]).with_min_inner_size([820.0, 480.0]).with_drag_and_drop(true);
    if let Some(i) = icon {
        vp = vp.with_icon(Arc::new(i));
    }
    let opts = eframe::NativeOptions { viewport: vp, persist_window: true, ..Default::default() };
    let handle = rt.handle().clone();
    let core2 = core.clone();
    let res = eframe::run_native("Fetchwing", opts, Box::new(move |cc| Ok(Box::new(gui::App::new(cc, core2, handle, api_url)))));
    if let Err(e) = res {
        crate::log::error("gui", &format!("window failed: {e} - continuing headless; open the web UI instead"));
        rt.block_on(async {
            let _ = tokio::signal::ctrl_c().await;
        });
    }
    rt.block_on(core.shutdown());
}

/// Optional clipboard watcher (off by default; new links land in the Inbox).
fn spawn_clipboard_watch(core: Arc<core::Core>, rt: tokio::runtime::Handle) {
    std::thread::Builder::new().name("clipboard".into()).spawn(move || {
        let mut last = String::new();
        let mut first = true;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1200));
            if !core.settings.bool("links.clipboard") {
                first = true;
                continue;
            }
            let Ok(mut cb) = arboard::Clipboard::new() else { continue };
            let Ok(text) = cb.get_text() else { continue };
            if text == last {
                continue;
            }
            last = text.clone();
            if first {
                first = false;
                continue;
            }
            if !crawler::extract_urls(&text).is_empty() {
                let mut req = core::AddRequest::text(&text, "clipboard");
                req.auto_start = Some(false);
                let _g = rt.enter();
                core.add_links(req);
            }
        }
    }).ok();
}

