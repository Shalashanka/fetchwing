//! Free path of JD2 OneFichierCom (1,500 lines incl. API/premium).
//! GET page (cookie LG=en) -> first form: remove "save", did=1 -> POST -> "Click here to download".

use super::*;
use crate::util::http::decode_entities;

const DOMAINS: &[&str] = &["1fichier.com", "alterupload.com", "cjoint.net", "desfichiers.com", "dfichiers.com", "megadl.fr", "mesfichiers.org", "piecejointe.net", "pjointe.com", "tenvoi.com", "dl4free.com"];

pub fn all() -> Vec<P> {
    let pat = format!(r"(?i)^https?://(?:[a-z0-9]+\.)?(?:{})/\?([a-z0-9]{{5,20}})", DOMAINS.iter().map(|d| regex::escape(d)).collect::<Vec<_>>().join("|"));
    vec![Arc::new(OneFichier { meta: Meta::host("1fichier", "1fichier", DOMAINS, &[pat.as_str()]).limits(Limits { max_downloads: Some(1), max_conns: Some(1), max_conns_premium: Some(3) }) })]
}
struct OneFichier {
    meta: Meta,
}

fn canonical(u: &str) -> String {
    cap(r"(?i)/\?([a-z0-9]{5,20})", u).map(|id| format!("https://1fichier.com/?{}", id.to_lowercase())).unwrap_or_else(|| u.to_string())
}

fn errors(p: &Page) -> PResult<()> {
    if p.has(r"(?i)>\s*(?:File not found|The requested file (?:could not be found|has been deleted))|Le fichier demandé n'existe pas") {
        return Err(offline("File not found"));
    }
    let mut mins = p.m(r"(?i)you must wait (?:at least|up to)\s*(\d+)\s*minutes between each downloads").or_else(|| p.m(r"(?i)>\s*You must wait\s*(\d+)\s*minutes"));
    if p.has(r"(?i)>\s*IP Locked|Will be unlocked within 1h") {
        mins = Some("60".into());
    }
    if mins.is_some() || p.has(r"(?i)only download one file at a time|You must wait for another download|Without Premium, you must wait between downloads|You already downloading") {
        let m = mins.and_then(|m| m.parse::<u64>().ok()).unwrap_or(5).max(1);
        return Err(hoster_wait(m * 60_000, "1fichier: free limit (one file at a time, wait between downloads)"));
    }
    if p.has(r"(?i)No free slot|no more free slots") {
        return Err(hoster_wait(300_000, "1fichier: no free slots"));
    }
    Ok(())
}

#[async_trait]
impl Plugin for OneFichier {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        ctx.browser.set_cookie("1fichier.com", "LG", "en");
        let p = ctx.page(&canonical(&ctx.url)).await?;
        if p.status == 404 {
            return Ok(Some(LinkInfo::offline()));
        }
        if let Err(e) = errors(&p) {
            if e.code == ErrCode::Offline {
                return Ok(Some(LinkInfo::offline()));
            }
        }
        let name = p.m(r"(?is)>\s*File\s*Name\s*:?\s*</td>\s*<td[^>]*>\s*([^<]+?)\s*<").or_else(|| p.m(r#"(?is)<td class="normal">\s*<span[^>]*>\s*([^<]+?)\s*</span>"#));
        let size = p.m(r"(?is)>\s*Size\s*:?\s*</td>\s*<td[^>]*>\s*([\d.,]+\s*[KMGT]?B)").and_then(|s| crate::util::parse_bytes(&s));
        let mut li = LinkInfo::online(name.map(|n| decode_entities(&n)), size.map(|s| s as i64));
        li.canonical = Some(canonical(&ctx.url));
        Ok(Some(li))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        ctx.browser.set_cookie("1fichier.com", "LG", "en");
        let url = canonical(&ctx.url);
        let p = ctx.page(&url).await?;
        if p.status == 404 {
            return Err(offline("File not found"));
        }
        errors(&p)?;
        if let Some(ct) = p.m(r"var\s+ct\s*=\s*(\d+)").and_then(|s| s.parse::<u64>().ok()).filter(|s| *s < 3600) {
            ctx.wait(ct * 1000 + 1000, "1fichier countdown").await?;
        }
        let mut form = p.forms().into_iter().next().ok_or_else(|| defect("1fichier: download form missing"))?;
        form.remove("save");
        form.set("did", "1");
        if form.get("pass").is_some() {
            let pw = ctx.prop("linkPassword").and_then(|v| v.as_str().map(str::to_string)).ok_or_else(|| PluginError::new(ErrCode::Password, "File is password protected").hint("Set the download password on the link."))?;
            form.set("pass", &pw);
        }
        let action = if form.action.is_empty() { url.clone() } else { form.action.clone() };
        let r = ctx.post(&action, &form.fields, true).await?;
        if let Some(loc) = r.location().filter(|_| (300..400).contains(&r.status)) {
            return Ok(Resolution::http(DirectRequest::get(loc).conns(1).html_guard()));
        }
        errors(&r)?;
        let dl = r.m(r#"(?is)<a href="([^"]+)"[^>]*>\s*(?:Click here to|Start your) download"#)
            .or_else(|| r.m(r#"align:middle">\s+<a href=["'](https?://[a-zA-Z0-9_-]+\.(?:1fichier|desfichiers)\.com/[a-zA-Z0-9]+[^"']*)["']"#))
            .ok_or_else(|| defect("1fichier: no download link"))?;
        Ok(Resolution::http(DirectRequest::get(decode_entities(&dl)).conns(1).html_guard()))
    }
}
