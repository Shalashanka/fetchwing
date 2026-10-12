//! Port of JD2 GofileIo + GoFileIoCrawler (rev 53159).
//! - guest token: POST https://api.gofile.io/accounts -> data.token (cached 30 min), cookie accountToken
//! - content calls need X-Website-Token = sha256("<UA>::<lang>::<token>::<unix/14400>::<salt>")
//!   (JD2 uses a dedicated whitelisted UA "JDownloader<->Gofile"; we use our own UA consistently)

use super::*;
use crate::model::{HashAlgo, HashInfo};
use sha2::{Digest, Sha256};

const SALT: &str = "9844d94d963d30";

pub fn all() -> Vec<P> {
    vec![
        Arc::new(GofileFolder { meta: Meta::crawler("gofile-folder", "GoFile folders", &["gofile.io"], &[r"(?i)^https?://(?:www\.)?gofile\.io/(?:d/|\?c=|#download#)[A-Za-z0-9-]+/?$"]) }),
        Arc::new(Gofile { meta: Meta::host("gofile", "GoFile", &["gofile.io"], &[r"(?i)^https?://(?:www\.)?gofile\.io/(?:d/|\?c=)[A-Za-z0-9-]+#file=[a-f0-9-]+"]).limits(Limits { max_conns: Some(4), max_downloads: Some(2), ..Default::default() }) }),
    ]
}

struct Gofile {
    meta: Meta,
}
struct GofileFolder {
    meta: Meta,
}

static GUEST: Mutex<Option<(String, i64)>> = Mutex::new(None);

async fn token(ctx: &mut Ctx) -> PResult<String> {
    if let Some((t, at)) = GUEST.lock().clone() {
        if crate::model::now_ms() - at < 30 * 60_000 {
            return Ok(t);
        }
    }
    let p = ctx.post_json("https://api.gofile.io/accounts", &serde_json::json!({}), vec![("origin", "https://gofile.io".into()), ("referer", "https://gofile.io/".into())]).await?;
    let j = p.json()?;
    let t = j_str(&j, "data/token").filter(|_| j["status"] == "ok").ok_or_else(|| defect("Could not get a gofile guest token"))?;
    *GUEST.lock() = Some((t.clone(), crate::model::now_ms()));
    Ok(t)
}

fn website_token(ua: &str, tok: &str) -> String {
    let block = crate::model::now_ms() / 1000 / 14400;
    hex::encode(Sha256::digest(format!("{ua}::en-US::{tok}::{block}::{SALT}").as_bytes()))
}

async fn contents(ctx: &mut Ctx, id: &str, password: Option<&str>) -> PResult<Value> {
    let tok = token(ctx).await?;
    ctx.browser.set_cookie("gofile.io", "accountToken", &tok);
    let ua = ctx.setting("connection.userAgent").as_str().unwrap_or_default().to_string();
    let mut url = format!("https://api.gofile.io/contents/{id}?contentId={id}");
    if let Some(pw) = password {
        url.push_str(&format!("&password={}", hex::encode(Sha256::digest(pw.as_bytes()))));
    }
    let p = ctx.get_h(&url, vec![("authorization", format!("Bearer {tok}")), ("x-website-token", website_token(&ua, &tok)), ("x-bl", "en-US".into()), ("origin", "https://gofile.io".into()), ("referer", "https://gofile.io/".into())]).await?;
    let j = p.json()?;
    match j["status"].as_str().unwrap_or("") {
        "ok" => {}
        "error-notFound" => return Err(offline("Not found on gofile")),
        "error-notPremium" => return Err(premium_only("This gofile link needs a premium account")),
        s => return Err(defect(&format!("gofile: {s}")).hint("Gofile changed its API or website-token algorithm.")),
    }
    let d = j["data"].clone();
    if matches!(d["passwordStatus"].as_str(), Some("passwordRequired") | Some("passwordWrong")) {
        return Err(PluginError::new(ErrCode::Password, "Folder is password protected").hint("Add the password when pasting the link."));
    }
    if d["canAccess"] == Value::Bool(false) {
        return Err(premium_only("Private gofile link"));
    }
    Ok(d)
}

#[async_trait]
impl Plugin for GofileFolder {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn crawl(&self, ctx: &mut Ctx) -> PResult<CrawlResult> {
        let cid = cap(r"(?:d/|\?c=|#download#)([A-Za-z0-9-]+)", &ctx.url).ok_or_else(|| offline("bad url"))?;
        let pw = ctx.passwords.first().cloned();
        let d = contents(ctx, &cid, pw.as_deref()).await?;
        let mut items = vec![];
        let kids: Vec<Value> = if d["type"] == "file" { vec![d.clone()] } else { d["children"].as_object().map(|m| m.values().cloned().collect()).unwrap_or_default() };
        for e in kids {
            if e["type"] == "file" {
                let mut it = CrawledItem::file(format!("https://gofile.io/d/{cid}#file={}", e["id"].as_str().unwrap_or("")), j_str(&e, "name"), j_i64(&e, "size"));
                if let Some(l) = j_str(&e, "link") {
                    it.props.insert("gofileLink".into(), Value::String(l));
                }
                if let Some(m) = j_str(&e, "md5") {
                    it.props.insert("md5".into(), Value::String(m));
                }
                items.push(it);
            } else if let Some(code) = j_str(&e, "code") {
                items.push(CrawledItem::url(format!("https://gofile.io/d/{code}")));
            }
        }
        Ok(CrawlResult { items, package_name: j_str(&d, "name").or(Some(cid)), comment: j_str(&d, "description"), ..Default::default() })
    }
}

#[async_trait]
impl Plugin for Gofile {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let fid = cap(r"#file=([a-f0-9-]+)", &ctx.url).unwrap_or_default();
        match contents(ctx, &fid, None).await {
            Ok(e) => {
                let mut li = LinkInfo::online(j_str(&e, "name"), j_i64(&e, "size"));
                li.hash = j_str(&e, "md5").map(|v| HashInfo { algo: HashAlgo::Md5, value: v, verified: None });
                Ok(Some(li))
            }
            Err(e) if e.code == ErrCode::Offline => Ok(Some(LinkInfo::offline())),
            Err(_) => Ok(Some(LinkInfo::unknown())),
        }
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let fid = cap(r"#file=([a-f0-9-]+)", &ctx.url).unwrap_or_default();
        let link = match ctx.prop("gofileLink").and_then(|v| v.as_str().map(str::to_string)) {
            Some(l) => l,
            None => j_str(&contents(ctx, &fid, None).await?, "link").ok_or_else(|| defect("No download link in gofile response"))?,
        };
        let tok = token(ctx).await?;
        Ok(Resolution::http(DirectRequest::get(link).header("cookie", format!("accountToken={tok}")).header("referer", "https://gofile.io/").html_guard()))
    }
}
