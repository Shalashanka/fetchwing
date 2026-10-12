//! Engine tests against a local server that supports ranges, ignores them, drops connections and
//! rate-limits connections.

use crate::engine::*;
use crate::util::http::{Browser, NetOpts};
use crate::util::rate::TokenBucket;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::get;
use parking_lot::Mutex;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Srv {
    data: Vec<u8>,
    active: AtomicUsize,
    peak: AtomicUsize,
}

struct Guard(Arc<Srv>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn serve(State(s): State<Arc<Srv>>, Path(mode): Path<String>, h: HeaderMap) -> Response<Body> {
    let n = s.active.fetch_add(1, Ordering::SeqCst) + 1;
    s.peak.fetch_max(n, Ordering::SeqCst);
    let guard = Guard(s.clone());
    if mode == "limit2" && n > 2 {
        return Response::builder().status(429).body(Body::empty()).unwrap();
    }
    let range = h.get("range").and_then(|v| v.to_str().ok()).and_then(|r| {
        let c = regex::Regex::new(r"bytes=(\d+)-(\d*)").unwrap().captures(r)?;
        Some((c[1].parse::<usize>().ok()?, c[2].parse::<usize>().ok()))
    });
    let len = s.data.len();
    if mode == "norange" || range.is_none() {
        let data = s.data.clone();
        let stream = futures_util::stream::iter(data.chunks(65536).map(|c| Ok::<_, std::io::Error>(c.to_vec())).collect::<Vec<_>>());
        let st = futures_util::StreamExt::map(stream, move |x| { let _g = &guard; x });
        return Response::builder().status(200).header("content-length", len).header("content-disposition", "attachment; filename=\"norange.bin\"").body(Body::from_stream(st)).unwrap();
    }
    let (start, end) = range.unwrap();
    let end = end.unwrap_or(len - 1).min(len - 1);
    let slice = s.data[start..=end].to_vec();
    let flaky = mode == "flaky" && slice.len() > 1024 * 1024 && rand::random::<f32>() < 0.6;
    let cut = if flaky { slice.len() / 3 } else { slice.len() };
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> = slice[..cut]
        .chunks(65536)
        .map(|c| Ok(c.to_vec()))
        .chain(if flaky { vec![Err(std::io::Error::other("cut"))] } else { vec![] })
        .collect();
    let stream = futures_util::stream::iter(chunks).then(|c| async move {
        tokio::task::yield_now().await;
        c
    });
    let st = futures_util::StreamExt::map(stream, move |x| { let _g = &guard; x });
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header("content-range", format!("bytes {start}-{end}/{len}"))
        .header("content-length", end - start + 1)
        .header("etag", "\"v1\"")
        .body(Body::from_stream(st))
        .unwrap()
}
use futures_util::StreamExt;

async fn server() -> (String, Arc<Srv>) {
    let mut data = vec![0u8; 12 * 1024 * 1024 + 12345];
    rand::thread_rng().fill_bytes(&mut data);
    let s = Arc::new(Srv { data, active: AtomicUsize::new(0), peak: AtomicUsize::new(0) });
    let app = axum::Router::new().route("/{mode}", get(serve)).with_state(s.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (format!("http://{addr}"), s)
}

fn job(base: &str, mode: &str, dir: &std::path::Path, name: &str, state: Arc<Mutex<DlState>>, limiters: Vec<Arc<TokenBucket>>, cancel: CancellationToken) -> (DlJob, Arc<AtomicUsize>) {
    let progress = Arc::new(AtomicUsize::new(0));
    let p2 = progress.clone();
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let b = Browser::new(jar, &format!("test-{name}"), NetOpts { user_agent: "fw-test".into(), timeout_s: 20, proxy: String::new() });
    (
        DlJob {
            req: DirectRequest::get(format!("{base}/{mode}")),
            browser: b,
            part_path: dir.join(format!("{name}.part")),
            state,
            connections: 8,
            min_segment: 256 * 1024,
            preallocate: true,
            limiters,
            cancel,
            on_progress: Arc::new(move |n| { p2.fetch_add(n as usize, Ordering::SeqCst); }),
            on_meta: Arc::new(|_| {}),
            persist: Arc::new(|_| {}),
        },
        progress,
    )
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn fresh() -> Arc<Mutex<DlState>> {
    Arc::new(Mutex::new(DlState::new(vec![], -1, None, None, false)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn engine_suite() {
    let (base, srv) = server().await;
    let dir = std::env::temp_dir().join(format!("fw-test-{}", crate::model::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let want = sha(&srv.data);

    // 1. multi-connection, byte-exact
    let st = fresh();
    let (j, _) = job(&base, "ok", &dir, "a", st.clone(), vec![], CancellationToken::new());
    download(&j).await.expect("ok download");
    assert_eq!(sha(&std::fs::read(&j.part_path).unwrap()), want);
    assert!(st.lock().segments.len() > 1, "should split");
    assert!(srv.peak.load(Ordering::SeqCst) >= 4, "parallel connections expected");

    // 2. no range support
    let st = fresh();
    let (j, _) = job(&base, "norange", &dir, "b", st.clone(), vec![], CancellationToken::new());
    download(&j).await.expect("norange");
    assert_eq!(sha(&std::fs::read(&j.part_path).unwrap()), want);
    assert!(!st.lock().resumable);

    // 3. flaky server
    for i in 0..3 {
        let st = fresh();
        let (j, _) = job(&base, "flaky", &dir, &format!("c{i}"), st.clone(), vec![], CancellationToken::new());
        download(&j).await.expect("flaky");
        assert_eq!(sha(&std::fs::read(&j.part_path).unwrap()), want, "flaky run {i}");
    }

    // 4. server limiting connections
    let st = fresh();
    let (j, _) = job(&base, "limit2", &dir, "d", st.clone(), vec![], CancellationToken::new());
    download(&j).await.expect("limit2");
    assert_eq!(sha(&std::fs::read(&j.part_path).unwrap()), want);

    // 5. abort + resume
    let st = fresh();
    let cancel = CancellationToken::new();
    let lim = Arc::new(TokenBucket::new(4 * 1024 * 1024));
    let (j, _) = job(&base, "ok", &dir, "e", st.clone(), vec![lim], cancel.clone());
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        c2.cancel();
    });
    let r = download(&j).await;
    assert!(matches!(r, Err(ref e) if e.kind == DlKind::Cancelled), "expected cancel, got {r:?}");
    let before = st.lock().bytes_done();
    assert!(before > 0 && (before as usize) < srv.data.len(), "partial progress {before}");
    let (j2, prog) = job(&base, "ok", &dir, "e", st.clone(), vec![], CancellationToken::new());
    download(&j2).await.expect("resume");
    assert_eq!(sha(&std::fs::read(&j2.part_path).unwrap()), want);
    assert!(prog.load(Ordering::SeqCst) <= srv.data.len() - before as usize + 1, "resume must not re-download finished bytes");

    // 6. speed limit
    let st = fresh();
    let lim = Arc::new(TokenBucket::new(6 * 1024 * 1024));
    let (j, _) = job(&base, "ok", &dir, "f", st.clone(), vec![lim], CancellationToken::new());
    let t0 = std::time::Instant::now();
    download(&j).await.expect("limited");
    let rate = srv.data.len() as f64 / t0.elapsed().as_secs_f64();
    assert!(rate < 6.0 * 1024.0 * 1024.0 * 1.35, "rate {} MB/s too high", rate / 1048576.0);
    assert_eq!(sha(&std::fs::read(&j.part_path).unwrap()), want);

    let _ = std::fs::remove_dir_all(&dir);
}
