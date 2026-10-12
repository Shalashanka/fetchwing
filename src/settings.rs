//! Schema-driven settings. JD2 exposes ~1,000 raw config keys in "Advanced Settings". Here every
//! setting declares label, help, group and bounds, so UIs render, search and validate them
//! generically.

use parking_lot::RwLock;
use serde::Serialize;
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Number,
    Bool,
    Text,
    Secret,
    Enum,
    Path,
    List,
}

#[derive(Clone, Debug, Serialize)]
pub struct SettingDef {
    pub key: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub advanced: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub unit: &'static str,
    pub options: &'static [(&'static str, &'static str)],
    #[serde(skip)]
    pub default: fn() -> Value,
}

macro_rules! def {
    ($key:expr, $group:expr, $label:expr, $kind:expr, $default:expr $(, $field:ident = $val:expr)* ) => {
        SettingDef { $($field: $val,)* ..SettingDef { key: $key, group: $group, label: $label, help: "", kind: $kind, advanced: false, min: None, max: None, unit: "",
            options: &[], default: || json!($default) } }
    };
}

pub fn default_download_dir() -> String {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Downloads"))
        .join("Fetchwing")
        .to_string_lossy()
        .into_owned()
}

pub fn schema() -> &'static [SettingDef] {
    static S: std::sync::OnceLock<Vec<SettingDef>> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        use Kind::*;
        vec![
            SettingDef { key: "general.downloadDir", group: "General", label: "Download folder", help: "Packages get their own sub-folder here unless a rule says otherwise.", kind: Path, advanced: false, min: None, max: None, unit: "", options: &[], default: || json!(default_download_dir()) },
            def!("general.packageSubfolders", "General", "Create a sub-folder per package", Bool, true),
            def!("general.fileExists", "General", "If the file already exists", Enum, "rename", options = &[("rename", "Keep both (add a number)"), ("overwrite", "Overwrite"), ("skip", "Skip the download")]),
            def!("general.autoStart", "General", "Start downloads automatically", Bool, true, help = "Links that pass the online check go straight to the queue. Turn off to review everything in the Inbox first (JD2 behaviour)."),
            def!("general.autoStartDelay", "General", "Inbox grace period", Number, 3, min = Some(0.0), max = Some(600.0), unit = "s", help = "How long new links stay in the Inbox before auto-start, so you can still edit them."),
            def!("general.resumeOnStartup", "General", "Resume unfinished downloads on startup", Bool, true),
            def!("general.minimizeToTray", "General", "Keep running when the window is closed", Bool, false, help = "Downloads continue in the background; reopen Fetchwing to show the window."),

            def!("downloads.maxActive", "Downloads", "Simultaneous downloads", Number, 4, min = Some(1.0), max = Some(50.0)),
            def!("downloads.maxPerHost", "Downloads", "Simultaneous downloads per host", Number, 2, min = Some(1.0), max = Some(20.0)),
            def!("downloads.connections", "Downloads", "Connections per download", Number, 8, min = Some(1.0), max = Some(32.0), help = "Upper bound. Hoster plugins may lower it (free users often get 1)."),
            def!("downloads.minSegment", "Downloads", "Minimum segment size", Number, 1048576, min = Some(65536.0), unit = "bytes", advanced = true, help = "Segments are split dynamically (work stealing) but never below this size."),
            def!("downloads.speedLimit", "Downloads", "Global speed limit", Number, 0, min = Some(0.0), unit = "bytes/s", help = "0 = unlimited."),
            def!("downloads.retries", "Downloads", "Automatic retries", Number, 5, min = Some(0.0), max = Some(100.0)),
            def!("downloads.retryBackoff", "Downloads", "First retry delay", Number, 5, min = Some(1.0), max = Some(3600.0), unit = "s", advanced = true, help = "Doubles after every failed attempt (capped at 10 minutes)."),
            def!("downloads.verifyHash", "Downloads", "Verify checksums when available", Bool, true),
            def!("downloads.preallocate", "Downloads", "Pre-allocate disk space", Bool, true, advanced = true),
            def!("downloads.diskMinFree", "Downloads", "Pause when free disk space drops below", Number, 2147483648u64, unit = "bytes", advanced = true),

            def!("connection.userAgent", "Connection", "User-Agent", Text, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36", advanced = true),
            def!("connection.timeout", "Connection", "Connect/read timeout", Number, 30, min = Some(5.0), max = Some(600.0), unit = "s"),
            def!("connection.proxy", "Connection", "Proxy", Text, "", help = "http://user:pass@host:port or socks5://host:port. Empty = direct."),

            def!("links.deepCrawl", "Link handling", "Scan unknown web pages for downloadable links", Bool, true),
            def!("links.clipboard", "Link handling", "Watch the clipboard for links", Bool, false, help = "Off by default: clipboard snooping surprises people. New links go to the Inbox."),
            def!("links.cnl", "Link handling", "Click'n'Load (port 9666)", Bool, true, help = "Lets link-protection sites hand links over. Disabled automatically if the port is busy (e.g. JDownloader running)."),
            def!("links.ytdlp", "Link handling", "Use yt-dlp for video/audio sites", Bool, true, help = "Adds 1,800+ media sites."),
            def!("links.ytdlpManaged", "Link handling", "Keep yt-dlp up to date automatically", Bool, true, help = "Media sites change weekly; a stale yt-dlp is the #1 cause of broken video downloads."),
            def!("links.ytdlpPath", "Link handling", "yt-dlp executable", Path, "yt-dlp", advanced = true, help = "Leave as \"yt-dlp\" to use the managed copy (falls back to PATH)."),
            def!("links.ytdlpFormat", "Link handling", "Preferred media format", Text, "bv*+ba/b", advanced = true, help = "yt-dlp format selector."),
            def!("links.ytdlpArgs", "Link handling", "Extra yt-dlp arguments", Text, "", help = "Added to every yt-dlp call, e.g. --extractor-args \"generic:impersonate\" or --cookies-from-browser firefox. Pages behind a Cloudflare challenge are retried with impersonation automatically."),
            def!("links.ignoreExtensions", "Link handling", "Ignore files with these extensions", List, Vec::<String>::new()),

            def!("captcha.mode", "Captcha", "Captcha handling", Enum, "auto", options = &[("auto", "Services first, then ask me"), ("manual", "Always ask me"), ("off", "Skip links that need captchas")]),
            def!("captcha.timeout", "Captcha", "Manual captcha timeout", Number, 300, min = Some(30.0), max = Some(3600.0), unit = "s"),
            def!("captcha.serviceUrl", "Captcha", "Solver service", Enum, "", options = &[("", "None"), ("https://api.2captcha.com", "2Captcha"), ("https://api.anti-captcha.com", "Anti-Captcha"), ("https://api.capsolver.com", "CapSolver"), ("https://api.capmonster.cloud", "CapMonster Cloud"), ("custom", "Custom (createTask compatible)")]),
            def!("captcha.serviceCustomUrl", "Captcha", "Custom solver URL", Text, "", advanced = true, help = "e.g. a local DeCaptcha instance."),
            def!("captcha.serviceKey", "Captcha", "Solver API key", Secret, ""),
            def!("captcha.maxCostPerDay", "Captcha", "Daily solver budget (US cents)", Number, 100, min = Some(0.0)),

            def!("archives.autoExtract", "Archives", "Extract archives automatically", Bool, true),
            SettingDef { key: "archives.sevenZip", group: "Archives", label: "7-Zip executable", help: "Fetchwing uses the 7-Zip command line tool.", kind: Path, advanced: true, min: None, max: None, unit: "", options: &[], default: || json!(if cfg!(windows) { "C:\\Program Files\\7-Zip\\7z.exe" } else { "7z" }) },
            def!("archives.deleteAfter", "Archives", "After successful extraction", Enum, "recycle", options = &[("keep", "Keep archives"), ("recycle", "Move archives to .fetchwing-trash"), ("delete", "Delete archives")]),
            def!("archives.passwords", "Archives", "Password list", List, Vec::<String>::new(), help = "Tried after passwords found next to the links."),
            def!("archives.lowPriority", "Archives", "Extract with low CPU priority", Bool, true, help = "Keeps games and video smooth while extracting."),

            def!("ui.theme", "Interface", "Theme", Enum, "dark", options = &[("dark", "Dark"), ("light", "Light")]),
            def!("ui.compact", "Interface", "Compact rows", Bool, false),

            def!("remote.bind", "Remote access", "Listen address", Text, "127.0.0.1", help = "0.0.0.0 allows other devices on your network (they need the access token). Restart required."),
            def!("remote.port", "Remote access", "Port", Number, 7979, min = Some(1024.0), max = Some(65535.0), help = "Web UI + API port. Restart required."),
            def!("remote.token", "Remote access", "Access token", Secret, "", help = "Generated on first start. Required for every API call."),

            def!("schedule.rules", "Schedule", "Speed schedule", List, Vec::<String>::new(), help = "One rule per line: days time-range limit, e.g. \"Mon-Fri 08:00-18:00 2MB\", \"* 01:00-07:00 0\" (unlimited), \"Sat 10:00-12:00 pause\"."),
        ]
    })
}

pub fn def(key: &str) -> Option<&'static SettingDef> {
    schema().iter().find(|d| d.key == key)
}

pub struct Settings {
    values: RwLock<Map<String, Value>>,
}

impl Settings {
    pub fn new(initial: Map<String, Value>) -> Self {
        let mut m = Map::new();
        for (k, v) in initial {
            if def(&k).is_some() {
                m.insert(k, v);
            }
        }
        Settings { values: RwLock::new(m) }
    }

    pub fn get(&self, key: &str) -> Value {
        if let Some(v) = self.values.read().get(key) {
            return v.clone();
        }
        def(key).map(|d| (d.default)()).unwrap_or(Value::Null)
    }
    pub fn bool(&self, key: &str) -> bool {
        self.get(key).as_bool().unwrap_or(false)
    }
    pub fn num(&self, key: &str) -> f64 {
        self.get(key).as_f64().unwrap_or(0.0)
    }
    pub fn u64(&self, key: &str) -> u64 {
        self.num(key).max(0.0) as u64
    }
    pub fn str(&self, key: &str) -> String {
        match self.get(key) {
            Value::String(s) => s,
            Value::Null => String::new(),
            v => v.to_string(),
        }
    }
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Value::Array(a) => a.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => vec![],
        }
    }

    /// Validate + coerce + store. Returns the stored value.
    pub fn set(&self, key: &str, v: Value) -> Result<Value, String> {
        let d = def(key).ok_or_else(|| format!("Unknown setting {key}"))?;
        let val = coerce(d, v)?;
        self.values.write().insert(key.to_string(), val.clone());
        Ok(val)
    }

    pub fn overrides(&self) -> Map<String, Value> {
        self.values.read().clone()
    }
    pub fn all(&self) -> Map<String, Value> {
        schema().iter().map(|d| (d.key.to_string(), self.get(d.key))).collect()
    }
}

fn coerce(d: &SettingDef, v: Value) -> Result<Value, String> {
    Ok(match d.kind {
        Kind::Number => {
            let n = match &v {
                Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                Value::String(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
                _ => f64::NAN,
            };
            if !n.is_finite() {
                return Err(format!("{}: not a number", d.label));
            }
            if let Some(min) = d.min {
                if n < min {
                    return Err(format!("{}: minimum is {min}", d.label));
                }
            }
            if let Some(max) = d.max {
                if n > max {
                    return Err(format!("{}: maximum is {max}", d.label));
                }
            }
            json!(n)
        }
        Kind::Bool => json!(matches!(v, Value::Bool(true)) || v.as_str() == Some("true") || v.as_i64() == Some(1)),
        Kind::Enum => {
            let s = v.as_str().unwrap_or_default().to_string();
            if !d.options.iter().any(|(k, _)| *k == s) {
                return Err(format!("{}: invalid option", d.label));
            }
            json!(s)
        }
        Kind::List => match v {
            Value::Array(a) => json!(a.into_iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect::<Vec<_>>()),
            Value::String(s) => json!(s.lines().map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>()),
            _ => json!(Vec::<String>::new()),
        },
        _ => json!(match v {
            Value::String(s) => s,
            Value::Null => String::new(),
            o => o.to_string(),
        }),
    })
}
