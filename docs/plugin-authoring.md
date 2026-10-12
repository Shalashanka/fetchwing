# Writing Fetchwing plugins

## 1. JSON site plugins (no compiler needed)

Drop a `.json` file into the plugin folder, then press **Plugins → Reload**:
- Windows: `%LOCALAPPDATA%\Fetchwing\plugins`
- Linux: `~/.local/share/fetchwing/plugins`

A file can hold one object or an array of them. A user plugin with the same `id` as a builtin
replaces it.

```json
[
  { "type": "xfs",       "id": "myhost",    "domains": ["myhost.com", "myhost.net"], "maxChunks": -2 },
  { "type": "yetishare", "id": "otherhost", "domains": ["otherhost.io"] },
  { "type": "ytdlp",     "id": "videosite", "domains": ["videosite.tv"] },
  {
    "type": "regex", "id": "simplehost", "name": "Simple Host",
    "domains": ["files.example.org"],
    "pattern": "^https?://files\\.example\\.org/f/\\w+",
    "linkRegex": "href=\"(https://cdn\\.example\\.org/[^\"]+)\"",
    "nameRegex": "<h1[^>]*>([^<]+)</h1>",
    "sizeRegex": "Size: ([\\d.]+ [KMG]B)",
    "offlineRegex": "File not found|deleted",
    "waitRegex": "var wait = (\\d+);"
  }
]
```

| type | What it does |
|---|---|
| `xfs` | Full XFileSharingPro flow: download1 → countdown → captcha (Turnstile / hCaptcha / reCAPTCHA / image / plaintext) → password → download2 → direct link. Error texts map to waits, premium-only and offline. `maxChunks`: JD2 semantics (0 = unlimited, −n = n connections). |
| `yetishare` | YetiShare flow (countdown, reCAPTCHA form, `pt=` / `download_token` links). |
| `ytdlp` | Route the domain to yt-dlp. |
| `regex` | Fetch the page and read the link, name, size, offline flag and wait time by regex. Covers most simple one-click hosters. Rust regex syntax: no look-around or backreferences. |

## 2. Rust plugins (logic-heavy sites)

Implement `plugins::Plugin` (see `src/plugins/mod.rs`, and `pixeldrain.rs` as the smallest
example):

```rust
#[async_trait]
impl Plugin for MyHost {
    fn meta(&self) -> &Meta { &self.meta }                     // id, domains, URL regexes, limits
    async fn check(&self, ctx: &mut Ctx) -> PResult<Option<LinkInfo>> { … }  // cheap availability
    async fn resolve(&self, ctx: &mut Ctx) -> PResult<Resolution> { … }      // -> direct request / yt-dlp
}
```

Everything comes through `Ctx`:
- `ctx.page(url)` and `ctx.post(...)`: cookie-keeping browser, forms via `page.forms()`.
- `ctx.wait(ms, reason)`: visible countdown.
- `ctx.captcha(req)`: queued, never modal.
- `ctx.prop()` / `set_prop()`: persisted per-link state.
- `ctx.setting(key)`.
- `ctx.account`.

Errors are typed and drive the scheduler:

| Error | Effect |
|---|---|
| `offline(..)` | Permanent failure |
| `hoster_wait(ms, ..)` | Wait; blocks the host for free downloads |
| `premium_only(..)` | Tells the user to get an account |
| `defect(..)` | The site changed |
| `ErrCode::Retry` | Back-off retry |

Register the plugin in `Registry::new`.

Template data for XFS / YetiShare / KVS lives in `assets/template-sites.json`. It is generated
from the decompiled JD2 plugins by `tools/extract_templates.py`.
