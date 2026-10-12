//! MEGA (JD2: MegaNz.java + MegaNzFolder.java, ~3,000 lines incl. account login).
//!   file key (32 bytes, base64url in the URL fragment) -> a32[8]; AES key k = a[0..3] ^ a[4..7]
//!   CTR IV = [a4, a5, 0, 0]; attributes = AES-CBC(k, zero IV) of 'MEGA{"n":"name"}'
//!   API: POST https://g.api.mega.co.nz/cs?id=<seq>[&n=<folder>]  [{a:'g', g:1, ssl:2, p|n:<handle>}]
//! Fetchwing downloads the ciphertext with the normal segmented engine, then decrypts IN PLACE
//! (CTR mode is seekable, so an interrupted decrypt resumes from where it stopped).

use super::*;
use aes::cipher::{BlockDecrypt, BlockDecryptMut, KeyInit, KeyIvInit, StreamCipher, StreamCipherSeek};
use base64::Engine;
use std::sync::atomic::{AtomicU64, Ordering};

const API: &str = "https://g.api.mega.co.nz/cs";
static SEQ: AtomicU64 = AtomicU64::new(0);

pub fn all() -> Vec<P> {
    vec![
        Arc::new(MegaFolder { meta: Meta::crawler("mega-folder", "MEGA folders", &["mega.nz", "mega.co.nz"], &[r"(?i)^https?://(?:www\.)?mega(?:\.co)?\.nz/(?:folder/[\w-]{8}#[\w-]{22}/?$|#F![\w-]{8}![\w-]{22})"]) }),
        Arc::new(Mega { meta: Meta::host("mega", "MEGA", &["mega.nz", "mega.co.nz", "mega.io"], &[r"(?i)^https?://(?:www\.)?mega(?:\.co)?\.nz/(?:file/[\w-]{8}#[\w-]{43}|#![\w-]{8}![\w-]{43}|folder/[\w-]{8}#[\w-]{22}/file/[\w-]{8})"]).limits(Limits { max_conns: Some(6), max_downloads: Some(2), ..Default::default() }) }),
    ]
}
struct Mega {
    meta: Meta,
}
struct MegaFolder {
    meta: Meta,
}

fn b64u(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim_end_matches('=').replace('+', "-").replace('/', "_")).unwrap_or_default()
}
fn to_a32(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4).map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect()
}
fn from_a32(a: &[u32]) -> Vec<u8> {
    a.iter().flat_map(|v| v.to_be_bytes()).collect()
}

pub fn key_parts(a: &[u32]) -> PResult<([u32; 4], [u32; 4])> {
    match a.len() {
        8 => Ok(([a[0] ^ a[4], a[1] ^ a[5], a[2] ^ a[6], a[3] ^ a[7]], [a[4], a[5], 0, 0])),
        4 => Ok(([a[0], a[1], a[2], a[3]], [0; 4])),
        _ => Err(defect("Invalid MEGA key")),
    }
}

fn decrypt_attr(at: &str, key: &[u32; 4]) -> PResult<String> {
    let mut data = b64u(at);
    data.truncate(data.len() / 16 * 16);
    let dec = cbc::Decryptor::<aes::Aes128>::new_from_slices(&from_a32(key), &[0u8; 16]).map_err(|_| defect("bad key"))?;
    let out = dec.decrypt_padded_vec_mut::<cbc::cipher::block_padding::NoPadding>(&data).map_err(|_| defect("attr decrypt"))?;
    let s = String::from_utf8_lossy(&out).trim_end_matches('\0').to_string();
    let json = s.strip_prefix("MEGA").ok_or_else(|| PluginError::new(ErrCode::Defect, "Wrong MEGA key (attribute decryption failed)").hint("Check that the link includes the full #key part."))?;
    let v: Value = serde_json::from_str(json).map_err(|e| defect(&format!("attr json: {e}")))?;
    Ok(v["n"].as_str().unwrap_or("file").to_string())
}

fn decrypt_key(enc: &[u8], master: &[u32]) -> Vec<u32> {
    let cipher = aes::Aes128::new_from_slice(&from_a32(master)).expect("16-byte key");
    let mut out = enc.to_vec();
    for block in out.chunks_exact_mut(16) {
        cipher.decrypt_block(aes::Block::from_mut_slice(block));
    }
    to_a32(&out)
}

fn api_err(code: i64) -> PluginError {
    match code {
        -2 => defect("MEGA: bad arguments"),
        -9 => offline("File not found on MEGA"),
        -11 => premium_only("MEGA: access denied"),
        -16 => offline("MEGA: file was removed (ToS / user blocked)"),
        -17 => hoster_wait(3_600_000, "MEGA transfer quota exceeded"),
        -18 => hoster_wait(300_000, "MEGA: resource temporarily unavailable"),
        c => PluginError::new(ErrCode::Retry, format!("MEGA API error {c}")),
    }
}

async fn api(ctx: &mut Ctx, body: Value, folder: Option<&str>) -> PResult<Value> {
    for attempt in 0..6u32 {
        let url = format!("{API}?id={}{}", SEQ.fetch_add(1, Ordering::Relaxed), folder.map(|f| format!("&n={f}")).unwrap_or_default());
        let p = ctx.post_json(&url, &body, vec![]).await?;
        let j: Value = p.json()?;
        let code = j.as_i64().or_else(|| j[0].as_i64());
        match code {
            Some(-3) => ctx.wait(1000 * 2u64.pow(attempt), "MEGA API busy").await?,
            Some(c) if c < 0 => return Err(api_err(c)),
            _ => return Ok(j),
        }
    }
    Err(PluginError::new(ErrCode::Retry, "MEGA API keeps answering EAGAIN"))
}

pub struct Parsed {
    pub kind: &'static str,
    pub id: String,
    pub key: String,
    pub node: Option<String>,
}
pub fn parse_url(u: &str) -> Option<Parsed> {
    let r = crate::util::http::re;
    if let Some(c) = r(r"mega(?:\.co)?\.nz/folder/([\w-]{8})#([\w-]{22})/file/([\w-]{8})").captures(u) {
        return Some(Parsed { kind: "folderfile", id: c[1].into(), key: c[2].into(), node: Some(c[3].into()) });
    }
    if let Some(c) = r(r"mega(?:\.co)?\.nz/(?:folder/([\w-]{8})#([\w-]{22})|#F!([\w-]{8})!([\w-]{22}))").captures(u) {
        let id = c.get(1).or(c.get(3))?.as_str();
        let key = c.get(2).or(c.get(4))?.as_str();
        return Some(Parsed { kind: "folder", id: id.into(), key: key.into(), node: None });
    }
    if let Some(c) = r(r"mega(?:\.co)?\.nz/(?:file/([\w-]{8})#([\w-]{43})|#!([\w-]{8})!([\w-]{43}))").captures(u) {
        let id = c.get(1).or(c.get(3))?.as_str();
        let key = c.get(2).or(c.get(4))?.as_str();
        return Some(Parsed { kind: "file", id: id.into(), key: key.into(), node: None });
    }
    None
}

struct Node {
    h: String,
    p: String,
    t: i64,
    s: Option<i64>,
}

async fn folder_nodes(ctx: &mut Ctx, id: &str, key: &str) -> PResult<(Vec<Node>, std::collections::HashMap<String, Vec<u32>>, std::collections::HashMap<String, String>)> {
    let master = to_a32(&b64u(key));
    let res = api(ctx, serde_json::json!([{"a": "f", "c": 1, "ca": 1, "r": 1}]), Some(id)).await?;
    let mut nodes = vec![];
    let mut keys = std::collections::HashMap::new();
    let mut names = std::collections::HashMap::new();
    for n in res[0]["f"].as_array().into_iter().flatten() {
        let h = n["h"].as_str().unwrap_or("").to_string();
        let t = n["t"].as_i64().unwrap_or(0);
        let kpart = n["k"].as_str().unwrap_or("").split('/').find_map(|x| x.split_once(':').map(|y| y.1.to_string()));
        if let Some(kp) = kpart {
            let full = decrypt_key(&b64u(&kp), &master);
            let ak = if t == 0 { key_parts(&full).map(|x| x.0).ok() } else { full.get(0..4).map(|s| [s[0], s[1], s[2], s[3]]) };
            if let Some(ak) = ak {
                names.insert(h.clone(), decrypt_attr(n["a"].as_str().unwrap_or(""), &ak).unwrap_or_else(|_| h.clone()));
            }
            keys.insert(h.clone(), full);
        }
        nodes.push(Node { h, p: n["p"].as_str().unwrap_or("").to_string(), t, s: n["s"].as_i64() });
    }
    Ok((nodes, keys, names))
}

#[async_trait]
impl Plugin for MegaFolder {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn crawl(&self, ctx: &mut Ctx) -> PResult<CrawlResult> {
        let p = parse_url(&ctx.url).ok_or_else(|| defect("Unrecognised MEGA folder URL"))?;
        let (nodes, keys, names) = folder_nodes(ctx, &p.id, &p.key).await?;
        let by: std::collections::HashMap<&str, &Node> = nodes.iter().map(|n| (n.h.as_str(), n)).collect();
        let root = nodes.iter().find(|n| !by.contains_key(n.p.as_str()));
        let path_of = |n: &Node| {
            let mut parts = vec![];
            let mut cur = by.get(n.p.as_str());
            while let Some(c) = cur {
                if Some(c.h.as_str()) == root.map(|r| r.h.as_str()) {
                    break;
                }
                parts.insert(0, names.get(&c.h).cloned().unwrap_or(c.h.clone()));
                cur = by.get(c.p.as_str());
            }
            parts.join("/")
        };
        let items = nodes.iter().filter(|n| n.t == 0 && keys.contains_key(&n.h)).map(|n| {
            let mut it = CrawledItem::file(format!("https://mega.nz/folder/{}#{}/file/{}", p.id, p.key, n.h), names.get(&n.h).cloned(), n.s);
            let sub = path_of(n);
            it.subdir = (!sub.is_empty()).then_some(sub);
            it.props.insert("megaKey".into(), serde_json::json!(keys[&n.h]));
            it
        }).collect();
        Ok(CrawlResult { items, package_name: root.and_then(|r| names.get(&r.h).cloned()).or(Some(p.id)), ..Default::default() })
    }
}

struct Info {
    name: String,
    size: i64,
    url: Option<String>,
    a32: Vec<u32>,
}

async fn fetch_info(ctx: &mut Ctx, want_url: bool) -> PResult<Info> {
    let p = parse_url(&ctx.url).ok_or_else(|| defect("Unrecognised MEGA URL"))?;
    let (a32, res) = if p.kind == "file" {
        let a = to_a32(&b64u(&p.key));
        let r = api(ctx, serde_json::json!([{"a": "g", "g": want_url as i32, "ssl": 2, "p": p.id}]), None).await?;
        (a, r)
    } else {
        let stored: Option<Vec<u32>> = ctx.prop("megaKey").and_then(|v| serde_json::from_value(v).ok());
        let a = match stored {
            Some(a) => a,
            None => folder_nodes(ctx, &p.id, &p.key).await?.1.get(p.node.as_deref().unwrap_or("")).cloned().ok_or_else(|| offline("File not found in MEGA folder"))?,
        };
        let r = api(ctx, serde_json::json!([{"a": "g", "g": want_url as i32, "ssl": 2, "n": p.node}]), Some(&p.id)).await?;
        (a, r)
    };
    let r = &res[0];
    if let Some(e) = r["e"].as_i64().filter(|e| *e < 0) {
        return Err(api_err(e));
    }
    let (k, _) = key_parts(&a32)?;
    Ok(Info { name: decrypt_attr(r["at"].as_str().unwrap_or(""), &k)?, size: r["s"].as_i64().unwrap_or(-1), url: r["g"].as_str().map(str::to_string), a32 })
}

#[async_trait]
impl Plugin for Mega {
    fn meta(&self) -> &Meta {
        &self.meta
    }
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> {
        match fetch_info(ctx, false).await {
            Ok(i) => Ok(Some(LinkInfo::online(Some(i.name), Some(i.size)))),
            Err(e) if e.code == ErrCode::Offline => Ok(Some(LinkInfo::offline())),
            Err(e) => Err(e),
        }
    }
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> {
        let i = fetch_info(ctx, true).await?;
        let url = i.url.ok_or_else(|| defect("MEGA did not return a download URL"))?;
        let (key, iv) = key_parts(&i.a32)?;
        Ok(Resolution::Http { req: DirectRequest::get(url).conns(6), name: Some(i.name), size: Some(i.size), hash: None, post: vec![PostStep::MegaDecrypt { key, iv }] })
    }
}

/// In-place AES-128-CTR decryption; `start_at` resumes an interrupted pass.
pub fn decrypt_in_place(path: &std::path::Path, key: [u32; 4], iv: [u32; 4], start_at: u64, cancel: &CancellationToken, progress: &dyn Fn(u64)) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut c = ctr::Ctr128BE::<aes::Aes128>::new_from_slices(&from_a32(&key), &from_a32(&iv)).map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut pos = start_at - start_at % 16;
    c.seek(pos);
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        f.seek(SeekFrom::Start(pos))?;
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        c.apply_keystream(&mut buf[..n]);
        f.seek(SeekFrom::Start(pos))?;
        f.write_all(&buf[..n])?;
        pos += n as u64;
        progress(pos);
    }
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ctr_roundtrip_with_resume() {
        let dir = std::env::temp_dir().join(format!("fw-mega-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x");
        let plain: Vec<u8> = (0..1_000_003u32).map(|i| (i % 251) as u8).collect();
        let key = [1, 2, 3, 4];
        let iv = [5, 6, 0, 0];
        let mut enc = plain.clone();
        let mut c = ctr::Ctr128BE::<aes::Aes128>::new_from_slices(&from_a32(&key), &from_a32(&iv)).unwrap();
        c.apply_keystream(&mut enc);
        // simulate: first 320000 bytes already decrypted, then resume
        let mut half = enc.clone();
        half[..320000].copy_from_slice(&plain[..320000]);
        std::fs::write(&p, &half).unwrap();
        decrypt_in_place(&p, key, iv, 320000, &CancellationToken::new(), &|_| {}).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), plain);
        assert!(parse_url("https://mega.nz/file/abcdefgh#AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
