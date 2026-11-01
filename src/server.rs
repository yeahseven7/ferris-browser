//! A minimal HTTP/1.1 server that exposes the engine as a usable browser:
//! a chrome UI with an address bar, and a viewport that shows the page our own
//! engine rasterized, with clickable link regions overlaid on top of it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::engine::{self, PageRender};

/// Concurrent rasterizations allowed. Painting is CPU-bound and allocates a
/// full-page canvas, so this bounds peak memory.
const MAX_CONCURRENT_RENDERS: usize = 3;
const RENDER_CACHE_ENTRIES: usize = 24;

pub struct Server {
    pub port: u16,
    active: Arc<AtomicUsize>,
    cache: Arc<Mutex<HashMap<String, Arc<CachedPage>>>>,
    order: Arc<Mutex<Vec<String>>>,
}

struct CachedPage {
    png: Vec<u8>,
    title: String,
    status: u16,
    final_url: String,
    links: Vec<(f32, f32, f32, f32, String)>,
    info: String,
    clipped: bool,
    width: u32,
    height: u32,
    error: Option<String>,
}

impl Server {
    pub fn new(port: u16) -> Server {
        Server {
            port,
            active: Arc::new(AtomicUsize::new(0)),
            cache: Arc::new(Mutex::new(HashMap::new())),
            order: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn run(&self) -> std::io::Result<()> {
        let addr = format!("0.0.0.0:{}", self.port);
        let listener = TcpListener::bind(&addr)?;
        let bound = listener.local_addr()?;
        println!(
            "Ferris browser engine listening on http://0.0.0.0:{}",
            bound.port()
        );
        println!("  GET /            browser chrome UI");
        println!("  GET /view?url=   rendered page + clickable link overlay");
        println!("  GET /render?url= raw PNG of the rasterized page");
        println!("  GET /info?url=   JSON render metadata");

        for stream in listener.incoming() {
            match stream {
                Ok(s) => {
                    let active = self.active.clone();
                    let cache = self.cache.clone();
                    let order = self.order.clone();
                    std::thread::spawn(move || {
                        let _ = handle(s, active, cache, order);
                    });
                }
                Err(_) => continue,
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn cache_get(&self, key: &str) -> Option<Arc<CachedPage>> {
        self.cache.lock().ok()?.get(key).cloned()
    }

    #[cfg(test)]
    fn cache_put(&self, key: &str, page: Arc<CachedPage>) {
        let (mut c, mut o) = match (self.cache.lock(), self.order.lock()) {
            (Ok(c), Ok(o)) => (c, o),
            _ => return,
        };
        if !c.contains_key(key) {
            o.push(key.to_string());
            while o.len() > RENDER_CACHE_ENTRIES {
                if let Some(victim) = o.first().cloned() {
                    o.remove(0);
                    c.remove(&victim);
                } else {
                    break;
                }
            }
        }
        c.insert(key.to_string(), page);
    }
}

/// Render (or reuse) a page, serializing heavy work behind a small semaphore.
fn render_page(
    key: &str,
    url: &str,
    width: u32,
    images: bool,
    srv_cache: Arc<Mutex<HashMap<String, Arc<CachedPage>>>>,
    srv_order: Arc<Mutex<Vec<String>>>,
    active: Arc<AtomicUsize>,
) -> Arc<CachedPage> {
    if let (Ok(c),) = (srv_cache.lock(),)
        && let Some(hit) = c.get(key)
    {
        return hit.clone();
    }

    // Wait briefly for a render slot rather than piling up CPU work.
    let mut waited = 0;
    while active.load(Ordering::SeqCst) >= MAX_CONCURRENT_RENDERS && waited < 200 {
        std::thread::sleep(Duration::from_millis(50));
        waited += 1;
    }
    active.fetch_add(1, Ordering::SeqCst);
    let r: PageRender = engine::render(url, width, images);
    active.fetch_sub(1, Ordering::SeqCst);

    let links: Vec<(f32, f32, f32, f32, String)> = r
        .links
        .iter()
        .map(|l| (l.x, l.y, l.w, l.h, l.href.clone()))
        .collect();

    let info = format!(
        "{{\"status\":{},\"title\":{},\"url\":{},\"width\":{},\"height\":{},\"clipped\":{},\
\"sheets\":{},\"error\":{},\"links\":{},\"stats\":{{\"nodes\":{},\"cmds\":{},\"images\":{},\
\"imagesFailed\":{},\"chars\":{},\"lines\":{},\"truncated\":{}}},\
\"timings\":{{\"fetch\":{:.1},\"parse\":{:.1},\"css\":{:.1},\"layout\":{:.1},\"paint\":{:.1},\"total\":{:.1}}},\
\"js\":{{\"enabled\":{},\"scripts\":{},\"ms\":{},\"console\":[{}],\"errors\":[{}]}},\"textLen\":{}}}",
        r.status,
        json_str(&r.title),
        json_str(&r.final_url),
        r.width,
        r.height,
        r.clipped,
        r.sheets,
        match &r.error {
            Some(e) => json_str(&format!("{} ({})", e.headline, e.code)),
            None => "null".to_string(),
        },
        links.len(),
        r.stats.nodes,
        r.stats.cmds,
        r.stats.images,
        r.stats.images_failed,
        r.stats.chars,
        r.stats.lines,
        r.stats.truncated,
        ms(r.timings.fetch),
        ms(r.timings.parse),
        ms(r.timings.css),
        ms(r.timings.layout),
        ms(r.timings.paint),
        ms(r.timings.total),
        r.js.enabled,
        r.js.scripts,
        r.js.ms,
        r.js.console
            .iter()
            .map(|l| json_str(l))
            .collect::<Vec<_>>()
            .join(","),
        r.js.errors
            .iter()
            .map(|l| json_str(l))
            .collect::<Vec<_>>()
            .join(","),
        r.text.len(),
    );

    let page = Arc::new(CachedPage {
        png: r.png,
        title: r.title,
        status: r.status,
        final_url: r.final_url,
        links,
        info,
        clipped: r.clipped,
        width: r.width,
        height: r.height,
        error: r.error.map(|e| e.headline),
    });

    if let (Ok(mut c), Ok(mut o)) = (srv_cache.lock(), srv_order.lock()) {
        if !c.contains_key(key) {
            o.push(key.to_string());
            while o.len() > RENDER_CACHE_ENTRIES {
                if let Some(victim) = o.first().cloned() {
                    o.remove(0);
                    c.remove(&victim);
                } else {
                    break;
                }
            }
        }
        c.insert(key.to_string(), page.clone());
    }
    page
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// HTTP plumbing
// ---------------------------------------------------------------------------

fn handle(
    stream: TcpStream,
    active: Arc<AtomicUsize>,
    cache: Arc<Mutex<HashMap<String, Arc<CachedPage>>>>,
    order: Arc<Mutex<Vec<String>>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    // Drain headers; we do not need them, but the stream must be consumed.
    let mut line = String::new();
    let mut guard = 0;
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        guard += 1;
        if n == 0 || guard > 200 || line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");

    if method != "GET" && method != "HEAD" {
        return respond(
            &mut stream.try_clone()?,
            405,
            "text/plain",
            b"method not allowed",
        );
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };
    let params = parse_query(&query);

    match path.as_str() {
        "/" | "/index.html" => respond(
            &mut stream.try_clone()?,
            200,
            "text/html; charset=utf-8",
            CHROME_HTML.as_bytes(),
        ),
        "/health" => respond(&mut stream.try_clone()?, 200, "text/plain", b"ok"),
        "/render" => {
            let url = params.get("url").cloned().unwrap_or_default();
            let w = parse_width(params.get("w"));
            let images = params.get("img").map(|v| v != "0").unwrap_or(true);
            let key = format!("{}|{}|{}", url, w, images as u8);
            let page = render_page(&key, &url, w, images, cache, order, active);
            respond_png(&mut stream.try_clone()?, &page.png)
        }
        "/info" => {
            let url = params.get("url").cloned().unwrap_or_default();
            let w = parse_width(params.get("w"));
            let images = params.get("img").map(|v| v != "0").unwrap_or(true);
            let key = format!("{}|{}|{}", url, w, images as u8);
            let page = render_page(&key, &url, w, images, cache, order, active);
            respond(
                &mut stream.try_clone()?,
                200,
                "application/json; charset=utf-8",
                page.info.as_bytes(),
            )
        }
        "/view" => {
            let url = params.get("url").cloned().unwrap_or_default();
            let w = parse_width(params.get("w"));
            let images = params.get("img").map(|v| v != "0").unwrap_or(true);
            let key = format!("{}|{}|{}", url, w, images as u8);
            let page = render_page(&key, &url, w, images, cache, order, active);
            let html = view_html(&page, &url, w, images);
            respond(
                &mut stream.try_clone()?,
                200,
                "text/html; charset=utf-8",
                html.as_bytes(),
            )
        }
        _ => respond(&mut stream.try_clone()?, 404, "text/plain", b"not found"),
    }
}

fn parse_width(v: Option<&String>) -> u32 {
    v.and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1000)
        .clamp(200, 2400)
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if q.is_empty() {
        return m;
    }
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        m.insert(url_decode(k), url_decode(v));
    }
    m
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = (b[i + 1] as char).to_digit(16);
                let l = (b[i + 2] as char).to_digit(16);
                match (h, l) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn respond(stream: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {ctype}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn respond_png(stream: &mut TcpStream, png: &[u8]) -> std::io::Result<()> {
    if png.is_empty() {
        return respond(stream, 500, "text/plain", b"render failed");
    }
    respond(stream, 200, "image/png", png)
}

/// Build the viewport document: the rasterized page as an <img>, with an
/// absolutely positioned anchor over every link region we found during layout.
fn view_html(page: &CachedPage, url: &str, w: u32, images: bool) -> String {
    let mut out = String::with_capacity(page.links.len() * 160 + 2048);
    let q = format!(
        "url={}&w={}&img={}",
        url_encode(url),
        w,
        if images { "1" } else { "0" }
    );
    out.push_str(&format!(
        "<!doctype html><html><head><meta charset='utf-8'>\
         <meta name='viewport' content='width=device-width'>\
         <title>{}</title><style>\
         html,body{{margin:0;padding:0;background:#fff}}\
         #page{{position:relative;width:{}px;height:{}px;overflow:hidden}}\
         #page img{{display:block;width:{}px;height:{}px}}\
         #page a.lk{{position:absolute;display:block;text-decoration:none;outline:0}}\
         #page.show a.lk:hover{{outline:2px solid rgba(26,115,232,.55);background:rgba(26,115,232,.10)}}\
         #clip{{font:12px/1.5 sans-serif;color:#5f6368;background:#f1f3f4;border-top:1px solid #dadce0;padding:6px 10px}}\
         </style></head><body>\
         <div id='page' class='show'>\
         <img src='/render?{}' width='{}' height='{}' alt=''>",
        engine::html_escape(&page.title),
        page.width,
        page.height,
        page.width,
        page.height,
        q,
        page.width,
        page.height,
    ));

    for (x, y, lw, lh, href) in &page.links {
        if *lw < 1.0 || *lh < 1.0 {
            continue;
        }
        let target = format!(
            "url={}&w={}&img={}",
            url_encode(href),
            w,
            if images { "1" } else { "0" }
        );
        out.push_str(&format!(
            "<a class='lk' href='/view?{}' style='left:{}px;top:{}px;width:{}px;height:{}px' title='{}'></a>",
            target,
            x.round() as i32,
            y.round() as i32,
            lw.ceil() as u32,
            lh.ceil() as u32,
            engine::html_escape(href),
        ));
    }

    out.push_str("</div>");
    let redirected = page.final_url.trim_end_matches('/') != url.trim_end_matches('/');
    if redirected {
        out.push_str(&format!(
            "<div id='clip'>Redirected (HTTP {}) to {}</div>",
            page.status,
            engine::html_escape(&page.final_url)
        ));
    }
    if page.clipped {
        out.push_str(&format!(
            "<div id='clip'>Page is taller than the {}px canvas cap; the bottom was clipped.</div>",
            crate::paint::MAX_PAGE_HEIGHT
        ));
    }
    if let Some(e) = &page.error {
        out.push_str(&format!(
            "<div id='clip'>Navigation failed: {}</div>",
            engine::html_escape(e)
        ));
    }
    out.push_str("</body></html>");
    out
}

// ---------------------------------------------------------------------------
// the browser chrome
// ---------------------------------------------------------------------------

const CHROME_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Ferris — a browser engine written in Rust</title>
<style>
  :root {
    --bg: #202124; --panel: #292a2d; --line: #3c4043; --fg: #e8eaed;
    --dim: #9aa0a6; --accent: #8ab4f8; --ok: #81c995; --bad: #f28b82;
  }
  * { box-sizing: border-box }
  html, body { margin:0; height:100%; background:var(--bg); color:var(--fg);
    font:13px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif }
  body { display:flex; flex-direction:column; height:100vh; overflow:hidden }

  #bar { display:flex; align-items:center; gap:6px; padding:8px 10px;
    background:var(--panel); border-bottom:1px solid var(--line); flex:0 0 auto }
  button { background:transparent; border:1px solid transparent; color:var(--dim);
    width:32px; height:32px; border-radius:50%; cursor:pointer; font-size:15px;
    display:flex; align-items:center; justify-content:center; flex:0 0 auto }
  button:hover:not(:disabled) { background:#3c4043; color:var(--fg) }
  button:disabled { opacity:.32; cursor:default }
  #addrwrap { flex:1 1 auto; display:flex; align-items:center; gap:8px;
    background:var(--bg); border:1px solid var(--line); border-radius:16px;
    padding:0 12px; height:32px; min-width:0 }
  #addrwrap:focus-within { border-color:var(--accent) }
  #lock { color:var(--dim); font-size:12px; flex:0 0 auto }
  #addr { flex:1 1 auto; background:transparent; border:0; outline:0; color:var(--fg);
    font:13px/1.5 inherit; min-width:0 }
  #go { background:var(--accent); color:#202124; font-weight:600; width:auto;
    padding:0 14px; border-radius:16px; font-size:12px }
  #go:hover:not(:disabled) { background:#aecbfa; color:#202124 }
  label.chk { display:flex; align-items:center; gap:4px; color:var(--dim);
    font-size:11px; cursor:pointer; user-select:none; white-space:nowrap }

  #spin { width:14px; height:14px; border:2px solid var(--line);
    border-top-color:var(--accent); border-radius:50%; display:none; flex:0 0 auto }
  #spin.on { display:block; animation:sp .7s linear infinite }
  @keyframes sp { to { transform:rotate(360deg) } }

  #stage { flex:1 1 auto; position:relative; min-height:0; background:#fff }
  iframe { position:absolute; inset:0; width:100%; height:100%; border:0; background:#fff }
  #hint { position:absolute; inset:0; display:flex; align-items:center;
    justify-content:center; text-align:center; color:var(--dim); padding:24px }

  #status { flex:0 0 auto; display:flex; gap:14px; align-items:center;
    padding:5px 10px; background:var(--panel); border-top:1px solid var(--line);
    font-size:11px; color:var(--dim); overflow-x:auto; white-space:nowrap }
  #status b { color:var(--fg); font-weight:600 }
  #status .ok { color:var(--ok) } #status .bad { color:var(--bad) }
  .pill { border:1px solid var(--line); border-radius:9px; padding:0 7px }
</style>
</head>
<body>
  <div id="bar">
    <button id="back" title="Back" disabled>&#8592;</button>
    <button id="fwd" title="Forward" disabled>&#8594;</button>
    <button id="reload" title="Reload" disabled>&#8635;</button>
    <button id="home" title="Home">&#8962;</button>
    <div id="addrwrap">
      <span id="spin"></span>
      <span id="lock" title="Connection">&#128274;</span>
      <input id="addr" spellcheck="false" autocomplete="off"
             placeholder="Search or enter address" value="">
    </div>
    <label class="chk" title="Fetch and rasterize &lt;img&gt; elements">
      <input type="checkbox" id="imgs" checked>images
    </label>
    <button id="go" title="Go">Go</button>
  </div>

  <div id="stage">
    <iframe id="frame" title="Ferris viewport"></iframe>
    <div id="hint">
      <div>
        <div style="font-size:40px;color:#8ab4f8">&#129440;</div>
        <div style="font-size:15px;color:#e8eaed;margin:10px 0 4px">
          Ferris &mdash; a browser engine written in Rust
        </div>
        <div style="max-width:520px">
          Type an address above. Ferris fetches it over TLS, parses the HTML with
          html5ever, applies the CSS cascade, runs its own block/inline/table
          layout, and rasterizes the result to pixels &mdash; no Chromium, no
          WebKit, no system browser involved.
        </div>
        <div style="margin-top:14px;display:flex;gap:8px;justify-content:center;flex-wrap:wrap" id="chips"></div>
      </div>
    </div>
  </div>

  <div id="status">
    <span id="st-title">ready</span>
    <span id="st-http"></span>
    <span id="st-perf"></span>
    <span id="st-dom"></span>
    <span id="st-net"></span>
    <span class="pill" id="st-engine">engine: rust</span>
  </div>

<script>
(function () {
  var $ = function (id) { return document.getElementById(id); };
  var addr = $('addr'), frame = $('frame'), spin = $('spin'), hint = $('hint');
  var back = $('back'), fwd = $('fwd'), reload = $('reload'), home = $('home');
  var imgs = $('imgs'), lock = $('lock');
  var history = [], hindex = -1, current = null, busy = false;

  var SUGGESTED = ['example.com', 'news.ycombinator.com', 'httpbin.org/html',
                   'www.rust-lang.org', 'info.cern.ch', 'motherfuckingwebsite.com'];
  var chips = $('chips');
  SUGGESTED.forEach(function (s) {
    var b = document.createElement('button');
    b.textContent = s;
    b.style.cssText = 'width:auto;height:auto;border-radius:14px;border:1px solid #3c4043;' +
                      'padding:4px 11px;font-size:11px;color:#9aa0a6';
    b.onclick = function () { addr.value = s; navigate(s, true); };
    chips.appendChild(b);
  });

  function viewportWidth() {
    var w = $('stage').clientWidth;
    return Math.max(200, Math.min(2400, Math.floor(w)));
  }

  function viewUrl(u) {
    return '/view?url=' + encodeURIComponent(u) + '&w=' + viewportWidth() +
           '&img=' + (imgs.checked ? '1' : '0');
  }

  function setBusy(on) {
    busy = on;
    spin.className = on ? 'on' : '';
    $('go').disabled = on;
    reload.disabled = on || !current;
  }

  function setStatus(info) {
    $('st-title').innerHTML = '<b>' + esc(info.title || current || '') + '</b>';
    $('st-http').innerHTML = info.status
      ? '<span class="' + (info.status < 400 ? 'ok' : 'bad') + '">HTTP ' + info.status + '</span>'
      : '';
    var t = info.timings || {};
    $('st-perf').textContent = 'layout ' + f(t.layout) + 'ms \u00b7 paint ' + f(t.paint) +
                               'ms \u00b7 total ' + f(t.total) + 'ms';
    var s = info.stats || {};
    $('st-dom').textContent = s.nodes + ' nodes \u00b7 ' + s.lines + ' lines \u00b7 ' +
                              s.cmds + ' paint ops \u00b7 ' + (info.links || 0) + ' links';
    $('st-net').textContent = (s.images || 0) + ' images' +
                              (s.imagesFailed ? ' (' + s.imagesFailed + ' failed)' : '') +
                              ' \u00b7 ' + (info.sheets || 0) + ' stylesheets';
    lock.innerHTML = info.error ? '&#9888;' : (String(current || '').indexOf('https') === 0 ? '&#128274;' : '&#9432;');
    lock.style.color = info.error ? 'var(--bad)' : 'var(--dim)';
  }
  function f(n) { return (typeof n === 'number') ? (Math.round(n * 10) / 10) : '0'; }
  function esc(s) {
    return String(s).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }

  function normalize(u) {
    u = u.trim();
    if (!u) return null;
    if (/^https?:\/\//i.test(u)) return u;
    if (/^[\w-]+(\.[\w-]+)+(:\d+)?(\/|$|\?|#)/.test(u)) return 'https://' + u;
    return 'https://' + u;
  }

  function navigate(u, pushHistory) {
    var full = normalize(u);
    if (!full) return;
    addr.value = full;
    hint.style.display = 'none';
    current = full;
    setBusy(true);
    if (pushHistory) {
      history = history.slice(0, hindex + 1);
      history.push(full);
      hindex = history.length - 1;
    }
    syncNav();
    frame.src = viewUrl(full);
    // Metadata comes from a second endpoint; the render itself is cached server
    // side, so this is a cache hit rather than a second fetch.
    fetch('/info?url=' + encodeURIComponent(full) + '&w=' + viewportWidth() +
          '&img=' + (imgs.checked ? '1' : '0'))
      .then(function (r) { return r.json(); })
      .then(function (info) { setStatus(info); setBusy(false); })
      .catch(function () { setBusy(false); });
  }

  function syncNav() {
    back.disabled = hindex <= 0;
    fwd.disabled = hindex >= history.length - 1;
    reload.disabled = !current;
  }

  back.onclick = function () {
    if (hindex > 0) { hindex--; var u = history[hindex]; addr.value = u; current = u; navigate(u, false); }
  };
  fwd.onclick = function () {
    if (hindex < history.length - 1) { hindex++; var u = history[hindex]; addr.value = u; current = u; navigate(u, false); }
  };
  reload.onclick = function () { if (current) navigate(current, false); };
  home.onclick = function () {
    hint.style.display = 'flex'; current = null; frame.removeAttribute('src');
    $('st-title').textContent = 'ready';
    $('st-http').textContent = $('st-perf').textContent = $('st-dom').textContent = $('st-net').textContent = '';
    addr.value = ''; syncNav();
  };
  imgs.onchange = function () { if (current) navigate(current, false); };
  $('go').onclick = function () { navigate(addr.value, true); };
  addr.addEventListener('keydown', function (e) { if (e.key === 'Enter') navigate(addr.value, true); });

  // In-page links navigate the iframe itself, so mirror that into our history.
  frame.addEventListener('load', function () {
    try {
      var d = frame.contentDocument;
      if (!d || !d.getElementById('page')) return;
      var links = d.querySelectorAll('a.lk');
      for (var i = 0; i < links.length; i++) {
        (function (a) {
          a.addEventListener('click', function (ev) {
            ev.preventDefault();
            var m = /[?&]url=([^&]+)/.exec(a.getAttribute('href'));
            if (m) navigate(decodeURIComponent(m[1]), true);
          });
        })(links[i]);
      }
      syncNav();
    } catch (e) { /* cross-origin or not yet navigated */ }
  });

  // Re-render at the new width when the window is resized.
  var rt = null, lastW = viewportWidth();
  window.addEventListener('resize', function () {
    clearTimeout(rt);
    rt = setTimeout(function () {
      if (Math.abs(viewportWidth() - lastW) > 24 && current) {
        lastW = viewportWidth();
        frame.src = viewUrl(current);
      }
    }, 260);
  });

  syncNav();
})();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn query_roundtrip() {
        let enc = url_encode("https://a.test/x?y=1&z=2 #h");
        let q = format!("url={enc}&w=800");
        let p = parse_query(&q);
        assert_eq!(p.get("url").unwrap(), "https://a.test/x?y=1&z=2 #h");
        assert_eq!(p.get("w").unwrap(), "800");
    }

    #[test]
    fn plus_decodes_to_space() {
        let p = parse_query("url=hello+world");
        assert_eq!(p.get("url").unwrap(), "hello world");
    }

    #[test]
    fn width_is_clamped() {
        assert_eq!(parse_width(None), 1000);
        assert_eq!(parse_width(Some("50".to_string()).as_ref()), 200);
        assert_eq!(parse_width(Some("99999".to_string()).as_ref()), 2400);
        assert_eq!(parse_width(Some("abc".to_string()).as_ref()), 1000);
    }

    #[test]
    fn json_escapes_control_chars() {
        assert_eq!(json_str("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        assert_eq!(json_str("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn view_html_contains_overlay_anchors() {
        let page = CachedPage {
            png: vec![1, 2, 3],
            title: "T".into(),
            status: 200,
            final_url: "https://x.test/".into(),
            links: vec![(10.0, 20.0, 30.0, 16.0, "https://x.test/a".into())],
            info: "{}".into(),
            clipped: false,
            width: 400,
            height: 300,
            error: None,
        };
        let h = view_html(&page, "https://x.test/", 400, true);
        assert!(h.contains("a class='lk'"));
        assert!(h.contains("url=https%3A%2F%2Fx.test%2Fa"));
        assert!(h.contains("/render?url="));
        assert!(h.contains("left:10px;top:20px;width:30px;height:16px"));
        assert!(!h.contains("Page is"), "no clip notice when not clipped");
    }

    #[test]
    fn view_html_notes_clipping_and_errors() {
        let mut page = CachedPage {
            png: vec![],
            title: "T".into(),
            status: 500,
            final_url: "https://x.test/".into(),
            links: vec![],
            info: "{}".into(),
            clipped: true,
            width: 400,
            height: 300,
            error: Some("Access denied".into()),
        };
        let h = view_html(&page, "https://x.test/", 400, false);
        assert!(h.contains("clipped"));
        assert!(h.contains("Access denied"));
        assert!(h.contains("img=0"));
        page.error = None;
    }

    /// The chrome UI must not rely on external assets: the preview iframe is
    /// sandboxed, so anything not inline would silently fail to load.
    #[test]
    fn chrome_ui_is_self_contained() {
        assert!(!CHROME_HTML.contains("http://") && !CHROME_HTML.contains("https://cdn"));
        assert!(CHROME_HTML.contains("<script>"));
        assert!(CHROME_HTML.contains("id=\"addr\""));
        assert!(CHROME_HTML.contains("iframe"));
    }

    #[test]
    fn cache_evicts_beyond_limit() {
        let s = Server::new(0);
        for i in 0..(RENDER_CACHE_ENTRIES + 5) {
            let k = format!("k{i}");
            s.cache_put(
                &k,
                Arc::new(CachedPage {
                    png: vec![],
                    title: k.clone(),
                    status: 200,
                    final_url: k.clone(),
                    links: vec![],
                    info: "{}".into(),
                    clipped: false,
                    width: 1,
                    height: 1,
                    error: None,
                }),
            );
        }
        // NB: never hold the guard across cache_get(), which locks the same
        // non-reentrant mutex.
        let len_after = { s.cache.lock().unwrap().len() };
        assert!(
            len_after <= RENDER_CACHE_ENTRIES,
            "cache grew past its cap: {len_after}"
        );
        assert!(
            s.cache_get("k0").is_none(),
            "oldest entry should be evicted"
        );
        let newest = format!("k{}", RENDER_CACHE_ENTRIES + 4);
        assert!(
            s.cache_get(&newest).is_some(),
            "newest entry must still be cached"
        );
    }

    /// End-to-end: bind a real socket, issue a request, check the response.
    #[test]
    fn serves_health_and_chrome_over_a_real_socket() {
        let srv = Arc::new(Server::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let s2 = srv.clone();
        // The accept loop outlives the test; the process tears it down on exit.
        std::thread::spawn(move || {
            let l = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
            for st in l.incoming() {
                let Ok(st) = st else { continue };
                let a = s2.active.clone();
                let c = s2.cache.clone();
                let o = s2.order.clone();
                let _ = handle(st, a, c, o);
            }
        });
        std::thread::sleep(Duration::from_millis(150));

        let get = |path: &str| -> (String, Vec<u8>) {
            let mut s = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
            s.set_read_timeout(Some(Duration::from_secs(5))).ok();
            write!(s, "GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            let text = String::from_utf8_lossy(&buf).to_string();
            let head_end = text.find("\r\n\r\n").unwrap_or(0);
            (text[..head_end].to_string(), buf[head_end + 4..].to_vec())
        };

        let (h1, b1) = get("/health");
        assert!(h1.starts_with("HTTP/1.1 200"), "got {h1}");
        assert_eq!(b1, b"ok");

        let (h2, b2) = get("/");
        assert!(h2.starts_with("HTTP/1.1 200"));
        assert!(h2.contains("text/html"));
        let s = String::from_utf8_lossy(&b2);
        assert!(s.contains("id=\"addr\""));

        let (h3, _) = get("/nope");
        assert!(h3.starts_with("HTTP/1.1 404"));
    }
}
