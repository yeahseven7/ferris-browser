//! Engine: wires fetch -> parse -> style -> layout -> paint together, and
//! renders browser-style error pages *through the same pipeline*.

use std::time::{Duration, Instant};

use scraper::Html;
use url::Url;

use crate::css::Stylesheet;
use crate::layout::{Layout, LayoutOutput, LinkRect, Stats};
use crate::net::{self, FetchErrorKind};
use crate::paint;
use crate::style;

const MAX_STYLESHEETS: usize = 16;
const MAX_CSS_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    pub fetch: Duration,
    pub parse: Duration,
    pub css: Duration,
    pub layout: Duration,
    pub paint: Duration,
    pub total: Duration,
}

#[derive(Clone, Debug)]
pub struct ErrorInfo {
    pub headline: String,
    pub detail: String,
    pub code: String,
}

/// What the JavaScript stage (feature `js`) did to a page.
#[derive(Debug, Clone, Default)]
pub struct JsReport {
    pub enabled: bool,
    pub scripts: usize,
    pub console: Vec<String>,
    pub errors: Vec<String>,
    pub ms: u64,
}

pub struct PageRender {
    pub requested: String,
    pub final_url: String,
    pub status: u16,
    pub content_type: String,
    pub title: String,
    pub favicon: Option<String>,
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub links: Vec<LinkRect>,
    pub stats: Stats,
    pub timings: Timings,
    pub clipped: bool,
    pub sheets: usize,
    pub error: Option<ErrorInfo>,
    /// Rasterized full-page pixels, for the native window shell.
    pub pixmap: Option<tiny_skia::Pixmap>,
    /// What JavaScript did before this page was rendered (feature `js`).
    pub js: JsReport,
    /// Visible text of the rendered page, whitespace-collapsed.
    pub text: String,
}

impl PageRender {
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }
}

/// Render whatever `input` points at: a bare host, a full URL, or something
/// that needs a scheme prepended.
pub fn render(input: &str, viewport_w: u32, fetch_images: bool) -> PageRender {
    let started = Instant::now();
    let url = match net::normalize(input) {
        Ok(u) => u,
        Err(e) => {
            let info = ErrorInfo {
                headline: e.headline().to_string(),
                detail: e.detail(),
                code: "ERR_INVALID_URL".to_string(),
            };
            return error_page(input, input, &info, viewport_w, fetch_images, started);
        }
    };

    let t_fetch = Instant::now();
    let fetched = match net::fetch_cached(&url, net::MAX_BODY_BYTES, Duration::from_secs(60)) {
        Ok(f) => f,
        Err(e) => {
            let code = match e.kind {
                FetchErrorKind::Dns => "DNS_PROBE_FINISHED_NXDOMAIN",
                FetchErrorKind::Connection => "ERR_CONNECTION_REFUSED",
                FetchErrorKind::Timeout => "ERR_TIMED_OUT",
                FetchErrorKind::Tls => "ERR_CERT_AUTHORITY_INVALID",
                FetchErrorKind::TooManyRedirects => "ERR_TOO_MANY_REDIRECTS",
                FetchErrorKind::Http(c) => {
                    return http_error_page(&url, c, viewport_w, fetch_images, started);
                }
                FetchErrorKind::InvalidUrl => "ERR_INVALID_URL",
                _ => "ERR_FAILED",
            };
            let info = ErrorInfo {
                headline: e.headline().to_string(),
                detail: e.detail(),
                code: code.to_string(),
            };
            return error_page(
                url.as_str(),
                url.as_str(),
                &info,
                viewport_w,
                fetch_images,
                started,
            );
        }
    };
    let fetch_elapsed = t_fetch.elapsed();

    if fetched.status >= 400 {
        return http_error_page_at(
            &url,
            &fetched.final_url,
            fetched.status,
            fetch_elapsed,
            viewport_w,
            fetch_images,
            started,
        );
    }

    let ct = fetched.content_type.clone();
    let ct_lower = ct.to_ascii_lowercase();
    let final_url = fetched.final_url.clone();

    // Non-HTML payloads are wrapped so the engine still renders something sane.
    let (html_text, base) = if ct_lower.starts_with("image/") {
        (
            format!(
                "<html><head><title>{} (image)</title></head>\
                 <body style='margin:0;background:#202124;text-align:center'>\
                 <img src='{}' style='max-width:100%'></body></html>",
                html_escape(final_url.as_str()),
                html_escape(final_url.as_str())
            ),
            final_url.clone(),
        )
    } else if ct_lower.starts_with("text/plain") || ct_lower.contains("json") {
        (
            format!(
                "<html><head><title>Plain text</title></head>\
                 <body style='margin:0;padding:12px;background:#fff'>\
                 <pre style='font-family:monospace;font-size:13px;margin:0;white-space:pre'>{}</pre>\
                 </body></html>",
                html_escape(&fetched.text())
            ),
            final_url.clone(),
        )
    } else {
        (fetched.text(), final_url.clone())
    };

    let mut timings = Timings {
        fetch: fetch_elapsed,
        ..Default::default()
    };

    // JavaScript (feature `js`): run the page's scripts against a mutable
    // mirror of the DOM before the immutable pipeline parses it.
    #[cfg(feature = "js")]
    let (html_text, js_report) = {
        let t = Instant::now();
        let (h, mut r) = crate::js::preprocess(&html_text, base.as_str());
        r.ms = t.elapsed().as_millis() as u64;
        (h, r)
    };
    #[cfg(not(feature = "js"))]
    let js_report = JsReport::default();

    let t = Instant::now();
    let doc = Html::parse_document(&html_text);
    timings.parse = t.elapsed();

    render_parsed(
        &doc,
        RenderInput {
            base,
            requested: url.as_str().to_string(),
            status: fetched.status,
            content_type: ct,
            viewport_w,
            fetch_images,
            timings,
            started,
            error: None,
            js: js_report,
        },
    )
}

#[allow(clippy::too_many_arguments)]
/// Everything `render_parsed` needs besides the already-parsed document.
struct RenderInput {
    base: Url,
    requested: String,
    status: u16,
    content_type: String,
    viewport_w: u32,
    fetch_images: bool,
    timings: Timings,
    started: Instant,
    error: Option<ErrorInfo>,
    js: JsReport,
}

fn render_parsed(doc: &Html, input: RenderInput) -> PageRender {
    let RenderInput {
        mut base,
        requested,
        status,
        content_type,
        viewport_w,
        fetch_images,
        mut timings,
        started,
        error,
        js,
    } = input;
    // <base href="..."> overrides the document URL for relative resolution.
    if let Some(b) = doc.select(&crate::sel("base[href]")).next()
        && let Some(href) = b.attr("href")
        && let Ok(joined) = base.join(href.trim())
    {
        base = joined;
    }

    let t = Instant::now();
    let mut sheets: Vec<Stylesheet> = Vec::new();
    let (ua, mut order) = Stylesheet::parse(style::UA_CSS, 0);
    sheets.push(ua);

    // Author stylesheets in document order: <link rel=stylesheet> and <style>.
    for node in doc.select(&crate::sel("link[href], style")) {
        if sheets.len() > MAX_STYLESHEETS {
            break;
        }
        let tag = node.value().name();
        if tag == "style" {
            let css: String = node.text().collect();
            let (s, next) = Stylesheet::parse(&css, order);
            order = next;
            sheets.push(s);
        } else {
            let rel = node.attr("rel").unwrap_or("").to_ascii_lowercase();
            if !rel.contains("stylesheet") {
                continue;
            }
            let href = match node.attr("href") {
                Some(h) => h,
                None => continue,
            };
            let abs = match base.join(href.trim()) {
                Ok(a) => a,
                Err(_) => continue,
            };
            if let Ok(res) = net::fetch_cached(&abs, MAX_CSS_BYTES, Duration::from_secs(300))
                && res.status < 400
            {
                let css = res.text();
                let (s, next) = Stylesheet::parse(&css, order);
                order = next;
                sheets.push(s);
            }
        }
    }
    timings.css = t.elapsed();

    let t = Instant::now();
    let mut out: LayoutOutput = {
        let mut lay = Layout::new(doc, &sheets, base.clone(), viewport_w as f32, fetch_images);
        lay.run()
    };
    timings.layout = t.elapsed();

    let t = Instant::now();
    let painted = paint::render_to_png(&out, viewport_w);
    timings.paint = t.elapsed();
    timings.total = started.elapsed();

    let links = std::mem::take(&mut out.links);
    let stats = out.stats;
    let title = out.title.clone();
    let favicon = out.favicon.clone();

    let (png, width, height, clipped, pixmap) = match painted {
        Some(p) => (p.png, p.width, p.height, p.clipped, p.pixmap),
        None => (Vec::new(), viewport_w, 1, false, None),
    };

    PageRender {
        requested,
        text: visible_text(doc),
        final_url: base.to_string(),
        status,
        content_type,
        title: if title.is_empty() {
            base.host_str().unwrap_or("Untitled").to_string()
        } else {
            title
        },
        favicon,
        png,
        width,
        height,
        links,
        stats,
        timings,
        clipped,
        sheets: sheets.len(),
        error,
        pixmap,
        js,
    }
}

/// Whitespace-collapsed visible text of a parsed document (script/style/
/// template/noscript content excluded). Used by the JS compat corpus and
/// exposed through `/info`.
pub fn visible_text(doc: &Html) -> String {
    fn walk(n: ego_tree::NodeRef<scraper::Node>, out: &mut String, skip: bool) {
        let skipping = match n.value() {
            scraper::Node::Element(e) => {
                skip || matches!(e.name(), "script" | "style" | "template" | "noscript")
            }
            _ => skip,
        };
        if let scraper::Node::Text(t) = n.value()
            && !skipping
        {
            for w in t.text.split_whitespace() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(w);
            }
        }
        for c in n.children() {
            walk(c, out, skipping);
        }
    }
    let mut out = String::new();
    walk(*doc.root_element(), &mut out, false);
    out
}

/// Render a literal HTML document, with no network involved. Used for the
/// window shell's start page and by tests.
pub fn render_html(html: &str, base: &str, viewport_w: u32, fetch_images: bool) -> PageRender {
    let doc = Html::parse_document(html);
    let base = Url::parse(base).unwrap_or_else(|_| Url::parse("https://start.ferris/").unwrap());
    let requested = base.to_string();
    render_parsed(
        &doc,
        RenderInput {
            base,
            requested,
            status: 200,
            content_type: "text/html".to_string(),
            viewport_w,
            fetch_images,
            timings: Timings::default(),
            started: Instant::now(),
            error: None,
            js: JsReport::default(),
        },
    )
}

// ---------------------------------------------------------------------------
// error pages
// ---------------------------------------------------------------------------

fn error_page_html(headline: &str, detail: &str, code: &str, url: &str) -> String {
    format!(
        r#"<html><head><title>{headline}</title><style>
body {{ font-family: sans-serif; margin: 0; background: #fff; color: #202124 }}
.wrap {{ margin: 0; padding: 64px 48px }}
h1 {{ font-size: 26px; font-weight: normal; margin: 0 0 12px 0; color: #202124 }}
.url {{ font-size: 14px; color: #5f6368; margin: 0 0 28px 0; word-wrap: break-word }}
.detail {{ font-size: 14px; color: #3c4043; margin: 0 0 8px 0; line-height: 20px }}
ul {{ margin: 8px 0 0 0; padding-left: 24px; font-size: 14px; color: #3c4043; line-height: 22px }}
.code {{ font-family: monospace; font-size: 12px; color: #80868b; margin: 32px 0 0 0 }}
.rule {{ border-top: 1px solid #dadce0; margin: 24px 0 0 0; height: 0 }}
.icon {{ font-size: 44px; margin: 0 0 18px 0; color: #5f6368 }}
</style></head><body><div class="wrap">
<div class="icon">!</div>
<h1>{headline}</h1>
<p class="url">{url}</p>
<p class="detail">{detail}</p>
<div class="rule"></div>
<ul>
<li>Check the address for typos.</li>
<li>If the address is correct, the server may be unreachable from this sandbox.</li>
<li>Some sites block datacenter traffic or non-standard clients.</li>
</ul>
<p class="code">{code}</p>
</div></body></html>"#,
        headline = html_escape(headline),
        detail = html_escape(detail),
        code = html_escape(code),
        url = html_escape(url),
    )
}

#[allow(clippy::too_many_arguments)]
fn error_page(
    requested: &str,
    display_url: &str,
    info: &ErrorInfo,
    viewport_w: u32,
    fetch_images: bool,
    started: Instant,
) -> PageRender {
    let html = error_page_html(&info.headline, &info.detail, &info.code, display_url);
    let doc = Html::parse_document(&html);
    let base = net::normalize(display_url).unwrap_or_else(|_| {
        Url::parse("about:error").unwrap_or_else(|_| Url::parse("https://invalid/").unwrap())
    });
    let mut r = render_parsed(
        &doc,
        RenderInput {
            base,
            requested: requested.to_string(),
            status: 0,
            content_type: "text/html".to_string(),
            viewport_w,
            fetch_images,
            timings: Timings::default(),
            started,
            error: Some(info.clone()),
            js: JsReport::default(),
        },
    );
    r.title = info.headline.clone();
    r
}

fn http_error_page_at(
    requested: &Url,
    final_url: &Url,
    status: u16,
    fetch: Duration,
    viewport_w: u32,
    fetch_images: bool,
    started: Instant,
) -> PageRender {
    let (headline, detail) = http_status_copy(status, final_url.as_str());
    let info = ErrorInfo {
        headline: headline.to_string(),
        detail,
        code: format!("HTTP_{status}"),
    };
    let mut r = error_page(
        requested.as_str(),
        final_url.as_str(),
        &info,
        viewport_w,
        fetch_images,
        started,
    );
    r.status = status;
    r.timings.fetch = fetch;
    r
}

fn http_error_page(
    url: &Url,
    status: u16,
    viewport_w: u32,
    fetch_images: bool,
    started: Instant,
) -> PageRender {
    let (headline, detail) = http_status_copy(status, url.as_str());
    let info = ErrorInfo {
        headline: headline.to_string(),
        detail,
        code: format!("HTTP_{status}"),
    };
    let mut r = error_page(
        url.as_str(),
        url.as_str(),
        &info,
        viewport_w,
        fetch_images,
        started,
    );
    r.status = status;
    r
}

fn http_status_copy(status: u16, url: &str) -> (&'static str, String) {
    let headline = match status {
        400 => "Bad request",
        401 => "Sign in required",
        403 => "Access denied",
        404 => "This page isn't there",
        405 => "Method not allowed",
        408 => "Request timed out",
        429 => "Too many requests",
        500 => "The server hit an error",
        502 => "Bad gateway",
        503 => "Service unavailable",
        504 => "Gateway timed out",
        _ => "The server reported an error",
    };
    let detail = match status {
        403 => format!(
            "The server refused to serve {url}. Many public sites block requests \
             from datacenter IP ranges or from clients they do not recognise."
        ),
        404 => format!("No resource exists at {url}."),
        429 => format!("The server rate-limited this request to {url}. Try again shortly."),
        s if s >= 500 => format!("The server at {url} returned HTTP {s}."),
        s => format!("The server returned HTTP {s} for {url}."),
    };
    (headline, detail)
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html() {
        assert_eq!(
            html_escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }

    #[test]
    fn status_copy_is_specific() {
        assert_eq!(http_status_copy(404, "u").0, "This page isn't there");
        assert_eq!(http_status_copy(403, "u").0, "Access denied");
        assert!(http_status_copy(500, "u").1.contains("500"));
        assert!(http_status_copy(418, "u").1.contains("418"));
    }

    /// Error pages must go through the real engine, not a hardcoded image.
    #[test]
    fn error_page_renders_through_pipeline() {
        let started = Instant::now();
        let info = ErrorInfo {
            headline: "This site can't be reached".to_string(),
            detail: "DNS lookup failed.".to_string(),
            code: "DNS_PROBE_FINISHED_NXDOMAIN".to_string(),
        };
        let r = error_page(
            "https://example.invalid/x",
            "https://example.invalid/x",
            &info,
            800,
            false,
            started,
        );
        assert!(r.error.is_some());
        assert!(!r.png.is_empty());
        assert!(r.height > 100);
        assert!(r.stats.lines > 3, "error page should have several lines");
    }

    #[test]
    fn invalid_input_yields_error_render() {
        let r = render("not a url at all", 600, false);
        assert!(r.error.is_some());
        assert!(!r.png.is_empty());
    }
}
