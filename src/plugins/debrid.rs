//! Multihosters / "debrid" services. JD2 has ~55 (RealDebridCom, AllDebridCom, TorboxApp, ...).
//! They are the main way people download from premium-only hosters in 2026, so Fetchwing treats
//! them as a core concept: an account on a debrid service transparently unlocks every host it
//! supports; the scheduler picks it automatically when there is no direct account for the host.
//! The account "password" field holds the service's API token.

use super::*;

pub fn all() -> Vec<P> {
    let mk = |id: &str, name: &str, dom: &str| {
        let mut m = Meta::host(id, name, &[dom], &[]);
        m.account_only = true;
        m.desc("Debrid service: add your API token as an account; it unlocks every hoster the service supports.")
    };
    vec![
        Arc::new(RealDebrid { meta: mk("real-debrid", "Real-Debrid", "real-debrid.com") }),
        Arc::new(AllDebrid { meta: mk("alldebrid", "AllDebrid", "alldebrid.com") }),
        Arc::new(Premiumize { meta: mk("premiumize", "Premiumize.me", "premiumize.me") }),
        Arc::new(TorBox { meta: mk("torbox", "TorBox", "torbox.app") }),
    ]
}

fn token(acc: &PluginAccount) -> PResult<String> {
    if acc.password.trim().is_empty() {
        return Err(PluginError::new(ErrCode::AccountInvalid, "API token missing").hint("Paste the API token from the service website into the password field."));
    }
    Ok(acc.password.trim().to_string())
}
fn bearer(t: &str) -> Vec<(&'static str, String)> {
    vec![("authorization", format!("Bearer {t}"))]
}
fn ok_http(url: String, name: Option<String>, size: Option<i64>) -> Resolution {
    Resolution::Http { req: DirectRequest::get(url).conns(16).html_guard(), name, size, hash: None, post: vec![] }
}

// ---- Real-Debrid -------------------------------------------------------------------------
struct RealDebrid {
    meta: Meta,
}
#[async_trait]
impl Plugin for RealDebrid {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn is_multihost(&self) -> bool {
        true
    }
    fn has_login(&self) -> bool {
        true
    }
    async fn login(&self, ctx: &mut Ctx, acc: &PluginAccount) -> PResult<Option<AccountInfo>> {
        let t = token(acc)?;
        let u = ctx.get_h("https://api.real-debrid.com/rest/1.0/user", bearer(&t)).await?;
        if u.status == 401 || u.status == 403 {
            return Ok(Some(AccountInfo { ok: false, message: Some("Invalid or expired token".into()), ..Default::default() }));
        }
        let j = u.json()?;
        let hosts: Vec<String> = ctx.get("https://api.real-debrid.com/rest/1.0/hosts/domains").await?.json().ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        Ok(Some(AccountInfo {
            ok: true,
            premium: j["type"] == "premium",
            valid_until: j_str(&j, "expiration").and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok()).map(|d| d.timestamp_millis()),
            message: j_str(&j, "username").map(|u| format!("Logged in as {u}")),
            supported_hosts: hosts,
            ..Default::default()
        }))
    }
    async fn unrestrict(&self, ctx: &mut Ctx, acc: &PluginAccount) -> PResult<Option<Resolution>> {
        let t = token(acc)?;
        let url = ctx.url.clone();
        let p = ctx.post_h("https://api.real-debrid.com/rest/1.0/unrestrict/link", &[("link".into(), url)], bearer(&t)).await?;
        let j = p.json()?;
        match j_str(&j, "download") {
            Some(d) => Ok(Some(ok_http(d, j_str(&j, "filename"), j_i64(&j, "filesize")))),
            None => {
                let code = j_i64(&j, "error_code").unwrap_or(0);
                let msg = format!("Real-Debrid: {}", j_str(&j, "error").unwrap_or_else(|| format!("HTTP {}", p.status)));
                Err(PluginError::new(
                    match code {
                        8 | 9 => ErrCode::AccountInvalid,
                        19 | 24 => ErrCode::Offline,
                        34 | 36 => ErrCode::Quota,
                        16 => ErrCode::Unsupported,
                        _ => ErrCode::Retry,
                    },
                    msg,
                ))
            }
        }
    }
}

// ---- AllDebrid ------------------------------------------------------------------------------
const AD: &str = "fetchwing";
struct AllDebrid {
    meta: Meta,
}
#[async_trait]
impl Plugin for AllDebrid {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn is_multihost(&self) -> bool {
        true
    }
    fn has_login(&self) -> bool {
        true
    }
    async fn login(&self, ctx: &mut Ctx, acc: &PluginAccount) -> PResult<Option<AccountInfo>> {
        let t = token(acc)?;
        let u = ctx.get_h(&format!("https://api.alldebrid.com/v4/user?agent={AD}"), bearer(&t)).await?.json()?;
        if u["status"] != "success" {
            return Ok(Some(AccountInfo { ok: false, message: j_str(&u, "error/message").or(Some("Login failed".into())), ..Default::default() }));
        }
        let h = ctx.get_h(&format!("https://api.alldebrid.com/v4/user/hosts?agent={AD}"), bearer(&t)).await?.json()?;
        let hosts = h["data"]["hosts"].as_object().into_iter().flat_map(|m| m.values()).filter(|x| x["status"] != Value::Bool(false))
            .flat_map(|x| x["domains"].as_array().cloned().unwrap_or_default()).filter_map(|d| d.as_str().map(str::to_string)).collect();
        Ok(Some(AccountInfo {
            ok: true,
            premium: u["data"]["user"]["isPremium"] == Value::Bool(true),
            valid_until: j_i64(&u, "data/user/premiumUntil").map(|s| s * 1000),
            message: j_str(&u, "data/user/username").map(|n| format!("Logged in as {n}")),
            supported_hosts: hosts,
            ..Default::default()
        }))
    }
    async fn unrestrict(&self, ctx: &mut Ctx, acc: &PluginAccount) -> PResult<Option<Resolution>> {
        let t = token(acc)?;
        let enc = percent_encoding::utf8_percent_encode(&ctx.url, percent_encoding::NON_ALPHANUMERIC).to_string();
        let j = ctx.get_h(&format!("https://api.alldebrid.com/v4/link/unlock?agent={AD}&link={enc}"), bearer(&t)).await?.json()?;
        if j["status"] != "success" {
            let c = j_str(&j, "error/code").unwrap_or_default();
            let code = if c.contains("AUTH") { ErrCode::AccountInvalid } else if c.contains("LINK_DOWN") || c.contains("LINK_IS_MISSING") { ErrCode::Offline } else if c.contains("MUST_BE_PREMIUM") || c.contains("NOT_SUPPORTED") { ErrCode::Unsupported } else if c.contains("LIMIT") { ErrCode::Quota } else { ErrCode::Retry };
            return Err(PluginError::new(code, format!("AllDebrid: {}", j_str(&j, "error/message").unwrap_or(c))));
        }
        let d = &j["data"];
        if let Some(l) = j_str(d, "link").filter(|l| !l.is_empty()) {
            return Ok(Some(ok_http(l, j_str(d, "filename"), j_i64(d, "filesize"))));
        }
        if let Some(delayed) = j_i64(d, "delayed") {
            for _ in 0..60 {
                ctx.wait(5000, "AllDebrid is preparing the file").await?;
                let r = ctx.get_h(&format!("https://api.alldebrid.com/v4/link/delayed?agent={AD}&id={delayed}"), bearer(&t)).await?.json()?;
                match j_i64(&r, "data/status") {
                    Some(2) => if let Some(l) = j_str(&r, "data/link") { return Ok(Some(ok_http(l, j_str(d, "filename"), j_i64(d, "filesize")))) },
                    Some(3) => break,
                    _ => {}
                }
            }
        }
        Err(PluginError::new(ErrCode::Retry, "AllDebrid: delayed link failed"))
    }
}

// ---- Premiumize --------------------------------------------------------------------------------
struct Premiumize {
    meta: Meta,
}
#[async_trait]
impl Plugin for Premiumize {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn is_multihost(&self) -> bool {
        true
    }
    fn has_login(&self) -> bool {
        true
    }
    async fn login(&self, ctx: &mut Ctx, acc: &PluginAccount) -> PResult<Option<AccountInfo>> {
        let t = token(acc)?;
        let a = ctx.page(&format!("https://www.premiumize.me/api/account/info?apikey={t}")).await?.json()?;
        if a["status"] != "success" {
            return Ok(Some(AccountInfo { ok: false, message: j_str(&a, "message").or(Some("Login failed".into())), ..Default::default() }));
        }
        let h = ctx.page(&format!("https://www.premiumize.me/api/services/list?apikey={t}")).await?.json()?;
        let until = j_i64(&a, "premium_until").map(|s| s * 1000);
        Ok(Some(AccountInfo {
            ok: true,
            premium: until.is_some_and(|u| u > crate::model::now_ms()),
            valid_until: until,
            supported_hosts: h["directdl"].as_array().into_iter().flatten().filter_map(|d| d.as_str().map(str::to_string)).collect(),
            ..Default::default()
        }))
    }
    async fn unrestrict(&self, ctx: &mut Ctx, acc: &PluginAccount) -> PResult<Option<Resolution>> {
        let t = token(acc)?;
        let url = ctx.url.clone();
        let j = ctx.post("https://www.premiumize.me/api/transfer/directdl", &[("apikey".into(), t), ("src".into(), url)], false).await?.json()?;
        let f = &j["content"][0];
        match (j["status"] == "success", j_str(f, "link")) {
            (true, Some(l)) => Ok(Some(ok_http(l, j_str(f, "path").map(|p| p.rsplit('/').next().unwrap_or(&p).to_string()), j_i64(f, "size")))),
            _ => Err(PluginError::new(ErrCode::Retry, format!("Premiumize: {}", j_str(&j, "message").unwrap_or("directdl failed".into())))),
        }
    }
}

// ---- TorBox (web downloads) ---------------------------------------------------------------------
struct TorBox {
    meta: Meta,
}
#[async_trait]
impl Plugin for TorBox {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    fn is_multihost(&self) -> bool {
        true
    }
    fn has_login(&self) -> bool {
        true
    }
    async fn login(&self, ctx: &mut Ctx, acc: &PluginAccount) -> PResult<Option<AccountInfo>> {
        let t = token(acc)?;
        let u = ctx.get_h("https://api.torbox.app/v1/api/user/me", bearer(&t)).await?.json()?;
        if u["success"] != Value::Bool(true) {
            return Ok(Some(AccountInfo { ok: false, message: j_str(&u, "detail").or(Some("Login failed".into())), ..Default::default() }));
        }
        let h = ctx.get_h("https://api.torbox.app/v1/api/webdl/hosters", bearer(&t)).await?.json().unwrap_or_default();
        let hosts = h["data"].as_array().into_iter().flatten().filter(|x| x["status"] != Value::Bool(false))
            .flat_map(|x| x["domains"].as_array().cloned().unwrap_or_else(|| x.get("domain").cloned().into_iter().collect()))
            .filter_map(|d| d.as_str().map(str::to_string)).collect();
        Ok(Some(AccountInfo {
            ok: true,
            premium: j_i64(&u, "data/plan").unwrap_or(0) > 0,
            valid_until: j_str(&u, "data/premium_expires_at").and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok()).map(|d| d.timestamp_millis()),
            message: j_str(&u, "data/email"),
            supported_hosts: hosts,
            ..Default::default()
        }))
    }
    async fn unrestrict(&self, ctx: &mut Ctx, acc: &PluginAccount) -> PResult<Option<Resolution>> {
        let t = token(acc)?;
        let url = ctx.url.clone();
        let c = ctx.post_h("https://api.torbox.app/v1/api/webdl/createwebdownload", &[("link".into(), url)], bearer(&t)).await?.json()?;
        let id = j_i64(&c, "data/webdownload_id").ok_or_else(|| PluginError::new(ErrCode::Retry, format!("TorBox: {}", j_str(&c, "detail").unwrap_or("create failed".into()))))?;
        for _ in 0..120 {
            let l = ctx.page(&format!("https://api.torbox.app/v1/api/webdl/requestdl?token={t}&web_id={id}&zip_link=false")).await?.json()?;
            if let Some(u) = j_str(&l, "data").filter(|_| l["success"] == Value::Bool(true)) {
                return Ok(Some(ok_http(u, None, None)));
            }
            ctx.wait(5000, "TorBox is fetching the file").await?;
        }
        Err(PluginError::new(ErrCode::Retry, "TorBox: timed out waiting for the web download"))
    }
}
