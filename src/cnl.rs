//! Click'n'Load v1/v2 server (127.0.0.1:9666), protocol-compatible with JD2
//! (org.jdownloader.api.cnl2.ExternInterfaceImpl + decrypter DummyCNL):
//!   GET  /jdcheck.js  -> "jdownloader=true;"  (link-protector sites probe this)
//!   GET  /crossdomain.xml, GET /flash/ -> "JDownloader"
//!   POST /flash/add          urls=..&passwords=..&source=..&package=..
//!   POST /flash/addcrypted2  crypted=<b64 AES-128-CBC>&jk=<js: function f(){return 'hex'}>
//! Security differences from JD2: JD2 evaluates the site-supplied `jk` JavaScript in Rhino;
//! Fetchwing never executes it - it extracts the hex key from the usual `return '...'` patterns.
//! CNL links always land in the Inbox (no auto-start) and keep their source page.

use crate::core::{AddRequest, Core};
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use std::collections::HashMap;
use std::sync::Arc;

pub fn decode_jk(jk: &str) -> Option<Vec<u8>> {
    let body = crate::util::http::re(r"return\s*([^;}]+)").captures(jk).map(|c| c[1].to_string()).unwrap_or_else(|| jk.to_string());
    let hex: String = crate::util::http::re(r#"['"]([0-9a-fA-F]*)['"]"#).captures_iter(&body).map(|c| c[1].to_string()).collect();
    if hex.len() != 32 {
        return None;
    }
    hex::decode(hex).ok()
}

pub fn decrypt(crypted: &str, key: &[u8]) -> Option<String> {
    let data = base64::engine::general_purpose::STANDARD.decode(crypted.trim().replace(' ', "+")).ok()?;
    let mut buf = data.clone();
    buf.truncate(buf.len() / 16 * 16);
    let d = cbc::Decryptor::<aes::Aes128>::new_from_slices(key, key).ok()?;
    let out = d.decrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut buf).ok()?;
    Some(String::from_utf8_lossy(out).trim_end_matches('\0').trim().to_string())
}

fn text(body: &str, ty: &str) -> Response {
    ([(header::CONTENT_TYPE, ty), (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"), (header::CACHE_CONTROL, "no-store")], body.to_string()).into_response()
}

pub async fn serve(core: Arc<Core>) -> bool {
    let app = Router::new()
        .route("/jdcheck.js", get(|| async { text("jdownloader=true;\r\nvar version='fetchwing';\r\n", "text/javascript") }))
        .route("/crossdomain.xml", get(|| async { text("<?xml version=\"1.0\"?>\r\n<!DOCTYPE cross-domain-policy SYSTEM \"http://www.macromedia.com/xml/dtds/cross-domain-policy.dtd\">\r\n<cross-domain-policy>\r\n<allow-access-from domain=\"*\" />\r\n</cross-domain-policy>\r\n", "text/xml") }))
        .route("/flash", get(|| async { text("JDownloader\r\n", "text/html") }))
        .route("/flash/", get(|| async { text("JDownloader\r\n", "text/html") }))
        .route("/flash/add", post(add))
        .route("/flash/addcrypted2", post(add))
        .with_state(core);
    match tokio::net::TcpListener::bind("127.0.0.1:9666").await {
        Ok(l) => {
            crate::log::info("cnl", "Click'n'Load listening on 127.0.0.1:9666");
            tokio::spawn(async move { let _ = axum::serve(l, app).await; });
            true
        }
        Err(e) => {
            crate::log::warn("cnl", &format!("Click'n'Load disabled: port 9666 {}", if e.kind() == std::io::ErrorKind::AddrInUse { "is in use (JDownloader running?)".to_string() } else { e.to_string() }));
            false
        }
    }
}

async fn add(State(core): State<Arc<Core>>, uri: axum::http::Uri, headers: HeaderMap, Query(q): Query<HashMap<String, String>>, body: Bytes) -> Response {
    let mut p: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    for (k, v) in q {
        p.entry(k).or_insert(v);
    }
    let source = p.get("source").cloned().or_else(|| headers.get(header::REFERER).and_then(|v| v.to_str().ok()).map(str::to_string));
    let passwords: Vec<String> = p.get("passwords").map(|s| s.lines().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()).unwrap_or_default();
    let urls = if uri.path().ends_with("addcrypted2") {
        let key = match p.get("k") {
            Some(k) => hex::decode(k).ok(),
            None => p.get("jk").and_then(|jk| decode_jk(jk)),
        };
        match (p.get("crypted"), key) {
            (Some(c), Some(k)) if k.len() == 16 => decrypt(c, &k).unwrap_or_default(),
            _ => return text("failed\r\n", "text/html"),
        }
    } else {
        p.get("urls").cloned().unwrap_or_default()
    };
    if urls.trim().is_empty() {
        return text("failed\r\n", "text/html");
    }
    crate::log::info("cnl", &format!("Click'n'Load from {}: {} link(s)", source.as_deref().unwrap_or("unknown"), urls.split_whitespace().count()));
    core.add_links(AddRequest { text: urls, package_name: p.get("package").cloned(), dir: None, passwords, auto_start: Some(false), source: "cnl".into(), origin: source, link_password: None });
    let _ = StatusCode::OK;
    text("success\r\n", "text/html")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;
    #[test]
    fn cnl_roundtrip() {
        let key = hex::decode("31323334353637383930393837363534").unwrap();
        let jk = "function f(){ return '31323334353637383930393837363534';}";
        assert_eq!(decode_jk(jk).unwrap(), key);
        let plain = b"https://a.com/x\r\nhttps://b.com/y";
        let mut buf = plain.to_vec();
        buf.resize(plain.len().div_ceil(16) * 16, 0);
        let n = buf.len();
        let e = cbc::Encryptor::<aes::Aes128>::new_from_slices(&key, &key).unwrap();
        e.encrypt_padded_mut::<cbc::cipher::block_padding::NoPadding>(&mut buf, n).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
        assert_eq!(decrypt(&b64, &key).unwrap(), "https://a.com/x\r\nhttps://b.com/y");
        assert!(decode_jk("function f(){ return '3132'+'333435363738393039383736353'+'4';}").is_some());
    }
}
