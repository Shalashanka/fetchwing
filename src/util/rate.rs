//! Token-bucket rate limiting and rolling speed meters.
//! A download draws from its own bucket and the global one. Rate 0 = unlimited.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub struct TokenBucket {
    inner: Mutex<Bucket>,
}

struct Bucket {
    rate: u64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(rate: u64) -> Self {
        TokenBucket { inner: Mutex::new(Bucket { rate, tokens: burst(rate), last: Instant::now() }) }
    }
    pub fn set_rate(&self, rate: u64) {
        let mut b = self.inner.lock();
        b.rate = rate;
        b.tokens = b.tokens.min(burst(rate));
    }
    pub fn rate(&self) -> u64 {
        self.inner.lock().rate
    }
    /// Wait until `n` bytes may be consumed.
    pub async fn take(&self, n: usize) {
        loop {
            let wait = {
                let mut b = self.inner.lock();
                if b.rate == 0 {
                    return;
                }
                let now = Instant::now();
                let cap = burst(b.rate);
                b.tokens = (b.tokens + now.duration_since(b.last).as_secs_f64() * b.rate as f64).min(cap);
                b.last = now;
                let need = (n as f64).min(cap);
                if b.tokens >= need {
                    b.tokens -= n as f64;
                    return;
                }
                Duration::from_secs_f64(((need - b.tokens) / b.rate as f64).max(0.005))
            };
            tokio::time::sleep(wait).await;
        }
    }
}

fn burst(rate: u64) -> f64 {
    (rate as f64 / 4.0).max(16.0 * 1024.0)
}

/// Bytes/s over a rolling window.
pub struct SpeedMeter {
    samples: Mutex<VecDeque<(Instant, u64)>>,
    window: Duration,
}

impl SpeedMeter {
    pub fn new(window_ms: u64) -> Self {
        SpeedMeter { samples: Mutex::new(VecDeque::new()), window: Duration::from_millis(window_ms) }
    }
    pub fn add(&self, n: u64) {
        let now = Instant::now();
        let mut s = self.samples.lock();
        match s.back_mut() {
            Some(last) if now.duration_since(last.0) < Duration::from_millis(100) => last.1 += n,
            _ => s.push_back((now, n)),
        }
        while s.front().is_some_and(|f| now.duration_since(f.0) > self.window) {
            s.pop_front();
        }
    }
    pub fn get(&self) -> u64 {
        let now = Instant::now();
        let mut s = self.samples.lock();
        while s.front().is_some_and(|f| now.duration_since(f.0) > self.window) {
            s.pop_front();
        }
        let Some(first) = s.front() else { return 0 };
        let total: u64 = s.iter().map(|x| x.1).sum();
        let span = now.duration_since(first.0).as_secs_f64().max(1.0);
        (total as f64 / span) as u64
    }
}
