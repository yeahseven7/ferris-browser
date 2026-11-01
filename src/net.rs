//! Network layer: HTTP/HTTPS fetching over rustls, redirect following,
//! decompression, a shared response cache, and browser-style error reporting.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use ureq::{Agent, ResponseExt};
use url::Url;

/// A realistic UA string. Many sites (Wikipedia included) reject the default
/// `ureq/3.x` agent outright, so identifying as a browser matters.
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36 Ferris/1.0";

pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchErrorKind {
    Dns,
    Connection,
    Timeout,
    Tls,
    TooManyRedirects,
    Http(u16),
    Io,
    InvalidUrl,
    TooLarge,
}

#[derive(Debug, Clone)]
pub struct FetchError {
    pub kind: FetchErrorKind,
    pub message: String,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl FetchError {
    /// A short, browser-error-page style headline.
    pub fn headline(&self) -> &'static str {
        match self.kind {
            FetchErrorKind::Dns => "This site can't be reached",
            FetchErrorKind::Connection => "This site can't be reached",
            FetchErrorKind::Timeout => "The request timed out",
            FetchErrorKind::Tls => "Your connection is not private",
            FetchErrorKind::TooManyRedirects => "Too many redirects",
            FetchErrorKind::Http(404) => "This page isn't there",
            FetchErrorKind::Http(_) => "The server reported an error",
            FetchErrorKind::Io => "The connection was interrupted",
            FetchErrorKind::InvalidUrl => "That address isn't valid",
            FetchErrorKind::TooLarge => "The response was too large",
        }
    }

    pub fn detail(&self) -> String {
        match &self.kind {
            FetchErrorKind::Dns => format!(
                "The server's DNS name could not be resolved. ({})",
                self.message
            ),
            FetchErrorKind::Http(c) => {
                format!("The server responded with HTTP {c}. ({})", self.message)
            }
            _ => self.message.clone(),
        }
    }
}

pub struct FetchResult {
    pub final_url: Url,
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub elapsed: Duration,
}

impl FetchResult {
    pub fn is_html(&self) -> bool {
        let ct = self.content_type.to_ascii_lowercase();
        ct.contains("html") || ct.contains("xml") || ct.is_empty()
    }
    pub fn charset(&self) -> Option<String> {
        let ct = self.content_type.to_ascii_lowercase();
        let i = ct.find("charset=")?;
        let rest = &ct[i + 8..];
        let end = rest.find(';').unwrap_or(rest.len());
        let cs = rest[..end].trim().trim_matches('"').to_string();
        if cs.is_empty() { None } else { Some(cs) }
    }
    /// Decode the body to text, honouring a declared charset where possible.
    pub fn text(&self) -> String {
        match String::from_utf8(self.body.clone()) {
            Ok(s) => s,
            Err(e) => {
                // Latin-1 is the historical default and rescues many old pages.
                let bytes = e.as_bytes();
                let declared = self.charset().unwrap_or_default().to_ascii_lowercase();
                if declared.contains("latin") || declared.contains("8859") || declared.is_empty() {
                    bytes.iter().map(|&b| b as char).collect()
                } else {
                    String::from_utf8_lossy(bytes).into_owned()
                }
            }
        }
    }
}

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let a = Agent::new_with_config(
            Agent::config_builder()
                .user_agent(USER_AGENT)
                .max_redirects(8)
                // 4xx/5xx should render as error pages, not Rust `Err`s.
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(25)))
                .build(),
        );
        // Restore any persisted session cookies so logins survive restarts.
        load_cookies_into(&a);
        a
    })
}

// ---------------------------------------------------------------------------
// cookies: a persistent, shared session jar (the basis of all web auth)
// ---------------------------------------------------------------------------

/// Per-user data directory, honouring `$XDG_DATA_HOME` and falling back to
/// `~/.local/share`. Cookies live here so a login survives across runs.
pub fn data_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME")
        && !xdg.trim().is_empty()
    {
        return PathBuf::from(xdg).join("ferris-browser");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("ferris-browser");
    }
    PathBuf::from(".ferris-browser")
}

fn cookie_file() -> PathBuf {
    data_dir().join("cookies.json")
}

fn load_cookies_into(a: &Agent) {
    let path = cookie_file();
    if let Ok(file) = std::fs::File::open(&path) {
        let reader = std::io::BufReader::new(file);
        // A corrupt/partial file must never stop the browser from starting.
        let _ = a.cookie_jar_lock().load_json(reader);
    }
}

/// Write the current cookie jar to disk so sessions persist across restarts.
/// Cheap enough to call after each top-level navigation.
pub fn persist_cookies() {
    let dir = data_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = cookie_file();
    if let Ok(mut file) = std::fs::File::create(&path) {
        let _ = agent().cookie_jar_lock().save_json(&mut file);
    }
}

/// Drop every cookie (and the on-disk copy) — the browser's "clear session".
pub fn clear_cookies() {
    agent().cookie_jar_lock().clear();
    let _ = std::fs::remove_file(cookie_file());
}

/// How many cookies are currently stored. Useful for tests and `/info`.
pub fn cookie_count() -> usize {
    agent().cookie_jar_lock().iter().count()
}

/// All unexpired cookies as a `name=value; …` string, for `document.cookie`.
pub fn cookies_as_header() -> String {
    let jar = agent().cookie_jar_lock();
    let mut out = String::new();
    for c in jar.iter() {
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(c.name());
        out.push('=');
        out.push_str(c.value());
    }
    out
}

/// Store a cookie set from script (`document.cookie = "k=v; path=/; …"`),
/// scoped to `page_url` following the RFC 6265 storage model.
pub fn set_cookie_from_script(page_url: &Url, cookie_str: &str) -> bool {
    let uri = match page_url.as_str().parse::<ureq::http::Uri>() {
        Ok(u) => u,
        Err(_) => return false,
    };
    let cookie = match ureq::Cookie::parse(cookie_str.to_string(), &uri) {
        Ok(c) => c,
        Err(_) => return false,
    };
    agent()
        .cookie_jar_lock()
        .insert(cookie, &uri)
        .is_ok()
}

/// Turn user input into an absolute URL, adding `https://` when the scheme is
/// missing and treating bare words as a search query would be overkill for.
pub fn normalize(input: &str) -> Result<Url, FetchError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(err(FetchErrorKind::InvalidUrl, "the address is empty"));
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    Url::parse(&candidate)
        .map_err(|e| err(FetchErrorKind::InvalidUrl, e.to_string()))
        .and_then(|u| {
            // Reject things that are clearly not navigable.
            if u.scheme() != "http" && u.scheme() != "https" {
                return Err(err(
                    FetchErrorKind::InvalidUrl,
                    format!("the {:?} scheme is not supported", u.scheme()),
                ));
            }
            if u.host_str().is_none() {
                return Err(err(FetchErrorKind::InvalidUrl, "the address has no host"));
            }
            Ok(u)
        })
}

fn err(kind: FetchErrorKind, message: impl Into<String>) -> FetchError {
    FetchError {
        kind,
        message: message.into(),
    }
}

fn classify(e: &ureq::Error) -> FetchErrorKind {
    let s = e.to_string().to_ascii_lowercase();
    match e {
        ureq::Error::StatusCode(code) => FetchErrorKind::Http(*code),
        ureq::Error::HostNotFound => FetchErrorKind::Dns,
        ureq::Error::Timeout(_) => FetchErrorKind::Timeout,
        ureq::Error::BadUri(_) => FetchErrorKind::InvalidUrl,
        ureq::Error::Io(_) => {
            if s.contains("dns") || s.contains("resolve") || s.contains("name or service") {
                FetchErrorKind::Dns
            } else if s.contains("timed out") || s.contains("timeout") {
                FetchErrorKind::Timeout
            } else if s.contains("refused") || s.contains("unreachable") || s.contains("connect") {
                FetchErrorKind::Connection
            } else {
                FetchErrorKind::Io
            }
        }
        // Http / Protocol / RedirectFailed / Tls and anything added later.
        _ => {
            if s.contains("tls")
                || s.contains("certificate")
                || s.contains("ssl")
                || s.contains("handshake")
            {
                FetchErrorKind::Tls
            } else if s.contains("redirect") {
                FetchErrorKind::TooManyRedirects
            } else if s.contains("dns") || s.contains("resolve") {
                FetchErrorKind::Dns
            } else {
                FetchErrorKind::Io
            }
        }
    }
}

/// Perform a GET and read the whole body, capped at `max_bytes`.
pub fn fetch(url: &Url, max_bytes: usize) -> Result<FetchResult, FetchError> {
    let started = Instant::now();
    let res = agent()
        .get(url.as_str())
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Accept-Encoding", "gzip, deflate, br")
        .header("Upgrade-Insecure-Requests", "1")
        .header("Connection", "close")
        .call()
        .map_err(|e| err(classify(&e), e.to_string()))?;
    read_response(res, url, started, max_bytes)
}

/// POST a raw body (e.g. a form encoding) and read the response. Cookies are
/// attached automatically and any redirect chain is followed.
pub fn fetch_post(
    url: &Url,
    body: Vec<u8>,
    content_type: &str,
    max_bytes: usize,
) -> Result<FetchResult, FetchError> {
    let started = Instant::now();
    let res = agent()
        .post(url.as_str())
        .header("Content-Type", content_type)
        .header(
            "Accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Accept-Encoding", "gzip, deflate, br")
        .send(body)
        .map_err(|e| err(classify(&e), e.to_string()))?;
    read_response(res, url, started, max_bytes)
}

/// Submit an HTML form as `application/x-www-form-urlencoded`.
pub fn submit_form(
    url: &Url,
    fields: &[(String, String)],
    max_bytes: usize,
) -> Result<FetchResult, FetchError> {
    let body = encode_form(fields);
    fetch_post(
        url,
        body.into_bytes(),
        "application/x-www-form-urlencoded",
        max_bytes,
    )
}

/// Percent-encode form fields into `application/x-www-form-urlencoded`.
/// Spaces become `+`, per the HTML form spec (not RFC 3986 `%20`).
pub fn encode_form(fields: &[(String, String)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&urlenc_form(k));
        out.push('=');
        out.push_str(&urlenc_form(v));
    }
    out
}

fn urlenc_form(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Shared tail of every request: pull status, content-type, final URL and a
/// size-capped body out of a ureq `Response`.
fn read_response(
    res: ureq::http::Response<ureq::Body>,
    fallback: &Url,
    started: Instant,
    max_bytes: usize,
) -> Result<FetchResult, FetchError> {
    let status = res.status().as_u16();
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // Where we ended up after redirects, which relative URLs must resolve against.
    let final_url = res
        .get_uri()
        .to_string()
        .parse::<Url>()
        .unwrap_or_else(|_| fallback.clone());

    let mut body = res
        .into_body()
        .read_to_vec()
        .map_err(|e| err(classify(&e), e.to_string()))?;

    if body.len() > max_bytes {
        body.truncate(max_bytes);
    }

    Ok(FetchResult {
        final_url,
        status,
        content_type,
        body,
        elapsed: started.elapsed(),
    })
}

// ---------------------------------------------------------------------------
// cache
// ---------------------------------------------------------------------------

struct Entry {
    at: Instant,
    hits: u32,
    data: std::sync::Arc<FetchResult>,
}

const MAX_ENTRIES: usize = 128;

struct Cache(Mutex<HashMap<String, Entry>>);

impl Cache {
    fn get(&self, key: &str, max_age: Duration) -> Option<std::sync::Arc<FetchResult>> {
        let mut m = self.0.lock().ok()?;
        if let Some(e) = m.get_mut(key) {
            if e.at.elapsed() <= max_age {
                e.hits += 1;
                return Some(e.data.clone());
            }
            m.remove(key);
        }
        None
    }

    fn put(&self, key: &str, data: FetchResult) -> std::sync::Arc<FetchResult> {
        let arc = std::sync::Arc::new(data);
        if let Ok(mut m) = self.0.lock() {
            if m.len() >= MAX_ENTRIES {
                // Evict the least-recently-inserted entry that is not hot.
                if let Some(victim) = m
                    .iter()
                    .min_by_key(|(_, e)| (e.hits, std::cmp::Reverse(e.at)))
                    .map(|(k, _)| k.clone())
                {
                    m.remove(&victim);
                }
            }
            m.insert(
                key.to_string(),
                Entry {
                    at: Instant::now(),
                    hits: 0,
                    data: arc.clone(),
                },
            );
        }
        arc
    }

    fn clear(&self) {
        if let Ok(mut m) = self.0.lock() {
            m.clear();
        }
    }
}

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Cache(Mutex::new(HashMap::new())))
}

/// Fetch through the shared cache. `max_age` of zero forces a revalidation.
pub fn fetch_cached(
    url: &Url,
    max_bytes: usize,
    max_age: Duration,
) -> Result<std::sync::Arc<FetchResult>, FetchError> {
    let key = format!("{url}|{max_bytes}");
    if let Some(hit) = cache().get(&key, max_age) {
        return Ok(hit);
    }
    let res = fetch(url, max_bytes)?;
    Ok(cache().put(&key, res))
}

pub fn invalidate() {
    cache().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_adds_scheme() {
        assert_eq!(
            normalize("example.com").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(normalize("http://example.com").unwrap().scheme(), "http");
        assert_eq!(
            normalize("example.com/a/b?c=d#e").unwrap().as_str(),
            "https://example.com/a/b?c=d#e"
        );
    }

    #[test]
    fn normalize_rejects_non_http() {
        assert!(normalize("ftp://example.com").is_err());
        assert!(normalize("file:///etc/passwd").is_err());
        assert!(normalize("").is_err());
        assert!(normalize("javascript:alert(1)").is_err());
    }

    #[test]
    fn relative_resolution() {
        let base = normalize("https://news.ycombinator.com/news").unwrap();
        assert_eq!(
            base.join("newest").unwrap().as_str(),
            "https://news.ycombinator.com/newest"
        );
        assert_eq!(
            base.join("/x").unwrap().as_str(),
            "https://news.ycombinator.com/x"
        );
        assert_eq!(
            base.join("//cdn.example.com/a.png").unwrap().as_str(),
            "https://cdn.example.com/a.png"
        );
    }

    #[test]
    fn cache_roundtrip() {
        let u = normalize("https://example.invalid/cached").unwrap();
        let first = FetchResult {
            final_url: u.clone(),
            status: 200,
            content_type: "text/html".into(),
            body: b"hello".to_vec(),
            elapsed: Duration::ZERO,
        };
        let key = format!("{u}|{}", MAX_BODY_BYTES);
        cache().put(&key, first);
        let hit = cache().get(&key, Duration::from_secs(60));
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().body, b"hello".to_vec());
    }

    #[test]
    fn error_headlines_are_user_facing() {
        let e = err(FetchErrorKind::Dns, "no such host".to_string());
        assert_eq!(e.headline(), "This site can't be reached");
        assert!(e.detail().contains("DNS"));
    }

    #[test]
    fn form_encoding_is_spec_shaped() {
        let fields = vec![
            ("user".to_string(), "ada lovelace".to_string()),
            ("pass".to_string(), "p@ss&w=rd!".to_string()),
            ("empty".to_string(), String::new()),
        ];
        let enc = encode_form(&fields);
        // Spaces -> '+', reserved chars percent-encoded, pairs joined by '&'.
        assert_eq!(
            enc,
            "user=ada+lovelace&pass=p%40ss%26w%3Drd%21&empty="
        );
    }

    #[test]
    fn cookie_roundtrip_in_memory() {
        // Script-set cookie is stored and readable via the header form.
        let url = normalize("https://example.org/acct").unwrap();
        assert!(set_cookie_from_script(&url, "sid=abc123; path=/"));
        let header = cookies_as_header();
        assert!(
            header.contains("sid=abc123"),
            "expected sid in {header:?}"
        );
        assert!(cookie_count() >= 1);
        // A bogus cookie string must be rejected, not panic.
        assert!(!set_cookie_from_script(&url, ";;;not a cookie;;"));
    }

    #[test]
    fn data_dir_is_absolute_under_home_or_cwd() {
        let d = data_dir();
        assert!(d.ends_with("ferris-browser"), "{d:?}");
    }
}
