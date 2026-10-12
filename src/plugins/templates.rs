//! Declarative template families.
//!
//! assets/template-sites.json is generated from the decompiled JD2 plugins by
//! tools/extract_templates.py: every subclass of XFileSharingProBasic / YetiShareCore /
//! KernelVideoSharingComV2 becomes {id, domains, maxChunks, resumable, maxDownloads, custom[]}.
//! One generic engine per family replaces ~450 Java classes.
//!
//! Users can add sites without recompiling: drop JSON files into <data>/plugins/, e.g.
//!   [{"type":"xfs","id":"myhost","domains":["myhost.com"]},
//!    {"type":"regex","id":"simple","domains":["files.example.org"],
//!     "pattern":"^https?://files\\.example\\.org/f/\\w+","linkRegex":"href=\"(https://cdn[^\"]+)\"",
//!     "nameRegex":"<h1>([^<]+)</h1>","offlineRegex":"File not found","waitRegex":"var wait = (\\d+)"}]

use super::ytdlp::Ytdlp;
use super::*;
use crate::util::http::{decode_entities, Form};
use serde::Deserialize;

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SiteDef {
    pub id: String,
    pub domains: Vec<String>,
    #[serde(default)]
    pub max_chunks: Option<i64>,
    #[serde(default)]
    pub resumable: Option<bool>,
    #[serde(default)]
    pub max_downloads: Option<i64>,
    #[serde(default)]
    pub custom: Vec<String>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct Families {
    XFileSharingProBasic: Vec<SiteDef>,
    YetiShareCore: Vec<SiteDef>,
    KernelVideoSharingComV2: Vec<SiteDef>,
}

const DATA: &str = include_str!("../../assets/template-sites.json");

pub fn stats() -> (usize, usize, usize) {
    let f: Families = serde_json::from_str(DATA).expect("template data");
    (f.XFileSharingProBasic.len(), f.YetiShareCore.len(), f.KernelVideoSharingComV2.len())
}

pub fn builtin() -> Vec<P> {
    let f: Families = serde_json::from_str(DATA).expect("template data");
    let mut v: Vec<P> = vec![];
    for s in f.XFileSharingProBasic {
        v.push(Arc::new(Xfs::new(&s, "builtin")));
    }
    for s in f.YetiShareCore {
        v.push(Arc::new(YetiShare::new(&s, "builtin")));
    }
    for s in f.KernelVideoSharingComV2 {
        v.push(Arc::new(kvs(&s)));
    }
    v
}

fn host_re(domains: &[String]) -> String {
    format!(r"(?:www\.)?(?:{})", domains.iter().map(|d| regex::escape(d)).collect::<Vec<_>>().join("|"))
}
fn chunks(n: Option<i64>) -> Option<usize> {
    match n {
        None | Some(0) => None,
        Some(n) => Some(n.unsigned_abs() as usize),
    }
}
fn limits(s: &SiteDef) -> Limits {
    Limits { max_conns: if s.resumable == Some(false) { Some(1) } else { chunks(s.max_chunks) }, max_downloads: s.max_downloads.filter(|n| *n > 0).map(|n| n as usize), ..Default::default() }
}

fn kvs(s: &SiteDef) -> Ytdlp {
    let pat = format!(r"(?i)^https?://{}/(?:videos?|embed|get_file)/\S+", host_re(&s.domains));
    let mut y = Ytdlp::new(&format!("kvs-{}", s.id.to_lowercase()), &s.domains[0], s.domains.clone(), &pat, -6);
    let _ = &mut y;
    y
}

// ============================================================================================
// XFileSharingPro
// ============================================================================================

pub struct Xfs {
    meta: Meta,
    site: SiteDef,
}

const XFS_OFFLINE: &str = r"(?i)>\s*File Not Found\s*<|file was removed|No such file|File has been removed|The file you were looking for could not be found|The file was deleted|file is no longer available|>\s*404 Not Found";

impl Xfs {
    pub fn new(s: &SiteDef, source: &'static str) -> Self {
        let pat = format!(r"(?i)^https?://{}/(?:embed-|d/|e/|f/)?[a-z0-9]{{12}}(?:[/.?#]|$)", host_re(&s.domains));
        let mut meta = Meta::host(&format!("xfs-{}", s.id.to_lowercase()), &s.domains[0], &[], &[pat.as_str()]).prio(-5).limits(limits(s));
        meta.domains = s.domains.clone();
        meta.version = "1.0.0-template".into();
        meta.source = source;
        meta.description = if s.custom.is_empty() { "XFileSharingPro template".into() } else { format!("XFileSharingPro template (best effort: JD2 customised {})", s.custom.join(", ")) };
        Xfs { meta, site: s.clone() }
    }
}

fn fuid(url: &str) -> Option<String> {
    let path = url::Url::parse(url).ok()?.path().to_string() + "/";
    cap(r"(?i)/(?:embed-|d/|e/|f/)?([a-z0-9]{12})(?:[/.?#]|$)", &path).map(|s| s.to_lowercase())
}

fn scan_info(p: &Page) -> (Option<String>, Option<i64>) {
    let name = p.m(r#"link="https?://[^/"]+/[a-z0-9]{12}/([^"/]+)""#)
        .map(|n| percent_encoding::percent_decode_str(&n).decode_utf8_lossy().into_owned())
        .or_else(|| p.m(r#"(?i)name="fname"\s+value="([^"]+)""#))
        .or_else(|| p.m(r#"(?is)<h\d[^>]*class="[^"]*(?:file-?name|title)[^"]*"[^>]*>\s*(?:<[^>]+>\s*)*([^<]{3,250}?)\s*<"#))
        .or_else(|| p.m(r"(?is)(?:Filename|File Name)\s*:?\s*</?[^>]*>\s*(?:<[^>]+>\s*)*([^<]{3,250}?)\s*<"))
        .or_else(|| p.m(r"(?is)<title>\s*(?:Download\s+)?([^<]+?\.[a-z0-9]{2,5})\s*(?:-[^<]*)?</title>"));
    let size = p.m(r"(?i)\(\s*([\d.,]+\s*(?:B|KB|MB|GB|TB|bytes))\s*\)")
        .or_else(|| p.m(r"(?is)(?:Size|File size)\s*:?\s*</?[^>]*>\s*(?:<[^>]+>\s*)*([\d.,]+\s*(?:B|KB|MB|GB|TB))"))
        .and_then(|s| crate::util::parse_bytes(&s.replace("bytes", "B")))
        .map(|s| s as i64);
    (name.map(|n| decode_entities(n.trim())), size)
}

fn xfs_errors(p: &Page) -> PResult<()> {
    if p.has(XFS_OFFLINE) {
        return Err(offline("File not found"));
    }
    if let Some(c) = crate::util::http::re(r"(?i)You have to wait\s*(?:(\d+)\s*hours?)?[,\s]*(?:(\d+)\s*minutes?)?[,\s]*(?:(\d+)\s*seconds?)?").captures(&p.text) {
        let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u64>().ok()).unwrap_or(0);
        let ms = ((n(1) * 60 + n(2)) * 60 + n(3)) * 1000;
        if ms > 0 {
            return Err(hoster_wait(ms.max(30_000), "Hoster wait between free downloads"));
        }
    }
    if p.has(r"(?i)You have reached the download[- ]limit|download limit (?:has been )?reached|exceeded your download limit") {
        return Err(hoster_wait(3_600_000, "Free download limit reached"));
    }
    if p.has(r"(?i)available for Premium Users only|only premium members can download|Max file size for free users|This file reached max downloads limit|upgrade your account to download") {
        return Err(premium_only("Only downloadable with a premium account"));
    }
    if p.has(r"(?i)>\s*Wrong captcha\s*<") {
        return Err(PluginError::new(ErrCode::CaptchaFailed, "Wrong captcha"));
    }
    if p.has(r"(?i)Wrong password") {
        return Err(PluginError::new(ErrCode::Password, "Wrong download password"));
    }
    if p.has(r"(?i)Server is currently overloaded|Server under maintenance|no free download slots") {
        return Err(hoster_wait(600_000, "Hoster busy"));
    }
    Ok(())
}

fn xfs_direct(p: &Page, fuid: &str) -> Option<String> {
    if (300..400).contains(&p.status) {
        if let Some(l) = p.location() {
            return Some(l);
        }
    }
    // XHR endpoints answer JSON with escaped slashes
    let unescaped;
    let p = if p.text.contains("\\/") {
        unescaped = Page { url: p.url.clone(), status: p.status, headers: p.headers.clone(), text: p.text.replace("\\/", "/") };
        &unescaped
    } else {
        p
    };
    let pats = [
        r#"(https?://[^"'<>\s\\]+/download/[A-Za-z0-9_-]{30,}(?:\?[^"'<>\s\\]*)?)"#,
        r#"(https?://[^"'<>\s]+/(?:d|files|cgi-bin/dl\.cgi|dl)/[^"'<>\s]+/[^"'<>\s]+)"#,
        r#"(?is)href="(https?://[^"]+)"[^>]*>\s*(?:<[^>]+>\s*)*(?:Direct Download Link|Click here to download|Download File)"#,
        r#"(?i)<a[^>]+id="(?:downloadbtn|direct_link|dl_link)"[^>]+href="([^"]+)""#,
        r#"(?i)var\s+(?:file_?link|dl_?link|download_?url)\s*=\s*['"](https?:[^'"]+)['"]"#,
    ];
    let img = crate::util::http::re(r"(?i)\.(?:png|jpe?g|gif|css|js)(?:\?|$)");
    for pat in pats {
        if let Some(m) = p.m(pat) {
            if !m.contains("/captchas/") && !img.is_match(&m) && !m.contains(&format!("/{fuid}")) {
                return Some(decode_entities(&m));
            }
        }
    }
    None
}

/// Newer XFS themes post `op=download2` from JavaScript instead of a <form>. Rebuild that request
/// (fields as in JD2 FilekeeperNet.findFormDownload2Free).
fn js_download2_form(p: &Page, fuid: &str) -> Option<Form> {
    if !p.has(r#"['"]op['"]\s*:\s*['"]download2['"]|op=download2"#) {
        return None;
    }
    // filekeeper.net (seen 2026-10): <div data-code data-rand data-referer data-method data-countdown
    // data-has-captcha data-has-password>; JS posts these fields to the same URL after the countdown
    let attr = |k: &str| p.m(&format!(r#"data-{k}="([^"]*)""#)).map(|v| decode_entities(&v));
    let code = attr("code").filter(|c| !c.is_empty()).unwrap_or_else(|| fuid.to_string());
    let rand = attr("rand").or_else(|| p.m(r#"['"]rand['"]\s*:\s*['"]([^'"]*)['"]"#)).unwrap_or_default();
    let referer = attr("referer").unwrap_or_default();
    let method = attr("method").unwrap_or_else(|| "Free download".into());
    // only look for captcha widgets when the page says there is one (ad scripts mention all sorts)
    let has_captcha = attr("has-captcha").is_none_or(|v| v != "false");
    let html = if has_captcha { p.m(r#"(?s)id="captcha-content"[^>]*>(.*?)</div>\s*</div>"#).unwrap_or_else(|| p.text.clone()) } else { String::new() };
    Some(Form {
        action: p.url.clone(),
        method: "POST".into(),
        id: None,
        html,
        fields: vec![
            ("op".into(), "download2".into()),
            ("id".into(), code),
            ("rand".into(), rand),
            ("referer".into(), referer),
            ("method_free".into(), method),
            ("down_direct".into(), "1".into()),
        ],
    })
}

fn wait_seconds(p: &Page) -> u64 {
    p.m(r#"data-countdown="(\d+)""#)
        .or_else(|| p.m(r#"(?is)<span id="countdown_str">[^<]*<span[^>]*>\s*(\d+)\s*</span>"#))
        .or_else(|| p.m(r#"(?is)id="countdown"[^>]*>\s*(?:<[^>]+>\s*)*(\d+)"#))
        .or_else(|| p.m(r#"(?i)class="seconds"[^>]*>\s*(\d+)\s*<"#))
        .or_else(|| p.m(r"(?i)(?:var\s+)?(?:countdown|seconds|waitTime)\s*=\s*(\d+)\s*;"))
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0 && *n < 600)
        .unwrap_or(0)
}

async fn image_data_url(ctx: &mut Ctx, url: &str) -> PResult<String> {
    use base64::Engine;
    let r = ctx.browser.open(url, &[("accept", "image/*".into())], "GET", None).await?;
    let ty = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("image/jpeg").split(';').next().unwrap_or("image/jpeg").to_string();
    let bytes = r.bytes().await.map_err(|e| PluginError::new(ErrCode::Retry, e.to_string()))?;
    Ok(format!("data:{ty};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)))
}

/// All XFS captcha variants handled by JD2's XFileSharingProBasic.handleCaptcha.
async fn xfs_captcha(ctx: &mut Ctx, page: &Page, form: &mut Form) -> PResult<()> {
    let html = form.html.clone();
    let sitekey = cap(r#"data-sitekey="([^"]+)""#, &html).or_else(|| page.m(r#"data-sitekey="([^"]+)""#));
    let page_url = page.url.clone();
    use crate::captcha::CaptchaRequest as C;
    if html.contains("cf-turnstile") && sitekey.is_some() {
        let t = ctx.captcha(C::Turnstile { site_key: sitekey.unwrap(), page_url }).await?;
        form.set("cf-turnstile-response", &t);
        form.set("g-recaptcha-response", &t);
    } else if (html.contains("h-captcha") || html.contains("hcaptcha.com")) && sitekey.is_some() {
        let t = ctx.captcha(C::Hcaptcha { site_key: sitekey.unwrap(), page_url }).await?;
        form.set("h-captcha-response", &t);
        form.set("g-recaptcha-response", &t);
    } else if (html.contains("g-recaptcha") || html.contains("recaptcha/api") || (!form.html.is_empty() && page.text.contains("g-recaptcha"))) && sitekey.is_some() {
        let t = ctx.captcha(C::Recaptcha2 { site_key: sitekey.unwrap(), page_url, invisible: false }).await?;
        form.set("g-recaptcha-response", &t);
    } else if crate::util::http::re(r"<span style=.position:absolute;padding-left:\d+px;padding-top:\d+px;.>&#\d+;</span>").is_match(&html) {
        // "plaintext" captcha: digits positioned with CSS - read them in visual order, no human needed
        let mut letters: Vec<(u32, String)> = crate::util::http::re(r"<span style=.position:absolute;padding-left:(\d+)px;padding-top:\d+px;.>(&#\d+;)</span>")
            .captures_iter(&html)
            .map(|c| (c[1].parse().unwrap_or(0), decode_entities(&c[2])))
            .collect();
        letters.sort_by_key(|l| l.0);
        let code: String = letters.into_iter().map(|l| l.1).collect();
        crate::log::debug("xfs", &format!("plaintext captcha solved locally: {code}"));
        form.set("code", &code);
    } else if html.contains("/captchas/") || (!form.html.is_empty() && page.text.contains("/captchas/")) {
        let src = cap(r#"(?i)(https?://[^"'\s]+/captchas/[a-z0-9]+\.jpe?g|/captchas/[a-z0-9]+\.jpe?g)"#, &format!("{html}{}", page.text)).ok_or_else(|| defect("XFS image captcha not found"))?;
        let abs = url::Url::parse(&page.url).and_then(|b| b.join(&src)).map(|u| u.to_string()).unwrap_or(src);
        let img = image_data_url(ctx, &abs).await?;
        let code = ctx.captcha(C::Image { image: img, hint: Some("4 characters".into()) }).await?;
        form.set("code", &code);
    }
    Ok(())
}

#[async_trait]
impl Plugin for Xfs {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let Some(id) = fuid(&ctx.url) else { return Ok(Some(LinkInfo::offline())) };
        let host = crate::util::http::host_of(&ctx.url);
        let p = ctx.page(&format!("https://{host}/{id}")).await?;
        if p.status == 404 || p.has(XFS_OFFLINE) {
            return Ok(Some(LinkInfo::offline()));
        }
        let (name, size) = scan_info(&p);
        let mut li = LinkInfo::online(name, size);
        li.canonical = Some(format!("https://{}/{id}", self.site.domains[0]));
        Ok(Some(li))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let id = fuid(&ctx.url).ok_or_else(|| offline("bad url"))?;
        let host = crate::util::http::host_of(&ctx.url);
        let mut page = ctx.page(&format!("https://{host}/{id}")).await?;
        if page.status == 404 {
            return Err(offline("File not found"));
        }
        xfs_errors(&page)?;
        let mut dl = xfs_direct(&page, &id);
        if dl.is_none() {
            if let Some(mut f1) = page.forms().into_iter().find(|f| f.get("op") == Some("download1")) {
                f1.remove("method_premium");
                if f1.get("method_free").is_none() {
                    f1.set("method_free", "Free Download");
                }
                page = ctx.post(&f1.action, &f1.fields, true).await?;
                xfs_errors(&page)?;
                dl = xfs_direct(&page, &id);
            }
        }
        let mut attempt = 0;
        while dl.is_none() && attempt < 3 {
            attempt += 1;
            let Some(mut f2) = page.forms().into_iter().find(|f| f.get("op") == Some("download2")).or_else(|| js_download2_form(&page, &id)) else { break };
            let t0 = std::time::Instant::now();
            let wait = wait_seconds(&page);
            if f2.get("password").is_some() {
                let pw = ctx.prop("linkPassword").and_then(|v| v.as_str().map(str::to_string)).ok_or_else(|| PluginError::new(ErrCode::Password, "File is password protected").hint("Set the download password on the link."))?;
                f2.set("password", &pw);
            }
            xfs_captcha(ctx, &page, &mut f2).await?;
            let left = (wait * 1000).saturating_sub(t0.elapsed().as_millis() as u64);
            if left > 0 {
                ctx.wait(left + 1000, "Hoster countdown").await?;
            }
            f2.remove("method_premium");
            page = ctx.post(&f2.action, &f2.fields, true).await?;
            if (300..400).contains(&page.status) {
                dl = xfs_direct(&page, &id);
                break;
            }
            match xfs_errors(&page) {
                Err(e) if e.code == ErrCode::CaptchaFailed && attempt < 3 => continue,
                Err(e) => return Err(e),
                Ok(()) => {}
            }
            dl = xfs_direct(&page, &id);
        }
        let dl = dl.ok_or_else(|| defect("XFS: no download link found"))?;
        let mut req = DirectRequest::get(dl).html_guard();
        req.max_connections = self.meta.limits.max_conns;
        if self.site.resumable == Some(false) {
            req.resumable = Some(false);
        }
        Ok(Resolution::http(req))
    }
}

// ============================================================================================
// YetiShare
// ============================================================================================

pub struct YetiShare {
    meta: Meta,
}
impl YetiShare {
    pub fn new(s: &SiteDef, source: &'static str) -> Self {
        let pat = format!(r"(?i)^https?://{}/([A-Za-z0-9]{{3,16}})(?:/[^?#]*)?$", host_re(&s.domains));
        let mut meta = Meta::host(&format!("yetishare-{}", s.id.to_lowercase()), &s.domains[0], &[], &[pat.as_str()]).prio(-6).limits(limits(s)).desc("YetiShare template");
        meta.domains = s.domains.clone();
        meta.version = "1.0.0-template".into();
        meta.source = source;
        YetiShare { meta }
    }
}

#[async_trait]
impl Plugin for YetiShare {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn matches(&self, url: &str) -> bool {
        // (no look-ahead in Rust regex) exclude the site's own pages
        self.meta.patterns.iter().any(|r| r.captures(url).is_some_and(|c| !matches!(c[1].to_ascii_lowercase().as_str(), "register" | "login" | "faq" | "terms" | "upload" | "account" | "report_file" | "folder")))
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let url = ctx.url.clone();
        let p = ctx.page(&url).await?;
        if p.status == 404 || p.url.contains("/error") || p.has(r"(?i)File has been removed|File not found") {
            return Ok(Some(LinkInfo::offline()));
        }
        let name = p.m(r#"(?is)<(?:span|div|h\d)[^>]*class="[^"]*(?:file-?name|heading-1)[^"]*"[^>]*>\s*([^<]+?)\s*<"#).or_else(|| p.m(r"(?is)<title>\s*([^<]+?)\s*(?:-|\|)[^<]*</title>"));
        let size = p.m(r"(?i)\(\s*([\d.,]+\s*[KMGT]?B)\s*\)").and_then(|s| crate::util::parse_bytes(&s)).map(|s| s as i64);
        Ok(Some(LinkInfo::online(name.map(|n| decode_entities(&n)), size)))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let url = ctx.url.clone();
        let mut p = ctx.page(&url).await?;
        for _ in 0..4 {
            if p.status == 404 || p.url.contains("/error") {
                let e = url::Url::parse(&p.url).ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "e").map(|(_, v)| v.into_owned())).unwrap_or_default();
                let l = e.to_lowercase();
                if l.contains("premium") || l.contains("account type") {
                    return Err(premium_only(&e));
                }
                if l.contains("wait") || l.contains("limit") {
                    return Err(hoster_wait(1_800_000, if e.is_empty() { "Hoster limit" } else { &e }));
                }
                return Err(offline(if e.is_empty() { "File not found" } else { &e }));
            }
            let direct = p.m(r#"(?i)(https?://[^"'<>\s]+\?(?:pt|download_token)=[^"'<>\s]+)"#);
            if let Some(s) = p.m(r"(?i)var\s+seconds\s*=\s*(\d+)").and_then(|s| s.parse::<u64>().ok()).filter(|s| *s > 0 && *s < 600) {
                ctx.wait(s * 1000 + 500, "Hoster countdown").await?;
            }
            let sitekey = p.m(r#"data-sitekey="([^"]+)""#);
            if let (Some(mut form), Some(sk)) = (p.forms().into_iter().find(|f| f.html.contains("g-recaptcha") || f.fields.iter().any(|(k, _)| k.contains("captcha"))), sitekey) {
                let t = ctx.captcha(crate::captcha::CaptchaRequest::Recaptcha2 { site_key: sk, page_url: p.url.clone(), invisible: false }).await?;
                form.set("g-recaptcha-response", &t);
                p = ctx.post(&form.action, &form.fields, true).await?;
                if let Some(l) = p.location().filter(|_| (300..400).contains(&p.status)) {
                    return Ok(Resolution::http(DirectRequest::get(l).html_guard()));
                }
                continue;
            }
            let Some(d) = direct else { break };
            let d = decode_entities(&d);
            let r = ctx.browser.get_with(&d, &crate::util::http::Req { allow_errors: true, no_follow: true, ..Default::default() }).await?;
            if let Some(l) = r.location().filter(|_| (300..400).contains(&r.status)) {
                return Ok(Resolution::http(DirectRequest::get(l).html_guard()));
            }
            if r.status == 200 && !r.header("content-type").unwrap_or_default().contains("text/html") {
                return Ok(Resolution::http(DirectRequest::get(d)));
            }
            p = r;
        }
        Err(defect("YetiShare: no download link found"))
    }
}

// ============================================================================================
// user JSON plugins
// ============================================================================================

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserDef {
    #[serde(rename = "type")]
    ty: String,
    id: String,
    #[serde(default)]
    name: Option<String>,
    domains: Vec<String>,
    #[serde(default)]
    max_chunks: Option<i64>,
    #[serde(default)]
    pattern: Option<String>,
    #[serde(default)]
    link_regex: Option<String>,
    #[serde(default)]
    name_regex: Option<String>,
    #[serde(default)]
    size_regex: Option<String>,
    #[serde(default)]
    offline_regex: Option<String>,
    #[serde(default)]
    wait_regex: Option<String>,
}

pub fn load_user_dir(dir: &std::path::Path) -> (Vec<P>, Vec<String>) {
    let mut out: Vec<P> = vec![];
    let mut errs = vec![];
    let _ = std::fs::create_dir_all(dir);
    let readme = dir.join("README.txt");
    if !readme.exists() {
        let _ = std::fs::write(&readme, "Drop *.json plugin files here (restart or use Plugins -> Reload).\nTypes: xfs, yetishare, ytdlp, regex. See docs/plugin-authoring.md.\n");
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return (out, errs) };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let defs: Result<Vec<UserDef>, _> = serde_json::from_str::<Vec<UserDef>>(&text).or_else(|_| serde_json::from_str::<UserDef>(&text).map(|d| vec![d]));
        match defs {
            Err(err) => errs.push(format!("{}: {err}", path.display())),
            Ok(defs) => {
                for d in defs {
                    let site = SiteDef { id: d.id.clone(), domains: d.domains.clone(), max_chunks: d.max_chunks, resumable: None, max_downloads: None, custom: vec![] };
                    match d.ty.as_str() {
                        "xfs" => out.push(Arc::new(Xfs::new(&site, "user"))),
                        "yetishare" => out.push(Arc::new(YetiShare::new(&site, "user"))),
                        "ytdlp" | "kvs" => {
                            let pat = d.pattern.clone().unwrap_or_else(|| format!(r"(?i)^https?://{}/", host_re(&site.domains)));
                            out.push(Arc::new(Ytdlp::new(&d.id, d.name.as_deref().unwrap_or(&site.domains[0]), site.domains.clone(), &pat, 5)));
                        }
                        "regex" => match RegexHost::new(d) {
                            Ok(p) => out.push(Arc::new(p)),
                            Err(e) => errs.push(format!("{}: {e}", path.display())),
                        },
                        t => errs.push(format!("{}: unknown plugin type {t}", path.display())),
                    }
                }
            }
        }
    }
    for p in &out {
        crate::log::info("plugins", &format!("loaded user plugin {}", p.meta().id));
    }
    (out, errs)
}

struct RegexHost {
    meta: Meta,
    d: UserDef,
}
impl RegexHost {
    fn new(d: UserDef) -> Result<Self, String> {
        let pat = d.pattern.clone().ok_or("regex plugin needs \"pattern\"")?;
        regex::Regex::new(&pat).map_err(|e| e.to_string())?;
        for r in [&d.link_regex, &d.name_regex, &d.size_regex, &d.offline_regex, &d.wait_regex].into_iter().flatten() {
            regex::Regex::new(r).map_err(|e| format!("{r}: {e}"))?;
        }
        if d.link_regex.is_none() {
            return Err("regex plugin needs \"linkRegex\"".into());
        }
        let mut meta = Meta::host(&d.id, d.name.as_deref().unwrap_or(&d.domains[0]), &[], &[pat.as_str()]).prio(5).limits(Limits { max_conns: chunks(d.max_chunks), ..Default::default() }).desc("User regex plugin");
        meta.domains = d.domains.clone();
        meta.source = "user";
        Ok(RegexHost { meta, d })
    }
}
#[async_trait]
impl Plugin for RegexHost {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let url = ctx.url.clone();
        let p = ctx.page(&url).await?;
        if p.status == 404 || self.d.offline_regex.as_deref().is_some_and(|r| p.has(r)) {
            return Ok(Some(LinkInfo::offline()));
        }
        let name = self.d.name_regex.as_deref().and_then(|r| p.m(r)).map(|s| decode_entities(s.trim()));
        let size = self.d.size_regex.as_deref().and_then(|r| p.m(r)).and_then(|s| crate::util::parse_bytes(&s)).map(|s| s as i64);
        Ok(Some(LinkInfo::online(name, size)))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let url = ctx.url.clone();
        let p = ctx.page(&url).await?;
        if p.status == 404 || self.d.offline_regex.as_deref().is_some_and(|r| p.has(r)) {
            return Err(offline("File not found"));
        }
        if let Some(w) = self.d.wait_regex.as_deref().and_then(|r| p.m(r)).and_then(|s| s.parse::<u64>().ok()) {
            ctx.wait(w * 1000, "Hoster countdown").await?;
        }
        let link = p.m(self.d.link_regex.as_deref().unwrap_or("")).ok_or_else(|| defect("linkRegex did not match"))?;
        let abs = url::Url::parse(&p.url).and_then(|b| b.join(&decode_entities(&link))).map(|u| u.to_string()).unwrap_or(link);
        let mut req = DirectRequest::get(abs).html_guard();
        req.max_connections = self.meta.limits.max_conns;
        Ok(Resolution::http(req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(text: &str) -> Page {
        Page { url: "https://filekeeper.net/abcdef123456".into(), status: 200, headers: Default::default(), text: text.into() }
    }

    #[test]
    fn xfs_vue_theme() {
        let p = page(r#"<div id="app" data-code="abcdef123456" data-countdown="15" link="https://filekeeper.net/abcdef123456/ACE%20COMBAT%208.zip"></div>
            <script>axios.post('/', {'op': 'download2', 'id': code, 'rand': 'r4nd'})</script>"#);
        assert_eq!(wait_seconds(&p), 15);
        assert_eq!(scan_info(&p).0.as_deref(), Some("ACE COMBAT 8.zip"));
        let f = js_download2_form(&p, "abcdef123456").expect("synthesized form");
        assert_eq!(f.get("op"), Some("download2"));
        assert_eq!(f.get("id"), Some("abcdef123456"));
        assert_eq!(f.get("rand"), Some("r4nd"));
        assert_eq!(f.get("down_direct"), Some("1"));
        let fk = page(r#"<div data-code="2h1nq2vz6sny" data-countdown="5" data-has-captcha="false" data-method="" data-rand="" data-referer=""></div><script>const fields = { 'op': 'download2', 'id': code };</script>"#);
        let f = js_download2_form(&fk, "2h1nq2vz6sny").unwrap();
        assert_eq!(f.get("method_free"), Some(""));
        assert!(f.html.is_empty());
        // JSON answer with the tunnel link
        let j = page(r#"{"status":"ok","url":"https:\/\/tunnel1.dlproxy.uk\/download\/s0Jb9lsw8n53AYLRv03-3bHt3WCOaL5LtDnEl2hnCOlhl47fRxf4QMo1LAFW?sig=vgbab"}"#);
        assert_eq!(xfs_direct(&j, "abcdef123456").as_deref(), Some("https://tunnel1.dlproxy.uk/download/s0Jb9lsw8n53AYLRv03-3bHt3WCOaL5LtDnEl2hnCOlhl47fRxf4QMo1LAFW?sig=vgbab"));
        // plain HTML button
        let h = page(r#"<a class="btn" href="https://tunnel1.dlproxy.uk/download/s0Jb9lsw8n53AYLRv03-3bHt3WCOaL5LtDnEl2hnCOl?sig=x">Download</a>"#);
        assert!(xfs_direct(&h, "abcdef123456").unwrap().starts_with("https://tunnel1.dlproxy.uk/download/"));
    }
    #[test]
    fn templates_load_and_match() {
        let v = builtin();
        assert!(v.len() > 400, "expected >400 template plugins, got {}", v.len());
        let rapid = v.iter().find(|p| p.meta().domains.iter().any(|d| d == "katfile.com"));
        if let Some(p) = rapid {
            assert!(p.matches("https://katfile.com/abcdef123456/file.zip.html"));
        }
        assert_eq!(fuid("https://x.com/abcdef123456/name.rar.html").as_deref(), Some("abcdef123456"));
    }
}
