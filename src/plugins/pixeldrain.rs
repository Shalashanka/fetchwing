//! Port of JD2 PixeldrainCom / PixeldrainComFolder: public JSON API, sha256 provided.

use super::*;
use crate::model::{HashAlgo, HashInfo};

const API: &str = "https://pixeldrain.com/api";

pub fn all() -> Vec<P> {
    vec![Arc::new(Pixeldrain { meta: Meta::host("pixeldrain", "Pixeldrain", &["pixeldrain.com", "pixeldra.in"], &[r"(?i)^https?://(?:www\.)?pixeldra(?:in\.com|\.in)/(?:u|api/file)/[A-Za-z0-9]{2,}"]).limits(Limits { max_conns: Some(4), ..Default::default() }) }),
         Arc::new(PixeldrainList { meta: Meta::crawler("pixeldrain-list", "Pixeldrain lists", &["pixeldrain.com"], &[r"(?i)^https?://(?:www\.)?pixeldrain\.com/l/[A-Za-z0-9]+"]) })]
}

fn fid(u: &str) -> Option<String> {
    cap(r"/(?:u|api/file)/([A-Za-z0-9]{2,})", u)
}

struct Pixeldrain {
    meta: Meta,
}
struct PixeldrainList {
    meta: Meta,
}

async fn info(ctx: &mut Ctx, id: &str) -> PResult<Value> {
    let p = ctx.page(&format!("{API}/file/{id}/info")).await?;
    if p.status == 404 {
        return Err(offline("File not found"));
    }
    Ok(p.json()?)
}

#[async_trait]
impl Plugin for Pixeldrain {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let Some(id) = fid(&ctx.url) else { return Ok(Some(LinkInfo::offline())) };
        let j = match info(ctx, &id).await {
            Err(e) if e.code == ErrCode::Offline => return Ok(Some(LinkInfo::offline())),
            r => r?,
        };
        let mut li = LinkInfo::online(j_str(&j, "name"), j_i64(&j, "size"));
        li.hash = j_str(&j, "hash_sha256").map(|v| HashInfo { algo: HashAlgo::Sha256, value: v, verified: None });
        li.canonical = Some(format!("https://pixeldrain.com/u/{id}"));
        Ok(Some(li))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let id = fid(&ctx.url).ok_or_else(|| offline("bad url"))?;
        let j = info(ctx, &id).await?;
        if let Some(a) = j_str(&j, "availability") {
            if a.contains("rate_limited") || a.contains("captcha") {
                return Err(hoster_wait(30 * 60_000, &j_str(&j, "availability_message").unwrap_or("Pixeldrain rate limit reached".into())));
            }
        }
        Ok(Resolution::Http {
            req: DirectRequest::get(format!("{API}/file/{id}?download")).html_guard(),
            name: j_str(&j, "name"),
            size: j_i64(&j, "size"),
            hash: j_str(&j, "hash_sha256").map(|v| HashInfo { algo: HashAlgo::Sha256, value: v, verified: None }),
            post: vec![],
        })
    }
}

#[async_trait]
impl Plugin for PixeldrainList {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn crawl(&self, ctx: &mut Ctx) -> PResult<CrawlResult> {
        let lid = cap(r"/l/([A-Za-z0-9]+)", &ctx.url).ok_or_else(|| offline("bad url"))?;
        let j = ctx.get(&format!("{API}/list/{lid}")).await?.json()?;
        let items = j["files"].as_array().cloned().unwrap_or_default().iter()
            .map(|f| CrawledItem::file(format!("https://pixeldrain.com/u/{}", f["id"].as_str().unwrap_or("")), j_str(f, "name"), j_i64(f, "size")))
            .collect();
        Ok(CrawlResult { items, package_name: j_str(&j, "title"), ..Default::default() })
    }
}
