//! Captcha broker.
//!
//! JD2: ChallengeResponseController asks every solver (JAntiCaptcha OCR, 9kw/2captcha plugins,
//! browser solver, dialog solver); the dialog solver opens a modal that steals focus and
//! interrupts full-screen games/videos. Fetchwing: a queue. Jobs appear in the Captcha view, nobody
//! gets interrupted.
//!   1. service solver (any createTask-compatible API: 2Captcha, Anti-Captcha, CapSolver,
//!      CapMonster, local DeCaptcha) within a daily budget
//!   2. the user (GUI / web UI / browser extension for token widgets)

use crate::events::{Event, Notice};
use crate::model::{new_id, now_ms};
use crate::plugins::{ErrCode, PResult, PluginError};
use crate::settings::Settings;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CaptchaRequest {
    /// `image` is a data: URL
    Image { image: String, hint: Option<String> },
    Recaptcha2 { site_key: String, page_url: String, invisible: bool },
    Recaptcha3 { site_key: String, page_url: String, action: Option<String> },
    Hcaptcha { site_key: String, page_url: String },
    Turnstile { site_key: String, page_url: String },
}

impl CaptchaRequest {
    pub fn describe(&self) -> &'static str {
        match self {
            CaptchaRequest::Image { .. } => "Type the characters shown in the image",
            CaptchaRequest::Recaptcha2 { .. } | CaptchaRequest::Recaptcha3 { .. } => "reCAPTCHA - solve it on the site (extension) or configure a solver service",
            CaptchaRequest::Hcaptcha { .. } => "hCaptcha - solve it on the site (extension) or configure a solver service",
            CaptchaRequest::Turnstile { .. } => "Cloudflare Turnstile - solve it on the site (extension) or configure a solver service",
        }
    }
    pub fn image_bytes(&self) -> Option<Vec<u8>> {
        use base64::Engine;
        match self {
            CaptchaRequest::Image { image, .. } => base64::engine::general_purpose::STANDARD.decode(image.split(',').nth(1)?).ok(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptchaJob {
    pub id: String,
    pub link_id: Option<String>,
    pub host: String,
    pub request: CaptchaRequest,
    pub created_at: i64,
    pub expires_at: i64,
}

pub struct Broker {
    jobs: Mutex<HashMap<String, (CaptchaJob, oneshot::Sender<Result<String, String>>)>>,
    settings: Arc<Settings>,
    events: broadcast::Sender<Event>,
    /// (day, cents spent)
    pub spend: Mutex<(String, u64)>,
}

impl Broker {
    pub fn new(settings: Arc<Settings>, events: broadcast::Sender<Event>, spend: (String, u64)) -> Self {
        Broker { jobs: Mutex::new(HashMap::new()), settings, events, spend: Mutex::new(spend) }
    }

    pub fn list(&self) -> Vec<CaptchaJob> {
        let mut v: Vec<_> = self.jobs.lock().values().map(|(j, _)| j.clone()).collect();
        v.sort_by_key(|j| j.created_at);
        v
    }

    pub fn submit(&self, id: &str, value: &str) -> bool {
        let Some((_, tx)) = self.jobs.lock().remove(id) else { return false };
        let _ = tx.send(Ok(value.to_string()));
        let _ = self.events.send(Event::CaptchaDone { id: id.into(), ok: true });
        true
    }
    pub fn skip(&self, id: &str) -> bool {
        let Some((_, tx)) = self.jobs.lock().remove(id) else { return false };
        let _ = tx.send(Err("skipped".into()));
        let _ = self.events.send(Event::CaptchaDone { id: id.into(), ok: false });
        true
    }
    pub fn cancel_for_link(&self, link_id: &str) {
        let ids: Vec<String> = self.jobs.lock().values().filter(|(j, _)| j.link_id.as_deref() == Some(link_id)).map(|(j, _)| j.id.clone()).collect();
        for id in ids {
            self.skip(&id);
        }
    }

    pub async fn solve(&self, req: CaptchaRequest, link_id: Option<String>, host: &str, cancel: &CancellationToken) -> PResult<String> {
        let mode = self.settings.str("captcha.mode");
        if mode == "off" {
            return Err(PluginError::new(ErrCode::CaptchaFailed, "Captcha solving is disabled").hint("Enable captcha handling in Settings → Captcha."));
        }
        if mode == "auto" && self.service_configured() {
            match self.solve_service(&req, cancel).await {
                Ok(Some(v)) => return Ok(v),
                Ok(None) => {}
                Err(e) => crate::log::warn("captcha", &format!("service failed for {host}: {e} - asking user")),
            }
        }
        self.ask_user(req, link_id, host, cancel).await
    }

    async fn ask_user(&self, req: CaptchaRequest, link_id: Option<String>, host: &str, cancel: &CancellationToken) -> PResult<String> {
        let timeout = self.settings.u64("captcha.timeout").max(30);
        let (tx, rx) = oneshot::channel();
        let job = CaptchaJob { id: new_id(), link_id: link_id.clone(), host: host.into(), request: req.clone(), created_at: now_ms(), expires_at: now_ms() + timeout as i64 * 1000 };
        let id = job.id.clone();
        self.jobs.lock().insert(id.clone(), (job.clone(), tx));
        let _ = self.events.send(Event::CaptchaNew(job));
        let _ = self.events.send(Event::Notify(Notice::new("captcha", &format!("Captcha needed for {host}"), req.describe(), link_id)));
        let res = tokio::select! {
            r = rx => r.unwrap_or(Err("dropped".into())),
            _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => Err("timeout".into()),
            _ = cancel.cancelled() => Err("cancelled".into()),
        };
        if self.jobs.lock().remove(&id).is_some() {
            let _ = self.events.send(Event::CaptchaDone { id: id.clone(), ok: false });
        }
        match res {
            Ok(v) => Ok(v),
            Err(e) if e == "timeout" => Err(PluginError::new(ErrCode::CaptchaSkipped, "Captcha not solved in time")),
            Err(e) if e == "cancelled" => Err(PluginError::new(ErrCode::Cancelled, "cancelled")),
            Err(_) => Err(PluginError::new(ErrCode::CaptchaSkipped, "Captcha skipped")),
        }
    }

    pub fn service_configured(&self) -> bool {
        !self.service_base().is_empty() && !self.settings.str("captcha.serviceKey").is_empty()
    }
    fn service_base(&self) -> String {
        let s = self.settings.str("captcha.serviceUrl");
        if s == "custom" { self.settings.str("captcha.serviceCustomUrl") } else { s }
    }
    fn spent_today(&self) -> u64 {
        let day = chrono::Local::now().format("%Y-%m-%d").to_string();
        let s = self.spend.lock();
        if s.0 == day { s.1 } else { 0 }
    }

    /// createTask / getTaskResult protocol (shared by 2Captcha v2, Anti-Captcha, CapSolver, CapMonster).
    async fn solve_service(&self, req: &CaptchaRequest, cancel: &CancellationToken) -> Result<Option<String>, String> {
        if self.spent_today() >= self.settings.u64("captcha.maxCostPerDay") {
            crate::log::warn("captcha", "daily solver budget exhausted");
            return Ok(None);
        }
        let base = self.service_base().trim_end_matches('/').to_string();
        let key = self.settings.str("captcha.serviceKey");
        let cs = base.contains("capsolver");
        let task = match req {
            CaptchaRequest::Image { image, .. } => json!({"type": "ImageToTextTask", "body": image.split(',').nth(1).unwrap_or(image)}),
            CaptchaRequest::Recaptcha2 { site_key, page_url, invisible } => json!({"type": if cs {"ReCaptchaV2TaskProxyLess"} else {"RecaptchaV2TaskProxyless"}, "websiteURL": page_url, "websiteKey": site_key, "isInvisible": invisible}),
            CaptchaRequest::Recaptcha3 { site_key, page_url, action } => json!({"type": if cs {"ReCaptchaV3TaskProxyLess"} else {"RecaptchaV3TaskProxyless"}, "websiteURL": page_url, "websiteKey": site_key, "pageAction": action.clone().unwrap_or("verify".into()), "minScore": 0.3}),
            CaptchaRequest::Hcaptcha { site_key, page_url } => json!({"type": if cs {"HCaptchaTaskProxyLess"} else {"HCaptchaTaskProxyless"}, "websiteURL": page_url, "websiteKey": site_key}),
            CaptchaRequest::Turnstile { site_key, page_url } => json!({"type": if cs {"AntiTurnstileTaskProxyLess"} else {"TurnstileTaskProxyless"}, "websiteURL": page_url, "websiteKey": site_key}),
        };
        let client = reqwest::Client::new();
        let post = |url: String, body: Value| {
            let c = client.clone();
            async move {
                let r = c.post(url).json(&body).send().await.map_err(|e| e.to_string())?;
                r.json::<Value>().await.map_err(|e| e.to_string())
            }
        };
        let created = post(format!("{base}/createTask"), json!({"clientKey": key, "task": task})).await?;
        if created["errorId"].as_i64().unwrap_or(0) != 0 {
            return Err(format!("{}: {}", created["errorCode"].as_str().unwrap_or("error"), created["errorDescription"].as_str().unwrap_or("")));
        }
        let task_id = created["taskId"].clone();
        let first = if matches!(req, CaptchaRequest::Image { .. }) { 3 } else { 10 };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut delay = first;
        while std::time::Instant::now() < deadline {
            tokio::select! {
                _ = cancel.cancelled() => return Err("cancelled".into()),
                _ = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
            }
            delay = 3;
            let r = post(format!("{base}/getTaskResult"), json!({"clientKey": key, "taskId": task_id})).await?;
            if r["errorId"].as_i64().unwrap_or(0) != 0 {
                return Err(format!("{}: {}", r["errorCode"].as_str().unwrap_or("error"), r["errorDescription"].as_str().unwrap_or("")));
            }
            if r["status"] == "ready" {
                let s = &r["solution"];
                let v = ["gRecaptchaResponse", "token", "text"].iter().find_map(|k| s[*k].as_str()).unwrap_or_default().to_string();
                if v.is_empty() {
                    return Err("empty solution".into());
                }
                let cents = (r["cost"].as_str().and_then(|c| c.parse::<f64>().ok()).or(r["cost"].as_f64()).unwrap_or(0.0) * 100.0).round() as u64;
                let day = chrono::Local::now().format("%Y-%m-%d").to_string();
                let mut sp = self.spend.lock();
                if sp.0 != day {
                    *sp = (day, 0);
                }
                sp.1 += cents;
                return Ok(Some(v));
            }
        }
        Err("service timeout".into())
    }
}
