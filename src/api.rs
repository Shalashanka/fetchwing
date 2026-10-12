//! Local REST + Server-Sent-Events API and the embedded web UI.
//!
//! JD2 offers (a) MyJDownloader through the AppWork cloud relay and (b) a "deprecated" local API
//! on port 3128 without auth. Fetchwing: one local API used by the web UI, the CLI, the browser
//! extension and scripts. Security model:
//!   * token required for every /api call (the web UI gets it injected when loaded from loopback)
//!   * Host header must be an IP / localhost / *.local (blocks DNS-rebinding from web pages)
//!   * cross-origin browser requests are refused unless they come from a browser extension

use crate::core::{AddRequest, Core};
use crate::events::Event;
use crate::model::Status;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures_util::stream::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

type C = State<Arc<Core>>;
type R = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn err(code: StatusCode, msg: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({ "error": msg })))
}
fn ok(v: Value) -> R {
    Ok(Json(v))
}

const INDEX: &str = include_str!("../assets/web/index.html");

pub fn router(core: Arc<Core>) -> Router {
    let api = Router::new()
        .route("/state", get(state))
        .route("/events", get(events))
        .route("/links", post(add_links))
        .route("/jobs/{id}", get(job))
        .route("/links/action", post(link_action))
        .route("/packages/action", post(pkg_action))
        .route("/packages/{id}", patch(pkg_patch))
        .route("/control", post(control))
        .route("/cleanup", post(cleanup))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/plugins", get(plugins))
        .route("/plugins/reload", post(plugins_reload))
        .route("/plugins/match", get(plugin_match))
        .route("/accounts", get(accounts).post(add_account))
        .route("/accounts/{id}", patch(patch_account).delete(del_account))
        .route("/accounts/{id}/check", post(check_account))
        .route("/captchas", get(captchas))
        .route("/captchas/{id}", post(solve_captcha))
        .route("/captchas/{id}/image", get(captcha_image))
        .route("/log", get(log))
        .route("/tools/ytdlp", post(update_ytdlp))
        .route("/open", post(open_path))
        .layer(middleware::from_fn_with_state(core.clone(), auth));
    Router::new()
        .nest("/api", api)
        .route("/", get(index))
        .route("/index.html", get(index))
        .fallback(get(index))
        .layer(middleware::from_fn(host_guard))
        .with_state(core)
}

async fn host_guard(req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let h = host.rsplit_once(':').map(|x| x.0).unwrap_or(host).trim_matches(|c| c == '[' || c == ']');
    let ok = h == "localhost" || h.parse::<std::net::IpAddr>().is_ok() || h.ends_with(".local") || h.ends_with(".lan") || h.ends_with(".home.arpa") || h.ends_with(".ts.net");
    if !ok {
        return (StatusCode::FORBIDDEN, "Forbidden host").into_response();
    }
    next.run(req).await
}

async fn auth(State(core): C, req: Request, next: Next) -> Response {
    let origin = req.headers().get(header::ORIGIN).and_then(|h| h.to_str().ok()).map(str::to_string);
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
    let ext = origin.as_deref().is_some_and(|o| o.starts_with("chrome-extension://") || o.starts_with("moz-extension://") || o.starts_with("safari-web-extension://"));
    let same = origin.as_deref().is_none_or(|o| o == format!("http://{host}"));
    if !same && !ext {
        return (StatusCode::FORBIDDEN, "Cross-origin request refused").into_response();
    }
    let cors = |mut r: Response| {
        if ext {
            let h = r.headers_mut();
            h.insert("access-control-allow-origin", origin.clone().unwrap_or_default().parse().unwrap());
            h.insert("access-control-allow-headers", "authorization, content-type".parse().unwrap());
            h.insert("access-control-allow-methods", "GET, POST, PUT, PATCH, DELETE".parse().unwrap());
        }
        r
    };
    if req.method() == Method::OPTIONS {
        return cors(StatusCode::NO_CONTENT.into_response());
    }
    let token = core.settings.str("remote.token");
    let given = req.headers().get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).map(|s| s.trim_start_matches("Bearer ").trim().to_string())
        .or_else(|| req.uri().query().and_then(|q| url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "token").map(|(_, v)| v.into_owned())))
        .unwrap_or_default();
    if !ct_eq(given.as_bytes(), token.as_bytes()) {
        return cors((StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response());
    }
    cors(next.run(req).await)
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn index(State(core): C, ConnectInfo(addr): ConnectInfo<SocketAddr>) -> Html<String> {
    // inject the token only for loopback clients; remote clients must type it once
    let tok = if addr.ip().is_loopback() { format!("{:?}", core.settings.str("remote.token")) } else { "null".into() };
    Html(INDEX.replace("</head>", &format!("<script>window.__FW_TOKEN__={tok};</script></head>")))
}

async fn state(State(core): C) -> R {
    let (packages, links) = core.snapshot();
    let schema: Vec<Value> = crate::settings::schema().iter().map(|d| serde_json::to_value(d).unwrap_or_default()).collect();
    ok(json!({
        "packages": packages, "links": links, "stats": core.stats(), "captchas": core.captcha.list(),
        "accounts": core.accounts(), "settings": core.settings.all(), "schema": schema,
        "version": env!("CARGO_PKG_VERSION"), "sys": *core.sys.lock(), "notices": *core.notices.lock(),
    }))
}

async fn events(State(core): C) -> Sse<impl Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>> {
    let mut rx = core.events.subscribe();
    let stream = async_stream(move |tx| async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let (name, data) = match &ev {
                        Event::Log(l) if l.level == "debug" => continue,
                        e => {
                            let v = serde_json::to_value(e).unwrap_or_default();
                            (v["type"].as_str().unwrap_or("event").to_string(), v["data"].clone())
                        }
                    };
                    if tx.send(axum::response::sse::Event::default().event(name).data(data.to_string())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let _ = tx.send(axum::response::sse::Event::default().event("resync").data("{}")).await;
                }
                Err(_) => break,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// tiny channel-backed stream (avoids pulling in async-stream)
fn async_stream<F, Fut>(f: F) -> impl Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>
where
    F: FnOnce(tokio::sync::mpsc::Sender<axum::response::sse::Event>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    tokio::spawn(f(tx));
    futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|e| (Ok(e), rx)) })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddBody {
    text: String,
    package_name: Option<String>,
    dir: Option<String>,
    passwords: Option<Value>,
    auto_start: Option<bool>,
    source: Option<String>,
    origin: Option<String>,
    link_password: Option<String>,
}

async fn add_links(State(core): C, Json(b): Json<AddBody>) -> R {
    if b.text.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "text is empty"));
    }
    let passwords = match b.passwords {
        Some(Value::Array(a)) => a.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => s.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
        _ => vec![],
    };
    let id = core.add_links(AddRequest {
        text: b.text, package_name: b.package_name.filter(|s| !s.trim().is_empty()), dir: b.dir.filter(|s| !s.trim().is_empty()), passwords,
        auto_start: b.auto_start, source: b.source.unwrap_or("api".into()), origin: b.origin, link_password: b.link_password.filter(|s| !s.is_empty()),
    });
    ok(json!({ "jobId": id }))
}

async fn job(State(core): C, Path(id): Path<String>) -> R {
    core.job(&id).map(|j| Json(serde_json::to_value(j).unwrap_or_default())).ok_or_else(|| err(StatusCode::NOT_FOUND, "not found"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActionBody {
    action: String,
    ids: Vec<String>,
    value: Option<Value>,
    delete_files: Option<bool>,
}

async fn link_action(State(core): C, Json(b): Json<ActionBody>) -> R {
    match b.action.as_str() {
        "confirm" => return ok(json!({ "n": core.confirm(&b.ids) })),
        "force" => core.force_start(&b.ids),
        "pause" => core.pause_links(&b.ids),
        "resume" => core.resume_links(&b.ids),
        "reset" => core.reset_links(&b.ids),
        "remove" => core.remove_links(&b.ids, b.delete_files.unwrap_or(false)),
        "enable" => core.set_enabled(&b.ids, true),
        "disable" => core.set_enabled(&b.ids, false),
        "priority" => core.set_priority(&b.ids, b.value.and_then(|v| v.as_i64()).unwrap_or(0) as i32),
        "check" => core.queue_checks(b.ids.clone()),
        "password" => {
            for id in &b.ids {
                core.set_link_password(id, b.value.as_ref().and_then(|v| v.as_str()).unwrap_or(""));
            }
        }
        _ => return Err(err(StatusCode::BAD_REQUEST, "unknown action")),
    }
    ok(json!({ "ok": true }))
}

async fn pkg_action(State(core): C, Json(b): Json<ActionBody>) -> R {
    let links = core.links_of(&b.ids);
    match b.action.as_str() {
        "confirm" => return ok(json!({ "n": core.confirm(&links) })),
        "remove" => core.remove_packages(&b.ids, b.delete_files.unwrap_or(false)),
        "pause" => core.pause_links(&links),
        "resume" => core.resume_links(&links),
        "force" => core.force_start(&links),
        "reset" => core.reset_links(&links),
        "extract" => {
            for id in b.ids.clone() {
                let c = core.clone();
                tokio::spawn(async move { c.extract_package(&id).await });
            }
        }
        "priority" => {
            for id in &b.ids {
                let _ = core.update_package(id, None, None, None, b.value.as_ref().and_then(|v| v.as_i64()).map(|v| v as i32), None);
            }
        }
        _ => return Err(err(StatusCode::BAD_REQUEST, "unknown action")),
    }
    ok(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct PkgPatch {
    name: Option<String>,
    dir: Option<String>,
    passwords: Option<Vec<String>>,
    priority: Option<i32>,
    expanded: Option<bool>,
}
async fn pkg_patch(State(core): C, Path(id): Path<String>, Json(b): Json<PkgPatch>) -> R {
    core.update_package(&id, b.name, b.dir, b.passwords, b.priority, b.expanded).map_err(|e| err(StatusCode::BAD_REQUEST, &e))?;
    ok(json!({ "ok": true }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlBody {
    action: Option<String>,
    speed_limit: Option<u64>,
}
async fn control(State(core): C, Json(b): Json<ControlBody>) -> R {
    match b.action.as_deref() {
        Some("start") => core.start_all(),
        Some("pause") => core.pause_all(),
        _ => {}
    }
    if let Some(l) = b.speed_limit {
        core.set_speed_limit(l);
    }
    ok(serde_json::to_value(core.stats()).unwrap_or_default())
}

async fn cleanup(State(core): C, Json(b): Json<Value>) -> R {
    ok(json!({ "removed": core.cleanup(b["what"].as_str().unwrap_or("finished")) }))
}

async fn get_settings(State(core): C) -> R {
    let schema: Vec<Value> = crate::settings::schema().iter().map(|d| serde_json::to_value(d).unwrap_or_default()).collect();
    ok(json!({ "values": core.settings.all(), "schema": schema }))
}
async fn put_settings(State(core): C, Json(b): Json<HashMap<String, Value>>) -> R {
    let mut errors = serde_json::Map::new();
    for (k, v) in b {
        match core.settings.set(&k, v) {
            Ok(_) => core.settings_changed(&k),
            Err(e) => {
                errors.insert(k, json!(e));
            }
        }
    }
    ok(json!({ "values": core.settings.all(), "errors": errors }))
}

async fn plugins(State(core): C) -> R {
    let reg = core.registry.read().clone();
    let list: Vec<Value> = reg.all().iter().map(|p| {
        let m = p.meta();
        json!({ "id": m.id, "name": m.name, "version": m.version, "kind": if m.kind == crate::plugins::Kind::Host { "host" } else { "crawler" },
                "domains": m.domains, "priority": m.priority, "description": m.description, "source": m.source, "accountOnly": m.account_only, "multihost": p.is_multihost(), "login": p.has_login() })
    }).collect();
    let (x, y, k) = crate::plugins::templates::stats();
    ok(json!({ "plugins": list, "errors": reg.user_errors, "templates": { "xfs": x, "yetishare": y, "kvs": k }, "userDir": core.data_dir.join("plugins") }))
}
async fn plugins_reload(State(core): C) -> R {
    ok(json!({ "n": core.reload_plugins() }))
}
async fn plugin_match(State(core): C, Query(q): Query<HashMap<String, String>>) -> R {
    let u = q.get("url").cloned().unwrap_or_default();
    let reg = core.registry.read().clone();
    ok(json!({ "crawler": reg.find_crawler(&u).map(|p| p.meta().id.clone()), "host": reg.find_host(&u).map(|p| p.meta().id.clone()) }))
}

async fn accounts(State(core): C) -> R {
    ok(serde_json::to_value(core.accounts()).unwrap_or_default())
}
async fn add_account(State(core): C, Json(b): Json<Value>) -> R {
    let host = b["host"].as_str().unwrap_or("").to_string();
    if host.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "host required"));
    }
    ok(json!({ "id": core.add_account(&host, b["user"].as_str().unwrap_or(""), b["password"].as_str().unwrap_or("")) }))
}
async fn patch_account(State(core): C, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let pw = b["password"].as_str().map(|p| core.vault.encrypt(p));
    core.update_account(&id, |a| {
        if let Some(e) = b["enabled"].as_bool() {
            a.enabled = e;
        }
        if let Some(u) = b["user"].as_str() {
            a.user = u.into();
        }
        if let Some(p) = pw {
            a.secret = p;
            a.status = crate::model::AccountStatus::Unchecked;
        }
    });
    ok(json!({ "ok": true }))
}
async fn del_account(State(core): C, Path(id): Path<String>) -> R {
    core.remove_account(&id);
    ok(json!({ "ok": true }))
}
async fn check_account(State(core): C, Path(id): Path<String>) -> R {
    core.check_account(&id).await;
    ok(serde_json::to_value(core.accounts().into_iter().find(|a| a.id == id)).unwrap_or_default())
}

async fn captchas(State(core): C) -> R {
    ok(serde_json::to_value(core.captcha.list()).unwrap_or_default())
}
async fn solve_captcha(State(core): C, Path(id): Path<String>, Json(b): Json<Value>) -> R {
    let done = if b["skip"].as_bool() == Some(true) { core.captcha.skip(&id) } else { core.captcha.submit(&id, b["value"].as_str().unwrap_or("")) };
    if done { ok(json!({ "ok": true })) } else { Err(err(StatusCode::NOT_FOUND, "not found")) }
}
async fn captcha_image(State(core): C, Path(id): Path<String>) -> Response {
    match core.captcha.list().into_iter().find(|j| j.id == id).and_then(|j| j.request.image_bytes()) {
        Some(b) => Response::builder().header(header::CONTENT_TYPE, "image/png").body(Body::from(b)).unwrap(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn log() -> R {
    ok(serde_json::to_value(crate::log::recent()).unwrap_or_default())
}

async fn update_ytdlp(State(core): C) -> R {
    core.refresh_tools(true).await;
    ok(serde_json::to_value(&*core.sys.lock()).unwrap_or_default())
}

async fn open_path(State(core): C, ConnectInfo(addr): ConnectInfo<SocketAddr>, Json(b): Json<Value>) -> R {
    if !addr.ip().is_loopback() {
        return Err(err(StatusCode::FORBIDDEN, "local only"));
    }
    let p = b["path"].as_str().unwrap_or("").to_string();
    let known = {
        let st = core.state.lock();
        st.links.values().any(|l| l.prop_str("finalPath") == Some(p.as_str())) || st.packages.values().any(|x| x.dir == p)
    };
    if !known {
        return Err(err(StatusCode::FORBIDDEN, "not a Fetchwing path"));
    }
    reveal(std::path::Path::new(&p));
    ok(json!({ "ok": true }))
}

/// Show a file in the OS file manager (or open a folder).
pub fn reveal(p: &std::path::Path) {
    #[cfg(windows)]
    {
        if p.is_file() {
            let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.display())).spawn();
            return;
        }
    }
    let target = if p.is_file() { p.parent().unwrap_or(p) } else { p };
    let _ = open::that_detached(target);
}

pub fn count_status(core: &Core, s: Status) -> usize {
    core.state.lock().links.values().filter(|l| l.status == s).count()
}

pub async fn serve(core: Arc<Core>) -> Result<String, String> {
    let bind = core.settings.str("remote.bind");
    let port = core.settings.u64("remote.port") as u16;
    let addr: SocketAddr = format!("{bind}:{port}").parse().map_err(|e| format!("bad listen address: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("cannot listen on {addr}: {e}"))?;
    let url = format!("http://{}:{port}/", if bind == "0.0.0.0" { "127.0.0.1" } else { bind.as_str() });
    crate::log::info("api", &format!("web UI + API on {url}"));
    let app = router(core);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
    });
    Ok(url)
}

pub fn _unused(_: HeaderMap) {}
