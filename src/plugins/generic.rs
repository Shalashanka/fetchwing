//! Direct HTTP links (any web server). Lowest priority fallback.

use super::*;
use crate::util::http::{filename_from_disposition, filename_from_url};

pub struct Generic {
    meta: Meta,
}
impl Generic {
    pub fn new() -> Self {
        Generic { meta: Meta::host("generic-http", "Direct HTTP", &[], &[r"(?i)^https?://"]).prio(-100).desc("Direct file links. Web pages are scanned for links by the crawler instead.") }
    }
}

/// Neutral UA used when a server drops our browser UA: some CDNs/bot filters close connections
/// whose UA claims to be Chrome while the TLS handshake (JA3/JA4) clearly is not (seen live on
/// Hetzner's speed-test CDN).
pub const HONEST_UA: &str = concat!("Fetchwing/", env!("CARGO_PKG_VERSION"));

pub struct Probe {
    pub status: u16,
    pub url: String,
    pub name: String,
    pub size: i64,
    pub content_type: String,
    pub is_html: bool,
    pub resumable: bool,
}

/// GET with Range: bytes=0-0 tells size, name, type and range support (HEAD is often broken).
pub async fn probe(b: &Browser, url: &str) -> Result<Probe, HttpErr> {
    let r = b.open(url, &[("range", "bytes=0-0".into())], "GET", None).await?;
    let status = r.status().as_u16();
    let final_url = r.url().to_string();
    let h = |k: &str| r.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    let ct = h("content-type").unwrap_or_default();
    let mut size = -1i64;
    if status == 206 {
        if let Some(cr) = h("content-range") {
            size = cr.rsplit('/').next().and_then(|s| s.parse().ok()).unwrap_or(-1);
        }
    } else if let Some(cl) = h("content-length") {
        size = cl.parse().unwrap_or(-1);
    }
    let name = filename_from_disposition(h("content-disposition").as_deref()).unwrap_or_else(|| filename_from_url(&final_url));
    let ctl = ct.to_ascii_lowercase();
    drop(r);
    Ok(Probe { status, url: final_url, name, size, is_html: ctl.contains("text/html") || ctl.contains("xhtml"), content_type: ct, resumable: status == 206 })
}

/// Headers captured from the browser ("Copy as cURL"): cookie, referer, user-agent, ...
pub fn stored_headers(props: &Map<String, Value>) -> Vec<(String, String)> {
    props.get("headers").and_then(|h| h.as_object()).map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect()).unwrap_or_default()
}

pub async fn probe_with_fallback(b: &mut Browser, url: &str, props: &Arc<Mutex<Map<String, Value>>>) -> Result<Probe, HttpErr> {
    let hs = stored_headers(&props.lock());
    if !hs.is_empty() {
        b.extra.extend(hs);
        return probe(b, url).await; // the browser's own headers are authoritative
    }
    let stored = props.lock().get("ua").and_then(|v| v.as_str()).map(str::to_string);
    if let Some(ua) = stored {
        b.extra.push(("user-agent".into(), ua));
        return probe(b, url).await;
    }
    match probe(b, url).await {
        Ok(p) => Ok(p),
        Err(e) if e.status == 0 => {
            b.extra.push(("user-agent".into(), HONEST_UA.into()));
            let p = probe(b, url).await?;
            props.lock().insert("ua".into(), Value::String(HONEST_UA.into()));
            Ok(p)
        }
        Err(e) => Err(e),
    }
}

#[async_trait]
impl Plugin for Generic {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let url = ctx.url.clone();
        let p = probe_with_fallback(&mut ctx.browser, &url, &ctx.props).await?;
        if p.status >= 400 {
            return Ok(Some(if p.status == 404 || p.status == 410 { LinkInfo::offline() } else { LinkInfo { name: Some(filename_from_url(&url)), ..LinkInfo::unknown() } }));
        }
        Ok(Some(LinkInfo::online(Some(p.name), (p.size > 0).then_some(p.size))))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let mut req = DirectRequest::get(ctx.url.clone());
        if let Some(ua) = ctx.prop("ua").and_then(|v| v.as_str().map(str::to_string)) {
            req = req.header("user-agent", ua);
        }
        let captured = stored_headers(&ctx.props.lock());
        for (k, v) in captured {
            req = req.header(&k, v);
        }
        Ok(Resolution::http(req))
    }
}
