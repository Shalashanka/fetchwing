//! Port of JD2 MediafireCom / MediafireComFolder. API 1.5 for info, HTML for the download button
//! (JD2 checks kNO = "...", redirect, id="downloadButton" href, data-scrambled-url base64).

use super::*;
use crate::model::{HashAlgo, HashInfo};
use crate::util::http::decode_entities;
use base64::Engine;

pub fn all() -> Vec<P> {
    vec![
        Arc::new(Mediafire { meta: Meta::host("mediafire", "MediaFire", &["mediafire.com"], &[r"(?i)^https?://(?:www\.)?mediafire\.com/(?:file(?:_premium)?/|download/|view/|\?)[a-z0-9]{11,15}"]).limits(Limits { max_conns: Some(8), ..Default::default() }) }),
        Arc::new(MediafireFolder { meta: Meta::crawler("mediafire-folder", "MediaFire folders", &["mediafire.com"], &[r"(?i)^https?://(?:www\.)?mediafire\.com/(?:folder/|\?sharekey=|#)[a-z0-9]{8,15}"]) }),
    ]
}
struct Mediafire {
    meta: Meta,
}
struct MediafireFolder {
    meta: Meta,
}

fn key(u: &str) -> Option<String> {
    cap(r"(?i)mediafire\.com/(?:file(?:_premium)?/|download/|view/|\?)([a-z0-9]{11,15})", u)
}

#[async_trait]
impl Plugin for Mediafire {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let Some(k) = key(&ctx.url) else { return Ok(Some(LinkInfo::offline())) };
        let j = ctx.page(&format!("https://www.mediafire.com/api/1.5/file/get_info.php?quick_key={k}&response_format=json")).await?.json()?;
        if j["response"]["result"] != "Success" {
            return Ok(Some(LinkInfo::offline()));
        }
        let f = &j["response"]["file_info"];
        let mut li = LinkInfo::online(j_str(f, "filename"), j_i64(f, "size"));
        li.hash = j_str(f, "hash").map(|v| HashInfo { algo: HashAlgo::Sha256, value: v, verified: None });
        li.canonical = Some(format!("https://www.mediafire.com/file/{k}"));
        Ok(Some(li))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let k = key(&ctx.url).ok_or_else(|| offline("bad url"))?;
        let p = ctx.page(&format!("https://www.mediafire.com/file/{k}")).await?;
        if p.status == 404 || p.url.contains("error.php") || p.has(r"(?i)File Removed|file you requested was not found") {
            return Err(offline("File removed"));
        }
        if p.has(r#"class\s*=\s*"passwordPrompt"|aria-labelledby\s*=\s*"passwordmsg""#) {
            return Err(PluginError::new(ErrCode::Password, "File is password protected").hint("Password-protected MediaFire files are not supported yet."));
        }
        if let Some(ttl) = p.m(r"var limitReachedTTL = (\d+);") {
            return Err(hoster_wait(ttl.parse::<u64>().unwrap_or(600) * 1000, "MediaFire download limit reached"));
        }
        let mut dl = p
            .m(r#"kNO\s*=\s*"(https?://[^"]+)""#)
            .or_else(|| p.m(r#"href\s*=\s*"(https?://[^"]+)"\s*id\s*=\s*"downloadButton""#))
            .or_else(|| p.m(r#"id\s*=\s*"downloadButton"[^>]*href\s*=\s*"(https?://[^"]+)""#))
            .or_else(|| p.m(r#"(https?://download\d+\.mediafire\.com/[^"'\s<>]+)"#));
        if dl.is_none() {
            if let Some(s) = p.m(r#"data-scrambled-url\s*=\s*"([^"]*)""#) {
                dl = base64::engine::general_purpose::STANDARD.decode(s).ok().and_then(|b| String::from_utf8(b).ok());
            }
        }
        let dl = dl.ok_or_else(|| defect("MediaFire download button not found"))?;
        Ok(Resolution::http(DirectRequest::get(decode_entities(&dl)).html_guard()))
    }
}

#[async_trait]
impl Plugin for MediafireFolder {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn crawl(&self, ctx: &mut Ctx) -> PResult<CrawlResult> {
        let fk = cap(r"(?i)(?:folder/|sharekey=|#)([a-z0-9]{8,15})", &ctx.url).ok_or_else(|| offline("bad url"))?;
        let title = ctx.page(&format!("https://www.mediafire.com/api/1.5/folder/get_info.php?folder_key={fk}&response_format=json")).await.ok()
            .and_then(|p| p.json().ok()).and_then(|j| j_str(&j, "response/folder_info/name"));
        let mut items = vec![];
        let mut stack = vec![(fk.clone(), String::new(), 0)];
        while let Some((key, sub, depth)) = stack.pop() {
            if depth > 8 {
                continue;
            }
            for ty in ["files", "folders"] {
                for chunk in 1..200 {
                    let j = ctx.get(&format!("https://www.mediafire.com/api/1.5/folder/get_content.php?folder_key={key}&content_type={ty}&chunk={chunk}&response_format=json")).await?.json()?;
                    let c = &j["response"]["folder_content"];
                    if j["response"]["result"] != "Success" {
                        return Err(offline("Folder not found"));
                    }
                    for f in c["files"].as_array().into_iter().flatten() {
                        let mut it = CrawledItem::file(format!("https://www.mediafire.com/file/{}", f["quickkey"].as_str().unwrap_or("")), j_str(f, "filename"), j_i64(f, "size"));
                        it.subdir = (!sub.is_empty()).then(|| sub.clone());
                        items.push(it);
                    }
                    for d in c["folders"].as_array().into_iter().flatten() {
                        let name = d["name"].as_str().unwrap_or("folder");
                        stack.push((d["folderkey"].as_str().unwrap_or("").to_string(), if sub.is_empty() { name.to_string() } else { format!("{sub}/{name}") }, depth + 1));
                    }
                    if c["more_chunks"] != "yes" {
                        break;
                    }
                }
            }
        }
        Ok(CrawlResult { items, package_name: title.or(Some(fk)), ..Default::default() })
    }
}
