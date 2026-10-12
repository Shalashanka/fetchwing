//! LinkCrawler: text -> URLs -> crawler plugins (recursively) -> links handled by host plugins.
//! JD2's LinkCrawler.java is 4,463 lines. Same idea: depth-limited, deduped, bounded concurrency,
//! a generic page scan for unknown pages, yt-dlp as last resort. Plus the packagizer heuristics.

use crate::plugins::generic::probe_with_fallback;
use crate::plugins::{Avail, Ctx, ErrCode, P, Registry};
use crate::util::http::{decode_entities, re};
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

pub const FILE_EXT: &str = r"(?i)\.(?:7z|zip|rar|r\d{2}|tar|gz|tgz|bz2|xz|zst|iso|img|dmg|exe|msi|apk|deb|rpm|appimage|pdf|epub|mobi|cbz|cbr|mp4|mkv|avi|mov|webm|m4v|mp3|flac|wav|ogg|opus|m4a|aac|jpg|jpeg|png|gif|webp|bin|dat|torrent|\d{3})(?:$|[?#])";

/// Pull URLs out of anything: plain text, HTML, BBCode, obfuscated "hxxp".
pub fn extract_urls(input: &str) -> Vec<String> {
    let text = decode_entities(input);
    let text = re(r#"(?i)<a\s[^>]*href\s*=\s*["']([^"']+)["'][^>]*>"#).replace_all(&text, " $1 ");
    let text = re(r"(?i)\[url=([^\]]+)\]").replace_all(&text, " $1 ");
    let mut out: Vec<String> = vec![];
    let mut seen = HashSet::new();
    for m in re(r#"(?i)\b(?:h(?:tt|xx|\*\*)ps?|ftp)://[^\s<>"'`{}|\\^\[\]]+"#).find_iter(&text) {
        let mut u = m.as_str().to_string();
        if !u[..4].eq_ignore_ascii_case("http") && !u[..3].eq_ignore_ascii_case("ftp") {
            u = format!("http{}", &u[4..]);
        }
        let u = trim_trailing(&u);
        if url::Url::parse(&u).is_ok_and(|x| x.host_str().is_some_and(|h| h.contains('.') || h == "localhost")) && seen.insert(u.clone()) {
            out.push(u);
        }
    }
    // bare well-known hosts without scheme
    for c in re(r#"(?i)(?:^|[\s(\[<"'])((?:www\.)?(?:mega\.nz|pixeldrain\.com|gofile\.io|mediafire\.com|1fichier\.com|drive\.google\.com|youtu\.be|youtube\.com|dropbox\.com|github\.com|archive\.org)/[^\s<>"'`]+)"#).captures_iter(&text) {
        let u = format!("https://{}", trim_trailing(&c[1]));
        if !out.iter().any(|o| o.contains(&c[1])) && seen.insert(u.clone()) {
            out.push(u);
        }
    }
    out
}

fn trim_trailing(u: &str) -> String {
    let mut s = u.to_string();
    loop {
        let Some(last) = s.chars().last() else { break };
        if ".,;:!?'\"»".contains(last) {
            s.pop();
            continue;
        }
        if last == ')' && s.matches('(').count() < s.matches(')').count() {
            s.pop();
            continue;
        }
        break;
    }
    s
}

/// Headers worth replaying from a browser request (the rest is transport noise or would break ranges).
const DROP_HEADERS: &[&str] = &["range", "accept-encoding", "content-length", "host", "connection", "if-range", "if-none-match", "if-modified-since", "priority", "upgrade-insecure-requests"];

/// Parse `curl ...` commands as produced by Chrome/Firefox/Edge "Copy as cURL" (bash or cmd
/// flavour). Returns (url, headers) per command plus the remaining text.
pub fn parse_curl_commands(text: &str) -> (Vec<(String, Vec<(String, String)>)>, String) {
    // cmd flavour: ^" quotes, ^ + newline continuations, ^x escapes; bash: \ + newline
    let norm = text
        .replace("^\r\n", " ")
        .replace("^\n", " ")
        .replace("^\\^\"", "\\\"")
        .replace("^\"", "\"")
        .replace("^^", "^")
        .replace("^%", "%")
        .replace("^&", "&")
        .replace("^<", "<")
        .replace("^>", ">")
        .replace("^|", "|")
        .replace("\\\r\n", " ")
        .replace("\\\n", " ");
    let mut out = vec![];
    let mut rest = String::new();
    for (n, part) in re(r"(?m)^\s*curl(?:\.exe)?\s").split(&norm).enumerate() {
        if n == 0 {
            rest.push_str(part);
            continue;
        }
        // a command is one (normalized) line; anything after it is ordinary text
        let (cmd, after) = part.split_once('\n').unwrap_or((part, ""));
        rest.push('\n');
        rest.push_str(after);
        let toks = shell_words(cmd);
        let mut url = None;
        let mut headers: Vec<(String, String)> = vec![];
        let mut i = 0;
        while i < toks.len() {
            let t = toks[i].as_str();
            let next = toks.get(i + 1).cloned();
            match t {
                "-H" | "--header" => {
                    if let Some((k, v)) = next.as_deref().and_then(|h| h.split_once(':')) {
                        let k = k.trim().to_ascii_lowercase();
                        if !DROP_HEADERS.contains(&k.as_str()) {
                            headers.push((k, v.trim().to_string()));
                        }
                    }
                    i += 2;
                }
                "-b" | "--cookie" => {
                    if let Some(c) = next {
                        headers.push(("cookie".into(), c));
                    }
                    i += 2;
                }
                "-A" | "--user-agent" => {
                    if let Some(a) = next {
                        headers.push(("user-agent".into(), a));
                    }
                    i += 2;
                }
                "-e" | "--referer" => {
                    if let Some(r) = next {
                        headers.push(("referer".into(), r));
                    }
                    i += 2;
                }
                "--url" => {
                    url = next;
                    i += 2;
                }
                "-X" | "--request" | "-d" | "--data" | "--data-raw" | "--data-binary" | "-o" | "--output" | "-u" | "--user" | "-x" | "--proxy" => i += 2,
                s if s.starts_with('-') => i += 1,
                s if url.is_none() && (s.starts_with("http://") || s.starts_with("https://")) => {
                    url = Some(s.to_string());
                    i += 1;
                }
                _ => i += 1,
            }
        }
        if let Some(u) = url {
            out.push((u, headers));
        }
    }
    (out, rest)
}

/// Minimal POSIX-ish word splitting: '...' literal, "..." with \ escapes, \x escapes.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut has = false;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                has = true;
                for d in it.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    cur.push(d);
                }
            }
            '"' => {
                has = true;
                while let Some(d) = it.next() {
                    match d {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = it.next() {
                                cur.push(e);
                            }
                        }
                        _ => cur.push(d),
                    }
                }
            }
            '\\' => {
                if let Some(e) = it.next() {
                    cur.push(e);
                    has = true;
                }
            }
            '$' if it.peek() == Some(&'\'') => {}
            c if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub fn extract_page_links(html: &str, base: &str) -> Vec<String> {
    let mut out = vec![];
    let mut seen = HashSet::new();
    let b = url::Url::parse(base).ok();
    for c in re(r#"(?i)(?:href|src|data-src|data-url)\s*=\s*["']([^"'#][^"']*)["']"#).captures_iter(html) {
        if let Some(u) = b.as_ref().and_then(|b| b.join(&decode_entities(&c[1])).ok()) {
            if (u.scheme() == "http" || u.scheme() == "https") && seen.insert(u.to_string()) {
                out.push(u.to_string());
            }
        }
    }
    for u in extract_urls(html) {
        if seen.insert(u.clone()) {
            out.push(u);
        }
    }
    out
}

/// "Password: xyz" near links on forum pages.
pub fn find_passwords(text: &str) -> Vec<String> {
    let plain = re(r"<[^>]+>").replace_all(text, " ");
    let mut out: Vec<String> = vec![];
    for c in re(r#"(?i)\b(?:password|passwort|pass|pw|mot de passe)\s*[:=]\s*([^\s<>"']{3,64})"#).captures_iter(&plain) {
        if !out.contains(&c[1].to_string()) {
            out.push(c[1].to_string());
        }
    }
    out
}

// ---- packagizer --------------------------------------------------------------------------------

const ARCHIVE_SUFFIX: &str = r"(?i)(?:\.part0*\d+\.rar|\.r\d{2,3}|\.rar|\.7z\.\d{3}|\.zip\.\d{3}|\.z\d{2}|\.tar\.(?:gz|bz2|xz|zst)|\.\d{3}|\.7z|\.zip|\.tgz)$";

pub fn archive_base(name: &str) -> Option<String> {
    let r = re(ARCHIVE_SUFFIX);
    r.is_match(name).then(|| r.replace(name, "").into_owned())
}

pub fn clean_package_name(s: &str) -> String {
    let t = re(r"[._]+").replace_all(s, " ");
    let t = re(r"\s+").replace_all(&t, " ");
    let t = t.trim().trim_end_matches(['-', ' ']).trim().to_string();
    if t.is_empty() { "Downloads".into() } else { t.chars().take(150).collect() }
}

pub struct Groupable {
    pub name: String,
    pub host: String,
    pub hint: Option<String>,
}

/// Package name per link (same name = same package): crawler hint > archive set > common prefix.
pub fn group_links(links: &[Groupable], forced: Option<&str>) -> Vec<String> {
    if let Some(f) = forced {
        return links.iter().map(|_| f.to_string()).collect();
    }
    let mut res = vec![String::new(); links.len()];
    let mut loose = vec![];
    for (i, l) in links.iter().enumerate() {
        if let Some(h) = &l.hint {
            res[i] = h.clone();
        } else if let Some(b) = archive_base(&l.name) {
            res[i] = clean_package_name(&b);
        } else {
            loose.push(i);
        }
    }
    if loose.len() == 1 {
        let n = &links[loose[0]].name;
        res[loose[0]] = clean_package_name(&re(r"\.[^.]{1,8}$").replace(n, ""));
    } else if loose.len() > 1 {
        let names: Vec<&str> = loose.iter().map(|&i| links[i].name.as_str()).collect();
        let mut prefix = names[0].to_string();
        for n in &names {
            while !n.starts_with(&prefix) {
                prefix.pop();
            }
        }
        let prefix = prefix.trim_end_matches([' ', '.', '_', '-']).to_string();
        let name = if prefix.chars().count() >= 4 { clean_package_name(&prefix) } else { format!("{} - {}", links[loose[0]].host, chrono::Local::now().format("%Y-%m-%d")) };
        for i in loose {
            res[i] = name.clone();
        }
    }
    res
}

// ---- crawling ----------------------------------------------------------------------------------

#[derive(Clone)]
pub struct Found {
    pub url: String,
    pub plugin: P,
    pub name: Option<String>,
    pub size: Option<i64>,
    pub status: Option<Avail>,
    pub subdir: Option<String>,
    pub props: Map<String, Value>,
    pub hint: Option<String>,
    pub passwords: Vec<String>,
    pub origin: Option<String>,
}

struct Item {
    url: String,
    depth: u32,
    hint: Option<String>,
    passwords: Vec<String>,
    origin: Option<String>,
    subdir: Option<String>,
    name: Option<String>,
    size: Option<i64>,
    props: Map<String, Value>,
}

pub trait CrawlSink: Send + Sync {
    fn links(&self, found: Vec<Found>);
    fn problem(&self, url: &str, msg: &str);
}

const MAX_DEPTH: u32 = 4;
const CONCURRENCY: usize = 4;

fn join_sub(a: &Option<String>, b: &Option<String>) -> Option<String> {
    let v: Vec<&str> = [a.as_deref(), b.as_deref()].into_iter().flatten().filter(|s| !s.is_empty()).collect();
    (!v.is_empty()).then(|| v.join("/"))
}

pub async fn run(
    registry: Arc<Registry>,
    text: String,
    passwords: Vec<String>,
    deep: bool,
    ytdlp: bool,
    make_ctx: Arc<dyn Fn(&str, Vec<String>) -> Ctx + Send + Sync>,
    sink: Arc<dyn CrawlSink>,
) -> (usize, usize) {
    // "Copy as cURL" from browser DevTools: keep the request's cookies/referer/user-agent
    let (curls, rest) = parse_curl_commands(&text);
    let mut items: Vec<Item> = curls
        .into_iter()
        .map(|(u, headers)| {
            let mut props = Map::new();
            if !headers.is_empty() {
                props.insert("headers".into(), Value::Object(headers.into_iter().map(|(k, v)| (k, Value::String(v))).collect()));
            }
            Item { url: u, depth: 0, hint: None, passwords: passwords.clone(), origin: None, subdir: None, name: None, size: None, props }
        })
        .collect();
    let plain: Vec<String> = extract_urls(&rest).into_iter().filter(|u| !items.iter().any(|i| &i.url == u)).collect();
    items.extend(plain.into_iter().map(|u| Item { url: u, depth: 0, hint: None, passwords: passwords.clone(), origin: None, subdir: None, name: None, size: None, props: Map::new() }));
    if items.is_empty() {
        sink.problem("", "No links found in the text");
        return (0, 1);
    }
    let queue = Arc::new(Mutex::new(items.into_iter().collect::<VecDeque<_>>()));
    let seen = Arc::new(Mutex::new(HashSet::<String>::new()));
    let counts = Arc::new(Mutex::new((0usize, 0usize)));
    let mut set = tokio::task::JoinSet::new();
    loop {
        while set.len() < CONCURRENCY {
            let Some(it) = queue.lock().pop_front() else { break };
            if !seen.lock().insert(it.url.trim_end_matches('/').to_string()) {
                continue;
            }
            let (reg, q, sink, mk, counts) = (registry.clone(), queue.clone(), sink.clone(), make_ctx.clone(), counts.clone());
            set.spawn(async move { process(it, reg, q, sink, mk, counts, deep, ytdlp).await });
        }
        if set.join_next().await.is_none() && queue.lock().is_empty() {
            break;
        }
    }
    let c = *counts.lock();
    c
}

#[allow(clippy::too_many_arguments)]
async fn process(it: Item, reg: Arc<Registry>, queue: Arc<Mutex<VecDeque<Item>>>, sink: Arc<dyn CrawlSink>, mk: Arc<dyn Fn(&str, Vec<String>) -> Ctx + Send + Sync>, counts: Arc<Mutex<(usize, usize)>>, deep: bool, ytdlp: bool) {
    let found = |f: Vec<Found>| {
        counts.lock().0 += f.len();
        sink.links(f);
    };
    let problem = |u: &str, m: &str| {
        counts.lock().1 += 1;
        sink.problem(u, m);
    };
    let mkf = |url: String, plugin: P, it: &Item, hint: Option<String>, pw: Vec<String>, sub: Option<String>| Found {
        url, plugin, name: it.name.clone(), size: it.size, status: None, subdir: sub, props: it.props.clone(), hint, passwords: pw, origin: it.origin.clone(),
    };
    // 1. crawler plugin
    if let Some(cr) = reg.find_crawler(&it.url).filter(|_| it.depth < MAX_DEPTH) {
        let mut ctx = mk(&it.url, it.passwords.clone());
        match cr.crawl(&mut ctx).await {
            Ok(res) => {
                let mut pw = it.passwords.clone();
                pw.extend(res.passwords.clone());
                let hint = res.package_name.clone().or(it.hint.clone());
                let mut direct = vec![];
                for c in res.items {
                    let sub = join_sub(&it.subdir, &c.subdir);
                    let host = reg.find_host(&c.url);
                    let child_cr = reg.find_crawler(&c.url);
                    match host {
                        Some(h) if child_cr.as_ref().is_none_or(|x| x.meta().id == cr.meta().id) => direct.push(Found {
                            url: c.url, plugin: h, name: c.name, size: c.size, status: c.online.then_some(Avail::Online), subdir: sub, props: c.props, hint: hint.clone(), passwords: pw.clone(), origin: Some(it.url.clone()),
                        }),
                        _ => queue.lock().push_back(Item { url: c.url, depth: it.depth + 1, hint: hint.clone(), passwords: pw.clone(), origin: Some(it.url.clone()), subdir: sub, name: c.name, size: c.size, props: c.props }),
                    }
                }
                if !direct.is_empty() {
                    found(direct);
                }
            }
            Err(e) => problem(&it.url, &e.to_string()),
        }
        return;
    }
    // 2. dedicated host plugin
    if let Some(h) = reg.find_host(&it.url).filter(|h| h.meta().id != "generic-http") {
        found(vec![mkf(it.url.clone(), h, &it, it.hint.clone(), it.passwords.clone(), it.subdir.clone())]);
        return;
    }
    if !it.url.to_ascii_lowercase().starts_with("http") {
        problem(&it.url, "Unsupported link type");
        return;
    }
    let generic = reg.get("generic-http").expect("generic plugin");
    // 3. unknown http(s): direct file or web page?
    let mut ctx = mk(&it.url, it.passwords.clone());
    ctx.props.lock().extend(it.props.clone());
    let probe = probe_with_fallback(&mut ctx.browser, &it.url, &ctx.props).await;
    let props = ctx.props.lock().clone();
    let path_is_file = url::Url::parse(&it.url).map(|u| re(FILE_EXT).is_match(u.path())).unwrap_or(false);
    match probe {
        Err(e) => {
            // unreachable right now -> keep it as "unknown"; the downloader retries with backoff
            crate::log::debug("crawler", &format!("probe {}: {e}", it.url));
            let mut f = mkf(it.url.clone(), generic, &it, it.hint.clone(), it.passwords.clone(), it.subdir.clone());
            f.status = Some(Avail::Unknown);
            f.props.extend(props);
            found(vec![f]);
        }
        Ok(p) if !p.is_html || path_is_file => {
            let mut f = mkf(it.url.clone(), generic, &it, it.hint.clone(), it.passwords.clone(), it.subdir.clone());
            f.name = f.name.or(Some(p.name));
            f.size = f.size.or((p.size > 0).then_some(p.size));
            f.status = Some(if p.status < 400 { Avail::Online } else if p.status == 404 || p.status == 410 { Avail::Offline } else { Avail::Unknown });
            f.props.extend(props);
            found(vec![f]);
        }
        Ok(_) => {
            if !deep || it.depth >= MAX_DEPTH {
                problem(&it.url, "Web page (deep scan disabled)");
                return;
            }
            let page = match ctx.page(&it.url).await {
                Ok(p) => p,
                Err(e) => return problem(&it.url, &e.to_string()),
            };
            let fe = re(FILE_EXT);
            let links: Vec<String> = extract_page_links(&page.text, &page.url)
                .into_iter()
                .filter(|u| u != &it.url && u != &page.url && (reg.knows(u) || url::Url::parse(u).map(|x| fe.is_match(x.path())).unwrap_or(false)))
                .collect();
            if !links.is_empty() {
                let title = page.m(r"(?is)<title>\s*([^<]{3,120}?)\s*</title>").map(|t| decode_entities(&t));
                let mut pw = it.passwords.clone();
                pw.extend(find_passwords(&page.text));
                let mut q = queue.lock();
                for u in links {
                    q.push_back(Item { url: u, depth: it.depth + 1, hint: it.hint.clone().or(title.clone()), passwords: pw.clone(), origin: Some(it.url.clone()), subdir: it.subdir.clone(), name: None, size: None, props: Map::new() });
                }
            } else if ytdlp {
                if let Some(y) = reg.get("ytdlp") {
                    found(vec![mkf(it.url.clone(), y, &it, it.hint.clone(), it.passwords.clone(), it.subdir.clone())]);
                }
            } else {
                problem(&it.url, "No downloadable links found on this page");
            }
        }
    }
    let _ = ErrCode::Retry;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls() {
        let u = extract_urls("see https://a.com/x.zip, and (https://en.wikipedia.org/wiki/Foo_(bar)) hxxp://b.org/y mega.nz/file/abc#k");
        assert!(u.contains(&"https://a.com/x.zip".to_string()));
        assert!(u.contains(&"https://en.wikipedia.org/wiki/Foo_(bar)".to_string()));
        assert!(u.contains(&"http://b.org/y".to_string()));
        assert!(u.iter().any(|x| x == "https://mega.nz/file/abc#k"));
    }
    #[test]
    fn curl_paste() {
        let bash = "curl 'https://tunnel.example/download/abc?sig=x' \\\n  -H 'accept: */*' \\\n  -H 'referer: https://filekeeper.example/f/1' \\\n  -b 'sess=42; a=b' \\\n  -H 'range: bytes=0-' \\\n  -H 'user-agent: Mozilla/5.0 X'\nsome text after\n";
        let (c, rest) = parse_curl_commands(bash);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, "https://tunnel.example/download/abc?sig=x");
        let h: std::collections::HashMap<_, _> = c[0].1.iter().cloned().collect();
        assert_eq!(h["referer"], "https://filekeeper.example/f/1");
        assert_eq!(h["cookie"], "sess=42; a=b");
        assert_eq!(h["user-agent"], "Mozilla/5.0 X");
        assert!(!h.contains_key("range"));
        assert!(extract_urls(&rest).is_empty());
        let cmd = "curl ^\"https://tunnel.example/d/x?sig=y^\" ^\n  -H ^\"referer: https://fk.example/^\" ^\n  -b ^\"k=v^\"\n";
        let (c, _) = parse_curl_commands(cmd);
        assert_eq!(c[0].0, "https://tunnel.example/d/x?sig=y");
        assert!(c[0].1.contains(&("cookie".to_string(), "k=v".to_string())));
        let (c, rest) = parse_curl_commands("plain https://a.com/x.zip");
        assert!(c.is_empty() && rest.contains("https://a.com/x.zip"));
    }

    #[test]
    fn grouping() {
        let g = group_links(&[
            Groupable { name: "Movie.2024.part1.rar".into(), host: "h".into(), hint: None },
            Groupable { name: "Movie.2024.part2.rar".into(), host: "h".into(), hint: None },
            Groupable { name: "single.iso".into(), host: "h".into(), hint: None },
        ], None);
        assert_eq!(g[0], "Movie 2024");
        assert_eq!(g[1], "Movie 2024");
        assert_eq!(g[2], "single");
        assert_eq!(find_passwords("<b>Password:</b> s3cret <br>"), vec!["s3cret"]);
    }
}
