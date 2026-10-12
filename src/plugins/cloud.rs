//! Google Drive, Dropbox, Internet Archive, GitHub releases.

use super::generic::probe;
use super::*;
use crate::model::{HashAlgo, HashInfo};
use crate::util::http::decode_entities;

pub fn all() -> Vec<P> {
    vec![
        Arc::new(GDrive { meta: Meta::host("gdrive", "Google Drive", &["drive.google.com", "docs.google.com", "drive.usercontent.google.com"], &[r"(?i)^https?://(?:drive|docs)\.google\.com/(?:file/d/|open\?id=|uc\?(?:.*&)?id=)[\w-]{20,}", r"(?i)^https?://drive\.usercontent\.google\.com/download\?(?:.*&)?id=[\w-]{20,}"]).limits(Limits { max_conns: Some(8), ..Default::default() }) }),
        Arc::new(GDriveFolder { meta: Meta::crawler("gdrive-folder", "Google Drive folders", &["drive.google.com"], &[r"(?i)^https?://drive\.google\.com/(?:drive/(?:u/\d+/)?folders/|embeddedfolderview\?id=)[\w-]{20,}"]) }),
        Arc::new(Dropbox { meta: Meta::host("dropbox", "Dropbox", &["dropbox.com", "dropboxusercontent.com"], &[r"(?i)^https?://(?:www\.)?dropbox\.com/(?:s|scl/fi|sh)/[^?#]+"]) }),
        Arc::new(ArchiveOrg { meta: Meta::crawler("archive-org", "Internet Archive", &["archive.org"], &[r"(?i)^https?://(?:www\.)?archive\.org/details/[^/?#]+"]) }),
        Arc::new(GithubReleases { meta: Meta::crawler("github-releases", "GitHub releases", &["github.com"], &[r"(?i)^https?://github\.com/[^/]+/[^/]+/releases(?:/(?:tag/[^/?#]+|latest))?/?$"]) }),
    ]
}

// ---- Google Drive ----------------------------------------------------------------------------
// JD2 GoogleDrive.java is ~2,000 lines. Core flow: drive.usercontent.google.com/download; large
// files return an HTML "virus scan warning" form (id=download-form) with confirm + uuid fields.
struct GDrive {
    meta: Meta,
}
fn gid(u: &str) -> Option<String> {
    cap(r"(?:/file/d/|[?&]id=)([\w-]{20,})", u)
}

#[async_trait]
impl Plugin for GDrive {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let Some(id) = gid(&ctx.url) else { return Ok(Some(LinkInfo::offline())) };
        let url = format!("https://drive.usercontent.google.com/download?id={id}&export=download");
        let p = probe(&ctx.browser, &url).await?;
        if p.status == 404 {
            return Ok(Some(LinkInfo::offline()));
        }
        let mut li = if !p.is_html {
            LinkInfo::online(Some(p.name), (p.size > 0).then_some(p.size))
        } else {
            let page = ctx.page(&url).await?;
            LinkInfo::online(page.m(r#"<a href="[^"]*">([^<]+)</a>\s*\("#).map(|s| decode_entities(&s)), None)
        };
        li.canonical = Some(format!("https://drive.google.com/file/d/{id}"));
        Ok(Some(li))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let id = gid(&ctx.url).ok_or_else(|| offline("bad url"))?;
        let url = format!("https://drive.usercontent.google.com/download?id={id}&export=download");
        let p = probe(&ctx.browser, &url).await?;
        if p.status == 404 {
            return Err(offline("File not found"));
        }
        if !p.is_html {
            return Ok(Resolution::Http { req: DirectRequest::get(url).html_guard(), name: Some(p.name), size: (p.size > 0).then_some(p.size), hash: None, post: vec![] });
        }
        let page = ctx.page(&url).await?;
        if page.has(r"(?i)Too many users have viewed or downloaded this file recently|quota") {
            return Err(hoster_wait(3_600_000, "Google Drive download quota exceeded for this file"));
        }
        if page.url.contains("accounts.google.com") || page.has("You need access") {
            return Err(premium_only("File is private (needs a Google account with access)"));
        }
        let form = page.forms().into_iter().find(|f| f.id.as_deref() == Some("download-form") || f.get("confirm").is_some()).ok_or_else(|| defect("Google Drive confirmation form not found"))?;
        let q = crate::util::http::form_urlencode(&form.fields);
        let sep = if form.action.contains('?') { '&' } else { '?' };
        Ok(Resolution::http(DirectRequest::get(format!("{}{sep}{q}", form.action)).html_guard()))
    }
}

struct GDriveFolder {
    meta: Meta,
}
#[async_trait]
impl Plugin for GDriveFolder {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn crawl(&self, ctx: &mut Ctx) -> PResult<CrawlResult> {
        // embeddedfolderview renders a plain HTML list without an API key (one level)
        let id = cap(r"(?:folders/|id=)([\w-]{20,})", &ctx.url).ok_or_else(|| offline("bad url"))?;
        let page = ctx.page(&format!("https://drive.google.com/embeddedfolderview?id={id}#list")).await?;
        if page.status == 404 {
            return Err(offline("Folder not found or private"));
        }
        let mut items = vec![];
        for c in crate::util::http::re(r#"(?s)<a href="(https://drive\.google\.com/(?:file/d|drive/folders)/[\w-]{20,})[^"]*"[^>]*>.*?class="flip-entry-title">([^<]+)<"#).captures_iter(&page.text) {
            let mut it = CrawledItem::url(c[1].to_string());
            if c[1].contains("/file/") {
                it.name = Some(decode_entities(&c[2]));
            }
            items.push(it);
        }
        Ok(CrawlResult { items, package_name: page.m(r"<title>([^<]+)</title>").map(|s| decode_entities(&s)).or(Some(id)), ..Default::default() })
    }
}

// ---- Dropbox ----------------------------------------------------------------------------------
struct Dropbox {
    meta: Meta,
}
fn dropbox_direct(u: &str) -> String {
    let mut url = url::Url::parse(u).unwrap_or_else(|_| url::Url::parse("https://www.dropbox.com/").unwrap());
    let pairs: Vec<(String, String)> = url.query_pairs().filter(|(k, _)| k != "dl").map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    url.query_pairs_mut().clear().extend_pairs(pairs).append_pair("dl", "1");
    url.to_string()
}
#[async_trait]
impl Plugin for Dropbox {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        let p = probe(&ctx.browser, &dropbox_direct(&ctx.url)).await?;
        if p.status == 404 || p.status == 410 {
            return Ok(Some(LinkInfo::offline()));
        }
        Ok(Some(if p.is_html { LinkInfo::unknown() } else { LinkInfo::online(Some(p.name), (p.size > 0).then_some(p.size)) }))
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        Ok(Resolution::http(DirectRequest::get(dropbox_direct(&ctx.url)).html_guard()))
    }
}

// ---- Internet Archive ---------------------------------------------------------------------------
struct ArchiveOrg {
    meta: Meta,
}
#[async_trait]
impl Plugin for ArchiveOrg {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn crawl(&self, ctx: &mut Ctx) -> PResult<CrawlResult> {
        let ident = cap(r"/details/([^/?#]+)", &ctx.url).ok_or_else(|| offline("bad url"))?;
        let j = ctx.get(&format!("https://archive.org/metadata/{ident}")).await?.json()?;
        let files = j["files"].as_array().cloned().unwrap_or_default();
        if files.is_empty() || j["is_dark"] == Value::Bool(true) {
            return Err(offline("Item not found or dark"));
        }
        let junk = crate::util::http::re(r"(?i)^(?:Metadata|Thumbnail|Item Tile|Archive BitTorrent|BitTorrent|BitTorrentContents|JSON|Spectrogram|Columbia Peaks|PNG|Item Image|Web ARChive GZ|Activity|Book Cover Image)$");
        let junk_name = crate::util::http::re(r"\.thumbs/|_meta\.(?:xml|sqlite)$|_files\.xml$|__ia_thumb\.jpg$|_reviews\.xml$");
        let enc = |s: &str| percent_encoding::utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string();
        let items = files
            .iter()
            .filter(|f| f["private"] != "true" && !junk.is_match(f["format"].as_str().unwrap_or("")) && !junk_name.is_match(f["name"].as_str().unwrap_or("")))
            .map(|f| {
                let name = f["name"].as_str().unwrap_or("");
                let mut it = CrawledItem::file(format!("https://archive.org/download/{ident}/{}", name.split('/').map(enc).collect::<Vec<_>>().join("/")), name.rsplit('/').next().map(str::to_string), j_i64(f, "size"));
                if name.contains('/') {
                    it.subdir = Some(name.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default());
                }
                if let Some(m) = j_str(f, "md5") {
                    it.props.insert("md5".into(), Value::String(m));
                }
                it
            })
            .collect();
        Ok(CrawlResult { items, package_name: j_str(&j, "metadata/title").or(Some(ident)), ..Default::default() })
    }
}

// ---- GitHub releases -----------------------------------------------------------------------------
struct GithubReleases {
    meta: Meta,
}
#[async_trait]
impl Plugin for GithubReleases {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn crawl(&self, ctx: &mut Ctx) -> PResult<CrawlResult> {
        let c = crate::util::http::re(r"github\.com/([^/]+)/([^/]+)/releases(?:/(?:tag/([^/?#]+)|latest))?").captures(&ctx.url).ok_or_else(|| offline("bad url"))?;
        let (owner, repo) = (c[1].to_string(), c[2].to_string());
        let api = match c.get(3) {
            Some(t) => format!("https://api.github.com/repos/{owner}/{repo}/releases/tags/{}", t.as_str()),
            None => format!("https://api.github.com/repos/{owner}/{repo}/releases/latest"),
        };
        let p = ctx.get_h(&api, vec![("accept", "application/vnd.github+json".into())]).await?;
        if p.status == 404 {
            return Err(offline("Release not found"));
        }
        if p.status == 403 {
            return Err(hoster_wait(600_000, "GitHub API rate limit"));
        }
        let j = p.json()?;
        let tag = j_str(&j, "tag_name").unwrap_or_default();
        let mut items: Vec<CrawledItem> = j["assets"].as_array().into_iter().flatten().map(|a| {
            let mut it = CrawledItem::file(a["browser_download_url"].as_str().unwrap_or("").to_string(), j_str(a, "name"), j_i64(a, "size"));
            if let Some(d) = j_str(a, "digest").and_then(|d| d.strip_prefix("sha256:").map(str::to_string)) {
                it.props.insert("sha256".into(), Value::String(d));
            }
            it
        }).collect();
        if items.is_empty() {
            items.push(CrawledItem::file(format!("https://github.com/{owner}/{repo}/archive/refs/tags/{tag}.zip"), Some(format!("{repo}-{tag}.zip")), None));
        }
        let name = j_str(&j, "name").filter(|s| !s.is_empty()).unwrap_or(tag);
        Ok(CrawlResult { items, package_name: Some(format!("{repo} {name}")), ..Default::default() })
    }
}

pub fn md5_hash(v: &str) -> HashInfo {
    HashInfo { algo: HashAlgo::Md5, value: v.into(), verified: None }
}
