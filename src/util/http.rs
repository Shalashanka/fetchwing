//! Fetchwing's equivalent of JD2's `jd.http.Browser`: a session with a per-host cookie jar,
//! redirect control, HTML form parsing and regex helpers. Built on reqwest + rustls.

use parking_lot::Mutex;
use regex::Regex;
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Response};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Cached regex compile (patterns are literals all over the plugins).
pub fn re(pat: &str) -> Regex {
    static CACHE: OnceLock<Mutex<HashMap<String, Regex>>> = OnceLock::new();
    let c = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(r) = c.lock().get(pat) {
        return r.clone();
    }
    let r = Regex::new(pat).unwrap_or_else(|e| panic!("bad regex {pat}: {e}"));
    c.lock().insert(pat.to_string(), r.clone());
    r
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NetOpts {
    pub user_agent: String,
    pub timeout_s: u64,
    pub proxy: String,
}

struct Clients {
    follow: Client,
    nofollow: Client,
}

/// Clients are cached per (host jar, net options); reqwest binds the cookie store at build time.
fn clients(jar: &Arc<Jar>, o: &NetOpts, key: &str) -> Arc<Clients> {
    static CACHE: OnceLock<Mutex<HashMap<(String, NetOpts), Arc<Clients>>>> = OnceLock::new();
    let c = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let k = (key.to_string(), o.clone());
    if let Some(x) = c.lock().get(&k) {
        return x.clone();
    }
    let build = |follow: bool| {
        let mut b = Client::builder()
            .cookie_provider(jar.clone())
            .user_agent(o.user_agent.clone())
            .connect_timeout(Duration::from_secs(o.timeout_s))
            .read_timeout(Duration::from_secs(o.timeout_s))
            .pool_max_idle_per_host(16)
            // HTTP/1.1 only: with HTTP/2 every "parallel" segment is multiplexed over ONE TCP
            // connection (no speed gain, and Cloudflare tunnels were seen stalling on it).
            // Download managers need separate connections.
            .http1_only()
            .redirect(if follow { reqwest::redirect::Policy::limited(10) } else { reqwest::redirect::Policy::none() });
        if !o.proxy.trim().is_empty() {
            if let Ok(p) = reqwest::Proxy::all(o.proxy.trim()) {
                b = b.proxy(p);
            }
        }
        b.build().expect("http client")
    };
    let x = Arc::new(Clients { follow: build(true), nofollow: build(false) });
    c.lock().insert(k, x.clone());
    x
}

pub fn reset_clients() {
    // net options are part of the cache key, so changed settings create new clients automatically.
}

#[derive(Clone)]
pub struct Browser {
    pub jar: Arc<Jar>,
    jar_key: String,
    pub opts: NetOpts,
    pub extra: Vec<(String, String)>,
    pub referer: Option<String>,
}

#[derive(Default, Clone)]
pub struct Req<'a> {
    pub headers: Vec<(&'a str, String)>,
    pub allow_errors: bool,
    pub no_follow: bool,
}

#[derive(Debug, Clone)]
pub struct HttpErr {
    pub status: u16,
    pub msg: String,
}
impl std::fmt::Display for HttpErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.status > 0 {
            write!(f, "HTTP {}: {}", self.status, self.msg)
        } else {
            write!(f, "{}", self.msg)
        }
    }
}
impl From<reqwest::Error> for HttpErr {
    fn from(e: reqwest::Error) -> Self {
        let mut msg = e.to_string();
        let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
        while let Some(s) = src {
            msg = format!("{msg}: {s}");
            src = s.source();
        }
        HttpErr { status: e.status().map(|s| s.as_u16()).unwrap_or(0), msg }
    }
}

impl Browser {
    pub fn new(jar: Arc<Jar>, jar_key: &str, opts: NetOpts) -> Self {
        Browser { jar, jar_key: jar_key.to_string(), opts, extra: vec![], referer: None }
    }
    fn cl(&self) -> Arc<Clients> {
        clients(&self.jar, &self.opts, &self.jar_key)
    }

    pub fn set_cookie(&self, domain: &str, name: &str, value: &str) {
        if let Ok(u) = url::Url::parse(&format!("https://{domain}/")) {
            self.jar.add_cookie_str(&format!("{name}={value}; Domain={domain}; Path=/"), &u);
        }
    }
    pub fn cookie(&self, url: &str, name: &str) -> Option<String> {
        let u = url::Url::parse(url).ok()?;
        let h = self.jar.cookies(&u)?;
        h.to_str().ok()?.split(';').map(str::trim).find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(str::to_string))
    }

    fn headers(&self, req: &Req) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("accept", HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"));
        h.insert("accept-language", HeaderValue::from_static("en-US,en;q=0.9"));
        if let Some(r) = &self.referer {
            if let Ok(v) = HeaderValue::from_str(r) {
                h.insert("referer", v);
            }
        }
        for (k, v) in self.extra.iter().map(|(k, v)| (k.as_str(), v.clone())).chain(req.headers.iter().map(|(k, v)| (*k, v.clone()))) {
            if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(&v)) {
                h.insert(n, v);
            }
        }
        h
    }

    pub async fn get(&mut self, url: &str) -> Result<Page, HttpErr> {
        self.request("GET", url, None, &Req::default()).await
    }
    pub async fn get_with(&mut self, url: &str, r: &Req<'_>) -> Result<Page, HttpErr> {
        self.request("GET", url, None, r).await
    }
    pub async fn post_form(&mut self, url: &str, form: &[(String, String)], r: &Req<'_>) -> Result<Page, HttpErr> {
        let body = form_urlencode(form);
        let mut r = r.clone();
        r.headers.push(("content-type", "application/x-www-form-urlencoded".into()));
        self.request("POST", url, Some(body.into_bytes()), &r).await
    }
    pub async fn post_json(&mut self, url: &str, json: &serde_json::Value, r: &Req<'_>) -> Result<Page, HttpErr> {
        let mut r = r.clone();
        r.headers.push(("content-type", "application/json".into()));
        r.headers.push(("accept", "application/json, */*".into()));
        self.request("POST", url, Some(json.to_string().into_bytes()), &r).await
    }

    pub async fn request(&mut self, method: &str, url: &str, body: Option<Vec<u8>>, r: &Req<'_>) -> Result<Page, HttpErr> {
        let cl = self.cl();
        let client = if r.no_follow { &cl.nofollow } else { &cl.follow };
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
        let mut rb = client.request(m, url).headers(self.headers(r));
        if let Some(b) = body {
            rb = rb.body(b);
        }
        let resp = rb.send().await?;
        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let headers = resp.headers().clone();
        let text = resp.text().await.unwrap_or_default();
        self.referer = Some(final_url.clone());
        if status >= 400 && !r.allow_errors {
            return Err(HttpErr { status, msg: final_url });
        }
        Ok(Page { url: final_url, status, headers, text })
    }

    /// Streaming response for downloads (follows redirects).
    pub async fn open(&self, url: &str, headers: &[(&str, String)], method: &str, body: Option<String>) -> Result<Response, HttpErr> {
        let cl = self.cl();
        let mut h = self.headers(&Req { headers: headers.to_vec(), ..Default::default() });
        h.insert("accept", HeaderValue::from_static("*/*"));
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
        let mut rb = cl.follow.request(m, url).headers(h);
        if let Some(b) = body {
            rb = rb.body(b);
        }
        Ok(rb.send().await?)
    }
}

pub fn form_urlencode(form: &[(String, String)]) -> String {
    let enc = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
    form.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

pub struct Page {
    pub url: String,
    pub status: u16,
    pub headers: HeaderMap,
    pub text: String,
}

impl Page {
    /// First capture group (or whole match) of `pat`.
    pub fn m(&self, pat: &str) -> Option<String> {
        let c = re(pat).captures(&self.text)?;
        Some(c.get(1).or_else(|| c.get(0))?.as_str().to_string())
    }
    pub fn has(&self, pat: &str) -> bool {
        re(pat).is_match(&self.text)
    }
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(&self.text).map_err(|e| format!("invalid JSON from {}: {e}", self.url))
    }
    pub fn header(&self, k: &str) -> Option<String> {
        self.headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string)
    }
    pub fn location(&self) -> Option<String> {
        let l = self.header("location")?;
        url::Url::parse(&self.url).ok()?.join(&l).ok().map(|u| u.to_string())
    }
    pub fn forms(&self) -> Vec<Form> {
        parse_forms(&self.text, &self.url)
    }
}

#[derive(Debug, Clone)]
pub struct Form {
    pub action: String,
    pub method: String,
    pub id: Option<String>,
    pub html: String,
    pub fields: Vec<(String, String)>,
}
impl Form {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.fields.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
    pub fn set(&mut self, k: &str, v: &str) {
        if let Some(f) = self.fields.iter_mut().find(|(a, _)| a == k) {
            f.1 = v.to_string();
        } else {
            self.fields.push((k.to_string(), v.to_string()));
        }
    }
    pub fn remove(&mut self, k: &str) {
        self.fields.retain(|(a, _)| a != k);
    }
}

pub fn parse_forms(html: &str, base: &str) -> Vec<Form> {
    let mut out = vec![];
    for c in re(r"(?is)<form\b([^>]*)>(.*?)</form>").captures_iter(html) {
        let attrs = parse_attrs(&c[1]);
        let mut fields: Vec<(String, String)> = vec![];
        for i in re(r"(?is)<(?:input|button|select|textarea)\b([^>]*)>").captures_iter(&c[2]) {
            let a = parse_attrs(&i[1]);
            let Some(name) = a.get("name") else { continue };
            let ty = a.get("type").map(|s| s.to_ascii_lowercase()).unwrap_or_default();
            if (ty == "checkbox" || ty == "radio") && !a.contains_key("checked") {
                continue;
            }
            if ty == "submit" && fields.iter().any(|(k, _)| k == name) {
                continue;
            }
            fields.push((name.clone(), decode_entities(a.get("value").map(String::as_str).unwrap_or(""))));
        }
        let action = attrs.get("action").map(|a| decode_entities(a)).unwrap_or_default();
        let action = url::Url::parse(base).and_then(|b| b.join(&action)).map(|u| u.to_string()).unwrap_or_else(|_| base.to_string());
        out.push(Form {
            action,
            method: attrs.get("method").map(|m| m.to_ascii_uppercase()).unwrap_or_else(|| "GET".into()),
            id: attrs.get("id").cloned(),
            html: c[0].to_string(),
            fields,
        });
    }
    out
}

fn parse_attrs(s: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for c in re(r#"([a-zA-Z_:][-a-zA-Z0-9_:.]*)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?"#).captures_iter(s) {
        let v = c.get(2).or(c.get(3)).or(c.get(4)).map(|m| m.as_str().to_string()).unwrap_or_default();
        m.insert(c[1].to_ascii_lowercase(), v);
    }
    m
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    re(r"(?i)&(#x[0-9a-f]+|#\d+|amp|lt|gt|quot|apos|nbsp);")
        .replace_all(s, |c: &regex::Captures| {
            let e = c[1].to_ascii_lowercase();
            match e.as_str() {
                "amp" => "&".into(),
                "lt" => "<".into(),
                "gt" => ">".into(),
                "quot" => "\"".into(),
                "apos" => "'".into(),
                "nbsp" => " ".into(),
                _ if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32).map(String::from).unwrap_or_default(),
                _ => e[1..].parse::<u32>().ok().and_then(char::from_u32).map(String::from).unwrap_or_default(),
            }
        })
        .into_owned()
}

/// RFC 6266 filename from Content-Disposition (handles filename*=UTF-8''...).
pub fn filename_from_disposition(cd: Option<&str>) -> Option<String> {
    let cd = cd?;
    if let Some(c) = re(r#"(?i)filename\*\s*=\s*[^']*'[^']*'([^;]+)"#).captures(cd) {
        let v = c[1].trim().trim_matches('"');
        if let Ok(d) = percent_encoding::percent_decode_str(v).decode_utf8() {
            return Some(d.into_owned());
        }
    }
    let c = re(r#"(?i)filename\s*=\s*("([^"]*)"|[^;]+)"#).captures(cd)?;
    let raw = c.get(2).map(|m| m.as_str()).unwrap_or(&c[1]).trim().to_string();
    // servers often send raw UTF-8 bytes interpreted as latin1
    let bytes: Option<Vec<u8>> = raw.chars().map(|ch| u8::try_from(ch as u32).ok()).collect();
    if let Some(b) = bytes {
        if let Ok(s) = String::from_utf8(b) {
            return Some(s);
        }
    }
    Some(raw)
}

pub fn filename_from_url(u: &str) -> String {
    url::Url::parse(u)
        .ok()
        .and_then(|x| x.path_segments().and_then(|s| s.filter(|p| !p.is_empty()).last().map(str::to_string)))
        .map(|s| percent_encoding::percent_decode_str(&s).decode_utf8_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download".into())
}

pub fn host_of(u: &str) -> String {
    url::Url::parse(u).ok().and_then(|x| x.host_str().map(|h| h.trim_start_matches("www.").to_string())).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disposition() {
        assert_eq!(filename_from_disposition(Some("attachment; filename=\"a b.zip\"")).unwrap(), "a b.zip");
        assert_eq!(filename_from_disposition(Some("attachment; filename*=UTF-8''%C3%A4.txt")).unwrap(), "ä.txt");
    }
    #[test]
    fn forms() {
        let f = parse_forms(r#"<form method="post" action="/x"><input type="hidden" name="op" value="download1"><input name="id" value="abc"><input type="submit" name="method_free" value="Free"></form>"#, "https://h.com/a");
        assert_eq!(f[0].action, "https://h.com/x");
        assert_eq!(f[0].get("op"), Some("download1"));
        assert_eq!(f[0].get("method_free"), Some("Free"));
    }
}
