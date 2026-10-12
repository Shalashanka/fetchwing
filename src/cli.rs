//! `fetchwing <command>` - command-line client for a running Fetchwing (local or remote).

use serde_json::{json, Value};
use std::path::Path;

pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    /// Connection details from flags/env, falling back to the local data dir's state.json.
    pub fn new(data_dir: &Path, url: Option<String>, token: Option<String>) -> Self {
        let st: Value = std::fs::read(data_dir.join("state.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let s = &st["kv"]["settings"];
        let port = s["remote.port"].as_f64().unwrap_or(7979.0) as u16;
        let base = url.or_else(|| std::env::var("FETCHWING_URL").ok()).unwrap_or(format!("http://127.0.0.1:{port}")).trim_end_matches('/').to_string();
        let token = token.or_else(|| std::env::var("FETCHWING_TOKEN").ok()).unwrap_or_else(|| s["remote.token"].as_str().unwrap_or("").to_string());
        Client { base, token, http: reqwest::Client::new() }
    }

    pub async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut rb = self.http.request(m, format!("{}/api{path}", self.base)).bearer_auth(&self.token).timeout(std::time::Duration::from_secs(30));
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.map_err(|e| format!("Cannot reach Fetchwing at {} ({e}). Is it running?", self.base))?;
        let st = r.status();
        let v: Value = r.json().await.unwrap_or_default();
        if !st.is_success() {
            return Err(format!("{}: {}", st, v["error"].as_str().unwrap_or("")));
        }
        Ok(v)
    }

    pub async fn alive(&self) -> bool {
        self.call("GET", "/captchas", None).await.is_ok()
    }
}

fn fmt(n: i64) -> String {
    crate::util::fmt_bytes(n)
}

pub async fn run(c: &Client, cmd: &str, args: &[String], inbox: bool, package: Option<String>, password: Option<String>) -> Result<(), String> {
    match cmd {
        "add" => {
            let text = if args.is_empty() {
                use std::io::Read;
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s).ok();
                s
            } else {
                args.join("\n")
            };
            let r = c.call("POST", "/links", Some(json!({"text": text, "source": "cli", "packageName": package, "passwords": password.map(|p| vec![p]), "autoStart": if inbox { Some(false) } else { None }}))).await?;
            let id = r["jobId"].as_str().unwrap_or("").to_string();
            for _ in 0..240 {
                let j = c.call("GET", &format!("/jobs/{id}"), None).await?;
                if j["done"] == true {
                    println!("✓ {} link(s) added", j["found"]);
                    for p in j["problems"].as_array().into_iter().flatten() {
                        println!("  ! {} {}", p[0].as_str().unwrap_or(""), p[1].as_str().unwrap_or(""));
                    }
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            println!("still crawling in the background…");
        }
        "ls" | "list" | "status" => {
            let s = c.call("GET", "/state", None).await?;
            let st = &s["stats"];
            println!("Fetchwing {}  ↓ {}/s  active {}  queued {}  waiting {}  inbox {}  done {}{}",
                if st["paused"] == true { "PAUSED" } else { "running" }, fmt(st["speed"].as_i64().unwrap_or(0)), st["active"], st["queued"], st["waiting"], st["inbox"], st["finished"],
                if s["captchas"].as_array().is_some_and(|a| !a.is_empty()) { format!("  captchas {}", s["captchas"].as_array().unwrap().len()) } else { String::new() });
            let all = args.iter().any(|a| a == "all");
            for p in s["packages"].as_array().into_iter().flatten() {
                let links: Vec<&Value> = s["links"].as_array().into_iter().flatten().filter(|l| l["package_id"] == p["id"] && (all || l["stage"] != "done")).collect();
                if links.is_empty() {
                    continue;
                }
                println!("\n{}  [{}]  {}", p["name"].as_str().unwrap_or(""), p["stage"].as_str().unwrap_or(""), p["dir"].as_str().unwrap_or(""));
                for l in links {
                    let size = l["size"].as_i64().unwrap_or(-1);
                    let done = l["bytes_done"].as_i64().unwrap_or(0);
                    let pct = if size > 0 { done * 100 / size } else { 0 };
                    let speed = l["speed"].as_i64().unwrap_or(0);
                    println!("  {} {:<12} {:>3}% {} ({}, {}){}{}", l["id"].as_str().unwrap_or(""), l["status"].as_str().unwrap_or(""), pct, l["name"].as_str().unwrap_or(""), l["host"].as_str().unwrap_or(""), fmt(size),
                        if speed > 0 { format!(" {}/s", fmt(speed)) } else { String::new() },
                        l["message"].as_str().map(|m| format!("  - {m}")).unwrap_or_default());
                    if let Some(h) = l["error"]["hint"].as_str() {
                        println!("      → {h}");
                    }
                }
            }
        }
        "start" | "pause" => {
            c.call("POST", "/control", Some(json!({"action": cmd}))).await?;
            println!("{}", if cmd == "start" { "Downloads started" } else { "Downloads paused" });
        }
        "limit" => {
            let mb: f64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(0.0);
            c.call("POST", "/control", Some(json!({"speedLimit": (mb * 1048576.0) as u64}))).await?;
            println!("{}", if mb > 0.0 { format!("Limit {mb} MB/s") } else { "Unlimited".into() });
        }
        "confirm" => {
            let ids: Vec<String> = if args.is_empty() {
                let s = c.call("GET", "/state", None).await?;
                s["links"].as_array().into_iter().flatten().filter(|l| l["stage"] == "inbox").filter_map(|l| l["id"].as_str().map(str::to_string)).collect()
            } else {
                args.to_vec()
            };
            let r = c.call("POST", "/links/action", Some(json!({"action": "confirm", "ids": ids}))).await?;
            println!("{} link(s) moved to the queue", r["n"]);
        }
        "retry" | "remove" | "pause-link" | "reset" => {
            let action = match cmd { "retry" => "resume", "remove" => "remove", "reset" => "reset", _ => "pause" };
            c.call("POST", "/links/action", Some(json!({"action": action, "ids": args}))).await?;
            println!("ok");
        }
        "captchas" => {
            for j in c.call("GET", "/captchas", None).await?.as_array().into_iter().flatten() {
                println!("{}  {}  {}", j["id"].as_str().unwrap_or(""), j["host"].as_str().unwrap_or(""), j["request"]["type"].as_str().unwrap_or(""));
            }
        }
        "solve" => {
            let id = args.first().ok_or("usage: fetchwing solve <id> <answer>")?;
            c.call("POST", &format!("/captchas/{id}"), Some(json!({"value": args[1..].join(" ")}))).await?;
            println!("ok");
        }
        "plugins" => {
            let r = c.call("GET", "/plugins", None).await?;
            let f = args.first().map(|s| s.to_lowercase()).unwrap_or_default();
            let mut n = 0;
            for p in r["plugins"].as_array().into_iter().flatten() {
                let doms: Vec<&str> = p["domains"].as_array().into_iter().flatten().filter_map(|d| d.as_str()).collect();
                if !f.is_empty() && !p["id"].as_str().unwrap_or("").contains(&f) && !doms.iter().any(|d| d.contains(&f)) {
                    continue;
                }
                n += 1;
                if n <= 200 {
                    println!("{:<8} {:<36} {:<16} {}", p["kind"].as_str().unwrap_or(""), p["id"].as_str().unwrap_or(""), p["version"].as_str().unwrap_or(""), doms.iter().take(3).cloned().collect::<Vec<_>>().join(", "));
                }
            }
            println!("{n} plugin(s); templates: {}", r["templates"]);
        }
        "match" => println!("{}", c.call("GET", &format!("/plugins/match?url={}", percent_encoding::utf8_percent_encode(args.first().map(String::as_str).unwrap_or(""), percent_encoding::NON_ALPHANUMERIC)), None).await?),
        "accounts" => {
            for a in c.call("GET", "/accounts", None).await?.as_array().into_iter().flatten() {
                println!("{}  {:<20} {:<20} {} {}", a["id"].as_str().unwrap_or(""), a["host"].as_str().unwrap_or(""), a["user"].as_str().unwrap_or(""), a["status"].as_str().unwrap_or(""), a["message"].as_str().unwrap_or(""));
            }
        }
        "account-add" => {
            if args.len() < 3 {
                return Err("usage: fetchwing account-add <host> <user> <password|token>".into());
            }
            println!("{}", c.call("POST", "/accounts", Some(json!({"host": args[0], "user": args[1], "password": args[2]}))).await?);
        }
        "get" => {
            let r = c.call("GET", "/settings", None).await?;
            for (k, v) in r["values"].as_object().into_iter().flatten() {
                if args.first().is_none_or(|f| k.contains(f.as_str())) {
                    println!("{k} = {}", if k.contains("token") || k.contains("Key") { "••••".to_string() } else { v.to_string() });
                }
            }
        }
        "set" => {
            let k = args.first().ok_or("usage: fetchwing set <key> <value>")?;
            let raw = args[1..].join(" ");
            let v: Value = serde_json::from_str(&raw).unwrap_or(Value::String(raw));
            let r = c.call("PUT", "/settings", Some(json!({ k.as_str(): v }))).await?;
            println!("{}", if r["errors"].as_object().is_some_and(|e| !e.is_empty()) { r["errors"].to_string() } else { "ok".into() });
        }
        "log" => {
            for l in c.call("GET", "/log", None).await?.as_array().into_iter().flatten() {
                println!("{} {:<5} [{}] {}", l["t"], l["level"].as_str().unwrap_or(""), l["scope"].as_str().unwrap_or(""), l["msg"].as_str().unwrap_or(""));
            }
        }
        "update-ytdlp" => println!("{}", c.call("POST", "/tools/ytdlp", None).await?),
        other => return Err(format!("unknown command '{other}' (try --help)")),
    }
    Ok(())
}
