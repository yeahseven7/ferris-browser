//! The native window shell: our own rasterized pixels in a real Wayland/X11
//! window (winit + softbuffer), with real browser furniture — a tab strip and
//! an editable address bar, both drawn by this engine's own glyph rasterizer.
//!
//! Everything interactive (frame composition, tab geometry, hit-testing,
//! scrolling, history, address-bar editing) is pure so it can be exercised
//! headlessly via `ferris shelltest` on machines with no display server.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Cursor, CursorIcon, Window, WindowId};

use crate::engine::{self, PageRender};
use crate::font::{BlitStyle, Family, fonts};
use crate::layout::LinkRect;

// ---------------------------------------------------------------------------
// chrome metrics
// ---------------------------------------------------------------------------

/// Tab strip height.
pub const TAB_H: u32 = 26;
/// URL bar height.
pub const TOOLBAR_H: u32 = 34;
/// Status strip height.
pub const STATUS_H: u32 = 22;

const BG_TABS: (u8, u8, u8) = (24, 25, 28);
const BG_TAB: (u8, u8, u8) = (41, 42, 45);
const BG_TAB_ACTIVE: (u8, u8, u8) = (53, 54, 58);
const BG_TOOLBAR: (u8, u8, u8) = (32, 33, 36);
const BG_FIELD: (u8, u8, u8) = (24, 25, 28);
const BG_STATUS: (u8, u8, u8) = (41, 42, 45);
const FG_BRIGHT: (u8, u8, u8) = (232, 234, 237);
const FG_DIM: (u8, u8, u8) = (154, 160, 166);
const ACCENT: (u8, u8, u8) = (138, 180, 248);
const ERROR: (u8, u8, u8) = (242, 139, 130);
const PAGE_FALLBACK: (u8, u8, u8) = (255, 255, 255);
const SEL: (u8, u8, u8) = (43, 74, 118);

const URL_FONT: f32 = 13.0;
const FIELD_X: f32 = 10.0;
const TEXT_X: f32 = 18.0;

/// Height of the page viewport inside a window of height `h`.
pub fn page_view_height(h: u32) -> u32 {
    h.saturating_sub(TAB_H)
        .saturating_sub(TOOLBAR_H)
        .saturating_sub(STATUS_H)
}

/// Y coordinate where the page slice begins.
pub fn page_top() -> u32 {
    TAB_H + TOOLBAR_H
}

// ---------------------------------------------------------------------------
// page view
// ---------------------------------------------------------------------------

/// A rendered page held ready for presentation: full-page pixels plus the
/// interactive metadata layout computed.
pub struct PageView {
    pub pixmap: tiny_skia::Pixmap,
    pub links: Vec<LinkRect>,
    pub title: String,
    pub url: String,
}

impl PageView {
    pub fn page_height(&self) -> u32 {
        self.pixmap.height()
    }
    pub fn page_width(&self) -> u32 {
        self.pixmap.width()
    }

    /// Fetch + render a URL through the engine.
    pub fn load(url: &str, width: u32, fetch_images: bool) -> Result<PageView, String> {
        let r = engine::render(url, width, fetch_images);
        Self::from_render(r)
    }

    /// Render literal HTML. Used for the start page. When `fetch_images` is
    /// set, remote images (e.g. video thumbnails) are fetched over the network.
    pub fn from_html(html: &str, width: u32, fetch_images: bool) -> Result<PageView, String> {
        let r = engine::render_html(html, "https://start.ferris/", width, fetch_images);
        Self::from_render(r)
    }

    fn from_render(mut r: PageRender) -> Result<PageView, String> {
        let pixmap = r
            .pixmap
            .take()
            .ok_or_else(|| "rendering produced no pixels".to_string())?;
        Ok(PageView {
            pixmap,
            links: r.links,
            title: r.title,
            url: r.final_url,
        })
    }

    /// The link under a point given in *page* coordinates, if any.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<&LinkRect> {
        self.links
            .iter()
            .find(|l| x >= l.x && x <= l.x + l.w && y >= l.y && y <= l.y + l.h)
    }
}

/// A few iconic, stable videos for the start-page cards. Ferris can't play
/// video itself, so these (and any YouTube URL) are handed to `mpv`/`yt-dlp`.
const START_VIDEOS: &[(&str, &str)] = &[
    ("jNQXAC9IVRw", "Me at the zoo — the very first YouTube video"),
    ("dQw4w9WgXcQ", "Rick Astley — Never Gonna Give You Up"),
    ("9bZkp7q19f0", "PSY — GANGNAM STYLE"),
];

/// The shell's start page, rendered by the engine itself.
pub fn start_page_html() -> String {
    let mut cards = String::new();
    for (id, title) in START_VIDEOS {
        let url = format!("https://www.youtube.com/watch?v={id}");
        // Text cards, not <img> thumbnails: the engine's inline replaced-element
        // paint (layout.rs `top = baseline - img_h`) overflows its line box, so
        // thumbnails misposition once other content shares the flow. Text links
        // render perfectly and still route through the mpv intercept. Thumbnail
        // cards return when replaced-element layout is fixed (see README).
        cards.push_str(&format!(
            "<li><a href=\"{url}\">{title}</a> \
             <span class=\"dim\">&mdash; plays in mpv</span></li>",
            url = url,
            title = crate::engine::html_escape(title),
        ));
    }
    format!(
        r#"<html><head><title>New tab</title><style>
body {{ font-family: sans-serif; margin: 0; background: #202124; color: #e8eaed }}
.wrap {{ padding: 48px }}
h1 {{ font-size: 30px; margin: 0 0 6px 0; color: #8ab4f8 }}
h2 {{ font-size: 18px; font-weight: normal; margin: 34px 0 6px 0; color: #e8eaed }}
p  {{ font-size: 14px; color: #9aa0a6; margin: 0 0 26px 0; line-height: 22px }}
ul {{ font-size: 15px; line-height: 30px; color: #e8eaed; padding-left: 22px }}
a  {{ color: #8ab4f8 }}
kbd {{ font-family: monospace; font-size: 12px; color: #e8eaed; background: #3c4043;
       padding: 1px 6px; border-radius: 4px }}
.hint {{ font-size: 13px; color: #9aa0a6; margin: 0 0 18px 0; line-height: 20px }}
.dim {{ color: #9aa0a6; font-size: 13px }}
</style></head><body><div class="wrap">
<h1>Ferris {version}</h1>
<p>A browser engine written in Rust. Every pixel in this window came from its own
rasterizer &mdash; no Chromium, WebKit or system browser anywhere.</p>
<ul>
<li><kbd>Ctrl+L</kbd> focus the address bar, type, <kbd>Enter</kbd> to go</li>
<li><kbd>Ctrl+T</kbd> new tab &middot; <kbd>Ctrl+W</kbd> close tab &middot; <kbd>Ctrl+Tab</kbd> next tab</li>
<li>click links to navigate &middot; wheel / arrows / <kbd>PgDn</kbd> to scroll</li>
<li><kbd>Alt+&#8592;</kbd> back &middot; <kbd>Alt+&#8594;</kbd> forward &middot; <kbd>F5</kbd> reload</li>
</ul>
<p>Try: <a href="https://example.com/">example.com</a> &middot;
<a href="https://news.ycombinator.com/">news.ycombinator.com</a> &middot;
<a href="http://info.cern.ch/">info.cern.ch</a></p>
<h2>Watch a video &mdash; opens in mpv</h2>
<p class="hint">Ferris has no video codec, so it hands every YouTube link to
<kbd>mpv</kbd> (which resolves the stream with <kbd>yt-dlp</kbd>). Click a title
below, or paste any YouTube URL into the address bar. Needs <kbd>mpv</kbd> and
<kbd>yt-dlp</kbd> installed.</p>
<ul>{cards}</ul>
</div></body></html>"#,
        version = crate::VERSION,
        cards = cards,
    )
}

// ---------------------------------------------------------------------------
// scrolling / history / address bar (pure, testable)
// ---------------------------------------------------------------------------

/// Clamp a scroll offset to the scrollable range of a page in a viewport.
pub fn clamp_scroll(scroll: i32, page_height: u32, view_height: u32) -> i32 {
    let max = (page_height as i32 - view_height as i32).max(0);
    scroll.clamp(0, max)
}

/// Linear per-tab history with a cursor.
#[derive(Default, Debug)]
pub struct History {
    entries: Vec<String>,
    index: isize,
}

impl History {
    pub fn push(&mut self, url: &str) {
        self.entries.truncate((self.index + 1).max(0) as usize);
        self.entries.push(url.to_string());
        self.index = self.entries.len() as isize - 1;
    }
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.index as usize).map(|s| s.as_str())
    }
    pub fn back(&mut self) -> Option<&str> {
        if self.index > 0 {
            self.index -= 1;
            self.current()
        } else {
            None
        }
    }
    pub fn forward(&mut self) -> Option<&str> {
        if (self.index + 1) < self.entries.len() as isize {
            self.index += 1;
            self.current()
        } else {
            None
        }
    }
    pub fn can_back(&self) -> bool {
        self.index > 0
    }
    pub fn can_forward(&self) -> bool {
        (self.index + 1) < self.entries.len() as isize
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The editable address bar: a char buffer, a caret, and select-all semantics.
#[derive(Default, Debug, Clone)]
pub struct AddressBar {
    chars: Vec<char>,
    caret: usize,
    select_all: bool,
    pub focused: bool,
}

impl AddressBar {
    pub fn set_text(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.caret = self.chars.len();
        self.select_all = false;
    }
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }
    pub fn caret(&self) -> usize {
        self.caret
    }
    pub fn selected_all(&self) -> bool {
        self.select_all
    }
    /// Text up to the caret, for caret x-position measurement.
    pub fn prefix(&self) -> String {
        self.chars.iter().take(self.caret).collect()
    }
    pub fn focus(&mut self, select_all: bool) {
        self.focused = true;
        self.select_all = select_all;
        if select_all {
            self.caret = self.chars.len();
        }
    }
    pub fn blur(&mut self) {
        self.focused = false;
        self.select_all = false;
    }
    pub fn select_all(&mut self) {
        self.select_all = true;
        self.caret = self.chars.len();
    }
    pub fn insert(&mut self, s: &str) {
        if self.select_all {
            self.chars.clear();
            self.caret = 0;
            self.select_all = false;
        }
        for ch in s.chars() {
            self.chars.insert(self.caret, ch);
            self.caret += 1;
        }
    }
    pub fn delete_back(&mut self) {
        if self.select_all {
            self.chars.clear();
            self.caret = 0;
            self.select_all = false;
            return;
        }
        if self.caret > 0 {
            self.caret -= 1;
            self.chars.remove(self.caret);
        }
    }
    pub fn delete_forward(&mut self) {
        if self.select_all {
            self.chars.clear();
            self.caret = 0;
            self.select_all = false;
            return;
        }
        if self.caret < self.chars.len() {
            self.chars.remove(self.caret);
        }
    }
    pub fn move_caret(&mut self, delta: isize) {
        self.select_all = false;
        let next = self.caret as isize + delta;
        self.caret = next.clamp(0, self.chars.len() as isize) as usize;
    }
    pub fn caret_home(&mut self) {
        self.select_all = false;
        self.caret = 0;
    }
    pub fn caret_end(&mut self) {
        self.select_all = false;
        self.caret = self.chars.len();
    }
}

// ---------------------------------------------------------------------------
// tab geometry + chrome hit-testing (pure, testable)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLook {
    pub title: String,
    pub active: bool,
}

/// (tab width, first tab x). Tabs shrink to fit, with a floor.
pub fn tab_metrics(w: u32, count: usize) -> (f32, f32) {
    let avail = (w as f32 - 44.0).max(64.0);
    let n = count.max(1) as f32;
    let tab_w = (avail / n).clamp(64.0, 180.0);
    (tab_w, 4.0)
}

pub fn tab_range(i: usize, w: u32, count: usize) -> (f32, f32) {
    let (tab_w, x0) = tab_metrics(w, count);
    let x = x0 + i as f32 * (tab_w + 2.0);
    (x, x + tab_w)
}

/// X extent of the "+" (new tab) button.
pub fn new_tab_range(w: u32, count: usize) -> (f32, f32) {
    let (_, x) = tab_range(count, w, count);
    (x + 4.0, x + 28.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeHit {
    Tab(usize),
    TabClose(usize),
    NewTab,
    Address,
    Page,
    Strip,
    Status,
}

/// Which chrome region a window-space point falls in.
pub fn hit_chrome(x: f32, y: f32, w: u32, h: u32, tabs: usize) -> ChromeHit {
    if y < 0.0 {
        return ChromeHit::Strip;
    }
    let yu = y as u32;
    if yu < TAB_H {
        let (nx0, nx1) = new_tab_range(w, tabs);
        if x >= nx0 && x <= nx1 {
            return ChromeHit::NewTab;
        }
        for i in 0..tabs {
            let (x0, x1) = tab_range(i, w, tabs);
            if x >= x0 && x < x1 {
                return if x >= x1 - 20.0 {
                    ChromeHit::TabClose(i)
                } else {
                    ChromeHit::Tab(i)
                };
            }
        }
        return ChromeHit::Strip;
    }
    if yu < TAB_H + TOOLBAR_H {
        return ChromeHit::Address;
    }
    if yu >= h.saturating_sub(STATUS_H) {
        return ChromeHit::Status;
    }
    ChromeHit::Page
}

// ---------------------------------------------------------------------------
// frame composition (pure, testable)
// ---------------------------------------------------------------------------

/// Everything the chrome needs to draw itself for one frame.
#[derive(Default, Clone)]
pub struct Chrome {
    pub url_text: String,
    pub focused: bool,
    pub select_all: bool,
    pub caret: Option<usize>,
    pub caret_on: bool,
    pub tabs: Vec<TabLook>,
    pub status: String,
    pub note: String,
}

impl Chrome {
    /// Unfocused chrome showing a view's own URL, one tab.
    pub fn plain(view: &PageView) -> Chrome {
        Chrome {
            url_text: view.url.clone(),
            tabs: vec![TabLook {
                title: view.title.clone(),
                active: true,
            }],
            status: format!("{} links", view.links.len()),
            ..Default::default()
        }
    }
}

/// One composited window frame in straight RGBA.
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    /// Convert to softbuffer's `0x00RRGGBB` u32 pixel format.
    pub fn to_u32(&self) -> Vec<u32> {
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | (p[2] as u32))
            .collect()
    }
}

fn fill_rect(buf: &mut [u8], w: u32, x0: u32, y0: u32, x1: u32, y1: u32, c: (u8, u8, u8)) {
    let w = w as usize;
    for y in (y0 as usize)..(y1 as usize).min(buf.len() / 4 / w) {
        let row = y * w * 4;
        for px in buf[row + (x0 as usize) * 4..row + (x1 as usize).min(w) * 4]
            .as_chunks_mut::<4>()
            .0
        {
            px[0] = c.0;
            px[1] = c.1;
            px[2] = c.2;
            px[3] = 255;
        }
    }
}

fn fill_row(buf: &mut [u8], w: u32, y: u32, c: (u8, u8, u8)) {
    fill_rect(buf, w, 0, y, w, y + 1, c);
}

/// Where and how a run of chrome text is drawn.
struct TextPlacement {
    dims: (u32, u32),
    x: f32,
    baseline: f32,
    size: f32,
    color: (u8, u8, u8),
}

fn draw_text(buf: &mut [u8], s: &str, at: TextPlacement) -> f32 {
    let db = fonts();
    let style = BlitStyle {
        size: at.size,
        fam: Family::Sans,
        bold: false,
        rgb: at.color,
        alpha: 255,
    };
    let mut pen = at.x;
    for ch in s.chars() {
        if pen > at.dims.0 as f32 {
            break;
        }
        pen += db.blit_char(
            buf,
            (at.dims.0 as usize, at.dims.1 as usize),
            ch,
            (pen, at.baseline),
            &style,
        );
    }
    pen
}

fn truncate_to(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

/// Compose one window frame: tab strip, URL bar, page slice, status bar.
pub fn compose(view: Option<&PageView>, scroll: i32, w: u32, h: u32, chrome: &Chrome) -> Frame {
    let w = w.max(240);
    let h = h.max(page_top() + STATUS_H + 8);
    let view_h = page_view_height(h);
    let top = page_top();
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];

    // ---- tab strip ----
    for y in 0..TAB_H {
        fill_row(&mut rgba, w, y, BG_TABS);
    }
    let count = chrome.tabs.len();
    for (i, tab) in chrome.tabs.iter().enumerate() {
        let (x0, x1) = tab_range(i, w, count);
        let bg = if tab.active { BG_TAB_ACTIVE } else { BG_TAB };
        fill_rect(&mut rgba, w, x0 as u32, 4, x1 as u32, TAB_H, bg);
        let label = truncate_to(&tab.title, (((x1 - x0) - 30.0) / 6.0).max(2.0) as usize);
        draw_text(
            &mut rgba,
            &label,
            TextPlacement {
                dims: (w, h),
                x: x0 + 8.0,
                baseline: (TAB_H - 8) as f32,
                size: 11.0,
                color: if tab.active { FG_BRIGHT } else { FG_DIM },
            },
        );
        draw_text(
            &mut rgba,
            "×",
            TextPlacement {
                dims: (w, h),
                x: x1 - 16.0,
                baseline: (TAB_H - 8) as f32,
                size: 11.0,
                color: FG_DIM,
            },
        );
    }
    let (nx0, nx1) = new_tab_range(w, count);
    draw_text(
        &mut rgba,
        "+",
        TextPlacement {
            dims: (w, h),
            x: nx0 + (nx1 - nx0) / 2.0 - 3.0,
            baseline: (TAB_H - 8) as f32,
            size: 13.0,
            color: FG_DIM,
        },
    );

    // ---- URL bar ----
    for y in TAB_H..top {
        fill_row(&mut rgba, w, y, BG_TOOLBAR);
    }
    let field_y0 = TAB_H + 5;
    let field_y1 = top - 5;
    fill_rect(
        &mut rgba,
        w,
        FIELD_X as u32,
        field_y0,
        w - FIELD_X as u32,
        field_y1,
        BG_FIELD,
    );
    if chrome.focused {
        // 1px accent outline around the field
        fill_rect(
            &mut rgba,
            w,
            FIELD_X as u32,
            field_y0,
            w - FIELD_X as u32,
            field_y0 + 1,
            ACCENT,
        );
        fill_rect(
            &mut rgba,
            w,
            FIELD_X as u32,
            field_y1 - 1,
            w - FIELD_X as u32,
            field_y1,
            ACCENT,
        );
        fill_rect(
            &mut rgba,
            w,
            FIELD_X as u32,
            field_y0,
            FIELD_X as u32 + 1,
            field_y1,
            ACCENT,
        );
        fill_rect(
            &mut rgba,
            w,
            w - FIELD_X as u32 - 1,
            field_y0,
            w - FIELD_X as u32,
            field_y1,
            ACCENT,
        );
    }
    let shown = truncate_to(
        &chrome.url_text,
        ((w as f32 - 40.0) / 7.0).max(8.0) as usize,
    );
    if chrome.focused && chrome.select_all {
        let tw = fonts().measure(&shown, URL_FONT, Family::Sans, false);
        fill_rect(
            &mut rgba,
            w,
            (TEXT_X - 2.0) as u32,
            field_y0 + 3,
            (TEXT_X + tw + 2.0).min(w as f32 - FIELD_X - 2.0) as u32,
            field_y1 - 3,
            SEL,
        );
    }
    draw_text(
        &mut rgba,
        &shown,
        TextPlacement {
            dims: (w, h),
            x: TEXT_X,
            baseline: (field_y1 - 8) as f32,
            size: URL_FONT,
            color: FG_BRIGHT,
        },
    );
    if chrome.focused
        && chrome.caret_on
        && let Some(caret) = chrome.caret
    {
        let prefix: String = chrome.url_text.chars().take(caret).collect();
        let cx = TEXT_X + fonts().measure(&prefix, URL_FONT, Family::Sans, false);
        fill_rect(
            &mut rgba,
            w,
            cx as u32,
            field_y0 + 4,
            cx as u32 + 1,
            field_y1 - 4,
            FG_BRIGHT,
        );
    }

    // ---- page slice ----
    match view {
        Some(v) => {
            let src = v.pixmap.data();
            let src_w = v.page_width() as usize;
            let src_h = v.page_height() as i64;
            for row in 0..view_h {
                let src_y = scroll as i64 + row as i64;
                let dst = ((top + row) as usize) * (w as usize) * 4;
                if src_y >= 0 && src_y < src_h {
                    let s = (src_y as usize) * src_w * 4;
                    let copy = (src_w.min(w as usize)) * 4;
                    rgba[dst..dst + copy].copy_from_slice(&src[s..s + copy]);
                    for px in rgba[dst + copy..dst + (w as usize) * 4]
                        .as_chunks_mut::<4>()
                        .0
                    {
                        *px = [PAGE_FALLBACK.0, PAGE_FALLBACK.1, PAGE_FALLBACK.2, 255];
                    }
                } else {
                    for px in rgba[dst..dst + (w as usize) * 4].as_chunks_mut::<4>().0 {
                        *px = [PAGE_FALLBACK.0, PAGE_FALLBACK.1, PAGE_FALLBACK.2, 255];
                    }
                }
            }
        }
        None => {
            for row in 0..view_h {
                fill_row(&mut rgba, w, top + row, PAGE_FALLBACK);
            }
        }
    }

    // ---- status bar ----
    for y in (top + view_h)..h {
        fill_row(&mut rgba, w, y, BG_STATUS);
    }
    draw_text(
        &mut rgba,
        &chrome.status,
        TextPlacement {
            dims: (w, h),
            x: 10.0,
            baseline: (h - 7) as f32,
            size: 11.0,
            color: FG_DIM,
        },
    );
    if !chrome.note.is_empty() {
        let note = truncate_to(&chrome.note, ((w as f32 / 2.0) / 6.0).max(8.0) as usize);
        let nw = fonts().measure(&note, 11.0, Family::Sans, false);
        draw_text(
            &mut rgba,
            &note,
            TextPlacement {
                dims: (w, h),
                x: (w as f32 - nw - 10.0).max(10.0),
                baseline: (h - 7) as f32,
                size: 11.0,
                color: ERROR,
            },
        );
    }

    Frame { w, h, rgba }
}

/// Encode a composed frame as PNG (used by `shelltest`).
pub fn frame_to_png(f: &Frame) -> Option<Vec<u8>> {
    let pm = tiny_skia::Pixmap::from_vec(f.rgba.clone(), tiny_skia::IntSize::from_wh(f.w, f.h)?)?;
    pm.encode_png().ok()
}

// ---------------------------------------------------------------------------
// the winit application
// ---------------------------------------------------------------------------

struct Tab {
    view: Option<PageView>,
    history: History,
    scroll: i32,
}

impl Tab {
    fn title(&self) -> String {
        self.view
            .as_ref()
            .map(|v| v.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "new tab".to_string())
    }
    fn url(&self) -> String {
        self.view
            .as_ref()
            .map(|v| v.url.clone())
            .unwrap_or_default()
    }
}

/// True when `href` points at YouTube (the main site, a subdomain like
/// `m.`/`music.`, or a `youtu.be` short link). Host-based only, so look-alikes
/// such as `notyoutube.com` or `youtube.com.evil.tld` do not match.
fn is_youtube(href: &str) -> bool {
    url::Url::parse(href)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_ascii_lowercase()))
        .map(|h| h == "youtu.be" || h == "youtube.com" || h.ends_with(".youtube.com"))
        .unwrap_or(false)
}

struct App {
    window: Option<Arc<Window>>,
    context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    surface_size: Option<(u32, u32)>,
    tabs: Vec<Tab>,
    active: usize,
    address: AddressBar,
    size: (u32, u32),
    images: bool,
    initial: Option<String>,
    mods: winit::keyboard::ModifiersState,
    note: String,
    last_pointer: (Option<f32>, Option<f32>),
    last_title: String,
    caret_on: bool,
    last_blink: Instant,
    quit: bool,
}

impl App {
    fn new(initial: Option<String>, images: bool) -> App {
        App {
            window: None,
            context: None,
            surface: None,
            surface_size: None,
            tabs: Vec::new(),
            active: 0,
            address: AddressBar::default(),
            size: (1180, 820),
            images,
            initial,
            mods: winit::keyboard::ModifiersState::empty(),
            note: String::new(),
            last_pointer: (None, None),
            last_title: String::new(),
            caret_on: true,
            last_blink: Instant::now(),
            quit: false,
        }
    }

    fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }
    fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn view_h(&self) -> u32 {
        page_view_height(self.size.1)
    }

    /// softbuffer (Wayland in particular) requires the surface to be sized
    /// before the first `buffer_mut()`, and the first `RedrawRequested` can
    /// arrive before any `Resized` event — so the surface size is owned here
    /// and synced before every present instead of only on resize events.
    fn resize_surface(&mut self) {
        let (w, h) = self.size;
        let (Some(w), Some(h)) = (NonZeroU32::new(w), NonZeroU32::new(h)) else {
            return;
        };
        if self.surface_size == Some((w.get(), h.get())) {
            return;
        }
        if let Some(surface) = self.surface.as_mut()
            && surface.resize(w, h).is_ok()
        {
            self.surface_size = Some((w.get(), h.get()));
        }
    }

    fn sync_address_from_tab(&mut self) {
        let url = self.tab().map(|t| t.url()).unwrap_or_default();
        self.address.set_text(&url);
        self.address.blur();
    }

    fn open_tab(&mut self, url: Option<&str>) {
        let w = self.size.0.max(320);
        let view = match url {
            Some(u) => PageView::load(u, w, self.images).ok(),
            None => PageView::from_html(&start_page_html(), w, self.images).ok(),
        };
        let mut tab = Tab {
            view: None,
            history: History::default(),
            scroll: 0,
        };
        if let Some(v) = view {
            tab.history.push(&v.url);
            tab.view = Some(v);
        }
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.sync_address_from_tab();
    }

    fn close_tab(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.tabs.remove(i);
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            } else if i < self.active {
                self.active -= 1;
            }
            if self.tabs.is_empty() {
                self.quit = true;
            } else {
                self.sync_address_from_tab();
            }
        }
    }

    fn navigate(&mut self, url: &str, push: bool) {
        // Ferris can't decode video, so hand YouTube links to an external
        // player (mpv, which resolves the stream via yt-dlp) instead of
        // loading YouTube's script-heavy page — which would render as a shell.
        if is_youtube(url) {
            self.note = match std::process::Command::new("mpv").arg(url).spawn() {
                Ok(_) => format!("opened in mpv: {url}"),
                Err(e) => format!("could not start mpv: {e} (install mpv + yt-dlp)"),
            };
            if let Some(win) = &self.window {
                win.request_redraw();
            }
            return;
        }
        self.note = format!("loading {url} …");
        // Paint the "loading" note before blocking on the network, so the
        // window never looks frozen.
        self.present();
        let w = self.size.0.max(320);
        match PageView::load(url, w, self.images) {
            Ok(v) => {
                if let Some(t) = self.tab_mut() {
                    if push {
                        t.history.push(&v.url);
                    }
                    t.scroll = 0;
                    t.view = Some(v);
                }
                self.note.clear();
                self.sync_address_from_tab();
            }
            Err(e) => {
                self.note = format!("navigation failed: {e}");
            }
        }
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let view_h = self.view_h();
        if let Some(t) = self.tab_mut() {
            let ph = t.view.as_ref().map(|v| v.page_height()).unwrap_or(0);
            t.scroll = clamp_scroll(t.scroll + delta, ph, view_h);
        }
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    fn set_scroll(&mut self, value: i32) {
        let view_h = self.view_h();
        if let Some(t) = self.tab_mut() {
            let ph = t.view.as_ref().map(|v| v.page_height()).unwrap_or(0);
            t.scroll = clamp_scroll(value, ph, view_h);
        }
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    fn click_at(&mut self, x: f32, y: f32) {
        match hit_chrome(x, y, self.size.0, self.size.1, self.tabs.len()) {
            ChromeHit::Tab(i) => {
                self.active = i;
                self.sync_address_from_tab();
            }
            ChromeHit::TabClose(i) => self.close_tab(i),
            ChromeHit::NewTab => self.open_tab(None),
            ChromeHit::Address => {
                self.address.focus(true);
                self.caret_on = true;
                self.last_blink = Instant::now();
            }
            ChromeHit::Status | ChromeHit::Strip => {}
            ChromeHit::Page => {
                self.address.blur();
                let y_page =
                    y - page_top() as f32 + self.tab().map(|t| t.scroll).unwrap_or(0) as f32;
                let href = self
                    .tab()
                    .and_then(|t| t.view.as_ref())
                    .and_then(|v| v.hit_test(x, y_page).map(|l| l.href.clone()));
                if let Some(href) = href {
                    self.navigate(&href, true);
                }
            }
        }
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    fn chrome_state(&self) -> Chrome {
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| TabLook {
                title: t.title(),
                active: i == self.active,
            })
            .collect();
        let (status, url_text, focused, select_all, caret) = if self.address.focused {
            (
                "type an address · Enter to go · Esc to cancel".to_string(),
                self.address.text(),
                true,
                self.address.selected_all(),
                Some(self.address.caret()),
            )
        } else {
            let t = self.tab();
            let ph = t
                .and_then(|t| t.view.as_ref())
                .map(|v| v.page_height())
                .unwrap_or(0);
            let scroll = t.map(|t| t.scroll).unwrap_or(0);
            let links = t
                .and_then(|t| t.view.as_ref())
                .map(|v| v.links.len())
                .unwrap_or(0);
            (
                format!(
                    "{}   ·   y {scroll}/{ph}   ·   {links} links   ·   Ctrl+L address · Ctrl+T tab",
                    t.map(|t| t.title()).unwrap_or_default()
                ),
                t.map(|t| t.url()).unwrap_or_default(),
                false,
                false,
                None,
            )
        };
        Chrome {
            url_text,
            focused,
            select_all,
            caret,
            caret_on: self.caret_on,
            tabs,
            status,
            note: self.note.clone(),
        }
    }

    fn present(&mut self) {
        self.resize_surface();
        let title = self
            .tab()
            .map(|t| t.title())
            .unwrap_or_else(|| "Ferris".to_string());
        if title != self.last_title {
            if let Some(win) = &self.window {
                win.set_title(&format!("{title} — Ferris"));
            }
            self.last_title = title;
        }
        let (w, h) = self.size;
        let chrome = self.chrome_state();
        let scroll = self.tab().map(|t| t.scroll).unwrap_or(0);
        let frame = compose(
            self.tab().and_then(|t| t.view.as_ref()),
            scroll,
            w,
            h,
            &chrome,
        );
        let pixels = frame.to_u32();
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        if let Ok(mut buffer) = surface.buffer_mut()
            && buffer.len() == pixels.len()
        {
            buffer.copy_from_slice(&pixels);
            let _ = buffer.present();
        }
    }

    fn commit_address(&mut self) {
        let text = self.address.text();
        self.address.blur();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.sync_address_from_tab();
            return;
        }
        match crate::net::normalize(trimmed) {
            Ok(url) => self.navigate(url.as_str(), true),
            Err(e) => {
                self.note = format!("bad address: {e}");
                self.sync_address_from_tab();
            }
        }
    }

    fn key_page(&mut self, event_loop: &ActiveEventLoop, key: &Key) {
        let view_h = self.view_h() as i32;
        match key {
            Key::Named(NamedKey::ArrowDown) => self.scroll_by(48),
            Key::Named(NamedKey::ArrowUp) => self.scroll_by(-48),
            Key::Named(NamedKey::PageDown) => self.scroll_by(view_h - 40),
            Key::Named(NamedKey::PageUp) => self.scroll_by(-(view_h - 40)),
            Key::Named(NamedKey::Home) => self.set_scroll(0),
            Key::Named(NamedKey::End) => self.set_scroll(i32::MAX),
            Key::Named(NamedKey::Backspace) | Key::Named(NamedKey::ArrowLeft)
                if self.mods.alt_key() =>
            {
                let t = self
                    .tab_mut()
                    .and_then(|t| t.history.back().map(str::to_string));
                if let Some(u) = t {
                    self.navigate(&u, false);
                }
            }
            Key::Named(NamedKey::ArrowRight) if self.mods.alt_key() => {
                let t = self
                    .tab_mut()
                    .and_then(|t| t.history.forward().map(str::to_string));
                if let Some(u) = t {
                    self.navigate(&u, false);
                }
            }
            Key::Named(NamedKey::F5) => {
                let u = self
                    .tab()
                    .and_then(|t| t.history.current().map(str::to_string));
                if let Some(u) = u {
                    self.navigate(&u, false);
                }
            }
            Key::Named(NamedKey::Escape) => event_loop.exit(),
            Key::Character(c) if c == "q" && self.mods.control_key() => event_loop.exit(),
            Key::Character(c) if c == " " => self.scroll_by(view_h - 40),
            Key::Character(c) if c == "l" && self.mods.control_key() => {
                self.address.focus(true);
                self.caret_on = true;
            }
            Key::Named(NamedKey::F6) => {
                self.address.focus(true);
                self.caret_on = true;
            }
            Key::Character(c) if c == "t" && self.mods.control_key() => self.open_tab(None),
            Key::Character(c) if c == "w" && self.mods.control_key() => {
                let i = self.active;
                self.close_tab(i);
            }
            Key::Named(NamedKey::Tab) if self.mods.control_key() && !self.tabs.is_empty() => {
                let dir = if self.mods.shift_key() { -1isize } else { 1 };
                let n = self.tabs.len() as isize;
                self.active = ((self.active as isize + dir).rem_euclid(n)) as usize;
                self.sync_address_from_tab();
            }
            _ => {}
        }
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    fn key_address(&mut self, event_loop: &ActiveEventLoop, key: &Key, repeat: bool) {
        if repeat
            && matches!(
                key,
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Escape)
            )
        {
            return;
        }
        match key {
            Key::Named(NamedKey::Enter) => self.commit_address(),
            Key::Named(NamedKey::Escape) => self.sync_address_from_tab(),
            Key::Named(NamedKey::Backspace) => self.address.delete_back(),
            Key::Named(NamedKey::Delete) => self.address.delete_forward(),
            Key::Named(NamedKey::ArrowLeft) => self.address.move_caret(-1),
            Key::Named(NamedKey::ArrowRight) => self.address.move_caret(1),
            Key::Named(NamedKey::Home) => self.address.caret_home(),
            Key::Named(NamedKey::End) => self.address.caret_end(),
            Key::Character(c) if c == "a" && self.mods.control_key() => self.address.select_all(),
            Key::Character(c) if !self.mods.control_key() && !self.mods.alt_key() => {
                self.address.insert(c)
            }
            Key::Named(NamedKey::Tab) => self.address.blur(),
            _ => {}
        }
        self.caret_on = true;
        self.last_blink = Instant::now();
        if let Some(win) = &self.window {
            win.request_redraw();
        }
        let _ = event_loop;
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_inner_size(winit::dpi::PhysicalSize::new(self.size.0, self.size.1))
            .with_title("Ferris — a browser engine in Rust")
            .with_window_icon(window_icon())
            .with_min_inner_size(winit::dpi::PhysicalSize::new(480u32, 360u32));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("window creation failed: {e}");
                return;
            }
        };
        match softbuffer::Context::new(window.clone()) {
            Ok(ctx) => match softbuffer::Surface::new(&ctx, window.clone()) {
                Ok(surface) => {
                    self.context = Some(ctx);
                    self.surface = Some(surface);
                }
                Err(e) => eprintln!("softbuffer surface: {e}"),
            },
            Err(e) => eprintln!("softbuffer context: {e}"),
        }
        self.window = Some(window);
        if let Some(win) = &self.window {
            let inner = win.inner_size();
            if inner.width >= 240 && inner.height >= 120 {
                self.size = (inner.width, inner.height);
            }
        }
        self.resize_surface();

        match self.initial.take() {
            Some(url) => {
                self.open_tab(None);
                self.navigate(&url, true);
            }
            None => self.open_tab(None),
        }
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                self.size = (size.width.max(240), size.height.max(120));
                let view_h = self.view_h();
                if let Some(t) = self.tab_mut() {
                    let ph = t.view.as_ref().map(|v| v.page_height()).unwrap_or(0);
                    t.scroll = clamp_scroll(t.scroll, ph, view_h);
                }
                if let Some(win) = &self.window {
                    win.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => self.present(),

            WindowEvent::MouseWheel { delta, .. } => {
                let d = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => lines * 24.0,
                    MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
                };
                self.scroll_by(-d as i32);
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.last_pointer = (Some(position.x as f32), Some(position.y as f32));
                let hit = hit_chrome(
                    position.x as f32,
                    position.y as f32,
                    self.size.0,
                    self.size.1,
                    self.tabs.len(),
                );
                let over_link = matches!(hit, ChromeHit::Page)
                    && self.tab().and_then(|t| t.view.as_ref()).is_some_and(|v| {
                        v.hit_test(
                            position.x as f32,
                            position.y as f32 - page_top() as f32
                                + self.tab().map(|t| t.scroll).unwrap_or(0) as f32,
                        )
                        .is_some()
                    });
                let icon = match hit {
                    ChromeHit::Address
                    | ChromeHit::Tab(_)
                    | ChromeHit::TabClose(_)
                    | ChromeHit::NewTab => CursorIcon::Pointer,
                    ChromeHit::Page if over_link => CursorIcon::Pointer,
                    _ => CursorIcon::Default,
                };
                if let Some(win) = &self.window {
                    win.set_cursor(Cursor::from(icon));
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let (Some(x), Some(y)) = self.last_pointer {
                    self.click_at(x, y);
                }
            }

            WindowEvent::ModifiersChanged(mods) => self.mods = mods.state(),

            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        repeat,
                        ..
                    },
                ..
            } => {
                if repeat && !self.address.focused {
                    return;
                }
                if self.address.focused {
                    self.key_address(event_loop, &logical_key, repeat);
                } else {
                    self.key_page(event_loop, &logical_key);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.quit {
            event_loop.exit();
            return;
        }
        // Caret blink while the address bar has focus.
        if self.address.focused && self.last_blink.elapsed() >= Duration::from_millis(500) {
            self.caret_on = !self.caret_on;
            self.last_blink = Instant::now();
            if let Some(win) = &self.window {
                win.request_redraw();
            }
        }
    }
}

/// The application icon, embedded in the binary and handed to the window
/// manager so taskbars/switchers show the crab instead of a placeholder.
fn window_icon() -> Option<winit::window::Icon> {
    let img = image::load_from_memory(include_bytes!("../packaging/icon-256.png")).ok()?;
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    winit::window::Icon::from_rgba(rgba.into_raw(), w, h).ok()
}

/// Open the native window. Returns an error (with guidance) when there is no
/// display server, e.g. inside a headless sandbox.
pub fn run(initial: Option<String>, images: bool) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| {
        format!(
            "no display server available ({e}).\n\
             Run this on a desktop session (Wayland or X11), or verify the \
             interactive logic headlessly with:\n    ferris shelltest <url>"
        )
    })?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App::new(initial, images);
    event_loop
        .run_app(&mut app)
        .map_err(|e| format!("event loop: {e}"))
}

// ---------------------------------------------------------------------------
// headless self-test
// ---------------------------------------------------------------------------

/// Render a page and exercise the interactive logic with no display server,
/// optionally writing the exact frames the window would present.
pub fn selftest(
    url: &str,
    extra_urls: &[String],
    width: u32,
    height: u32,
    out_png: Option<&str>,
) -> Result<String, String> {
    let view = PageView::load(url, width, true)?;
    let view_h = page_view_height(height);
    let mut report = String::new();
    report.push_str(&format!(
        "page      : {} ({})\ncanvas    : {}x{}\nlinks     : {}\n",
        view.url,
        view.title,
        view.page_width(),
        view.page_height(),
        view.links.len()
    ));

    // 1. composition with tabs + URL bar
    let chrome = Chrome::plain(&view);
    let f0 = compose(Some(&view), 0, width, height, &chrome);
    assert_eq!(f0.rgba.len(), (width as usize) * (height as usize) * 4);
    assert_eq!(f0.to_u32().len(), (width * height) as usize);
    report.push_str(&format!(
        "frame     : {}x{} composed (tabs+urlbar+status)\n",
        f0.w, f0.h
    ));

    // 2. focused address bar with a caret renders differently
    let mut focused = chrome.clone();
    focused.focused = true;
    focused.caret_on = true;
    focused.url_text = "rust-lang".to_string();
    focused.caret = Some(9);
    let f_focus = compose(Some(&view), 0, width, height, &focused);
    assert_ne!(f0.rgba, f_focus.rgba, "focused bar must change the frame");
    let mut blink = focused.clone();
    blink.caret_on = false;
    let f_blink = compose(Some(&view), 0, width, height, &blink);
    assert_ne!(
        f_focus.rgba, f_blink.rgba,
        "caret blink must change the frame"
    );
    report.push_str("address   : focus + caret + blink compose OK\n");

    // 3. scrolling changes the visible slice, and clamps (a page that fits
    //    the viewport has nothing to scroll, which is correct, not a failure)
    let max = (view.page_height() as i32 - view_h as i32).max(0);
    if max > 0 {
        let mid = clamp_scroll(view.page_height() as i32 / 2, view.page_height(), view_h);
        let f1 = compose(Some(&view), mid, width, height, &chrome);
        assert_ne!(f0.rgba, f1.rgba);
        report.push_str(&format!(
            "scroll    : 0 -> {mid} changes frame; clamp [0, {max}] OK\n"
        ));
    } else {
        report.push_str(&format!(
            "scroll    : page fits the viewport; clamp [0, {max}] OK\n"
        ));
    }
    assert_eq!(clamp_scroll(-500, view.page_height(), view_h), 0);
    assert_eq!(clamp_scroll(i32::MAX, view.page_height(), view_h), max);

    // 4. hit-testing resolves a link target
    if let Some(l) = view.links.first() {
        let hit = view.hit_test(l.x + l.w / 2.0, l.y + l.h / 2.0);
        assert_eq!(hit.map(|h| &h.href), Some(&l.href));
        report.push_str(&format!("hit-test  : ({}, {}) -> {}\n", l.x, l.y, l.href));
    } else {
        report.push_str("hit-test  : (page has no links)\n");
    }

    // 5. chrome region hit-testing
    assert_eq!(hit_chrome(10.0, 5.0, width, height, 2), ChromeHit::Tab(0));
    let (x0, x1) = tab_range(0, width, 2);
    assert_eq!(
        hit_chrome(x1 - 5.0, 5.0, width, height, 2),
        ChromeHit::TabClose(0)
    );
    let _ = x0;
    let (nx0, nx1) = new_tab_range(width, 2);
    assert_eq!(
        hit_chrome((nx0 + nx1) / 2.0, 5.0, width, height, 2),
        ChromeHit::NewTab
    );
    assert_eq!(
        hit_chrome(width as f32 / 2.0, TAB_H as f32 + 5.0, width, height, 2),
        ChromeHit::Address
    );
    assert_eq!(
        hit_chrome(width as f32 / 2.0, height as f32 - 5.0, width, height, 2),
        ChromeHit::Status
    );
    assert_eq!(
        hit_chrome(
            width as f32 / 2.0,
            page_top() as f32 + 40.0,
            width,
            height,
            2
        ),
        ChromeHit::Page
    );
    report.push_str("chrome    : tab/close/+/address/status/page regions OK\n");

    // 6. history + address editing semantics
    let mut h = History::default();
    h.push("https://a.test/");
    h.push("https://b.test/");
    assert_eq!(h.back(), Some("https://a.test/"));
    assert_eq!(h.forward(), Some("https://b.test/"));
    h.push("https://c.test/");
    assert!(!h.can_forward());
    let mut ab = AddressBar::default();
    ab.set_text("example.com");
    ab.focus(true);
    ab.insert("rust-lang.org");
    assert_eq!(ab.text(), "rust-lang.org", "select-all typing replaces");
    ab.caret_home();
    ab.insert("https://");
    assert_eq!(ab.text(), "https://rust-lang.org");
    ab.caret_end();
    for _ in 0..4 {
        ab.delete_back();
    }
    assert_eq!(ab.text(), "https://rust-lang");
    ab.caret_home();
    ab.move_caret(4);
    ab.delete_forward();
    assert_eq!(
        ab.text(),
        "http://rust-lang",
        "delete_forward removes the char at the caret"
    );
    report.push_str("editing   : select-all/insert/caret/delete OK\n");

    if let Some(path) = out_png {
        let png = frame_to_png(&f0).ok_or("png encode failed")?;
        std::fs::write(path, png).map_err(|e| format!("write {path}: {e}"))?;
        report.push_str(&format!("frame png : wrote {path}\n"));
        let focus_path = format!("{}-focus.png", path.trim_end_matches(".png"));
        let png2 = frame_to_png(&f_focus).ok_or("png encode failed")?;
        std::fs::write(&focus_path, png2).map_err(|e| format!("write {focus_path}: {e}"))?;
        report.push_str(&format!("frame png : wrote {focus_path}\n"));

        // A real multi-tab frame: every extra URL is fetched and rendered by
        // the engine, then shown as its own tab next to the primary one.
        if !extra_urls.is_empty() {
            let mut tabs = vec![TabLook {
                title: view.title.clone(),
                active: true,
            }];
            for u in extra_urls {
                match PageView::load(u, width, true) {
                    Ok(v) => tabs.push(TabLook {
                        title: v.title.clone(),
                        active: false,
                    }),
                    Err(e) => report.push_str(&format!("tab       : {u} failed ({e})\n")),
                }
            }
            let multi = Chrome {
                tabs,
                ..chrome.clone()
            };
            let fm = compose(Some(&view), 0, width, height, &multi);
            let tabs_path = format!("{}-tabs.png", path.trim_end_matches(".png"));
            let png3 = frame_to_png(&fm).ok_or("png encode failed")?;
            std::fs::write(&tabs_path, png3).map_err(|e| format!("write {tabs_path}: {e}"))?;
            report.push_str(&format!(
                "tabs      : {} tabs composed, wrote {tabs_path}\n",
                multi.tabs.len()
            ));
        }
    }

    report.push_str("selftest  : PASS\n");
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_view() -> PageView {
        let html = r#"<html><head><title>T</title></head><body>
            <h1>Head</h1><p>Some paragraph text here.</p>
            <a href="/next">a link</a></body></html>"#;
        let r = engine::render_html(html, "https://t.test/", 600, false);
        PageView::from_render(r).expect("pixels")
    }

    #[test]
    fn clamp_scroll_bounds() {
        assert_eq!(clamp_scroll(-1, 1000, 400), 0);
        assert_eq!(clamp_scroll(50, 1000, 400), 50);
        assert_eq!(clamp_scroll(9999, 1000, 400), 600);
        assert_eq!(clamp_scroll(10, 100, 400), 0);
    }

    #[test]
    fn history_truncates_forward() {
        let mut h = History::default();
        h.push("a");
        h.push("b");
        h.push("c");
        h.back();
        h.back();
        h.push("x");
        assert_eq!(h.len(), 2);
        assert_eq!(h.current(), Some("x"));
        assert!(!h.can_forward());
        assert!(h.can_back());
        assert!(!h.is_empty());
    }

    #[test]
    fn address_bar_editing() {
        let mut ab = AddressBar::default();
        ab.set_text("hello");
        assert_eq!(ab.caret(), 5);
        assert_eq!(ab.prefix(), "hello");
        ab.move_caret(-5);
        ab.insert("XY");
        assert_eq!(ab.text(), "XYhello");
        assert_eq!(ab.caret(), 2);
        ab.delete_back();
        assert_eq!(ab.text(), "Xhello");
        ab.caret_end();
        ab.delete_back();
        assert_eq!(ab.text(), "Xhell");
        ab.select_all();
        ab.insert("z");
        assert_eq!(ab.text(), "z", "typing over a selection replaces it");
        ab.caret_home();
        ab.delete_forward();
        assert_eq!(ab.text(), "");
    }

    #[test]
    fn tab_geometry_fits_and_floors() {
        let (w1, _) = tab_metrics(1000, 1);
        assert_eq!(w1, 180.0, "single tab uses the ideal width");
        let (w20, x0) = tab_metrics(1000, 20);
        assert!(
            w20 >= 64.0 - f32::EPSILON,
            "tabs never shrink below the floor"
        );
        assert_eq!(x0, 4.0);
        let (a0, a1) = tab_range(0, 1000, 3);
        let (b0, _b1) = tab_range(1, 1000, 3);
        assert!(a0 < a1 && b0 >= a1, "tabs must not overlap");
    }

    #[test]
    fn chrome_regions_are_disjoint() {
        let (w, h) = (900u32, 700u32);
        for y in [1u32, TAB_H + 2, page_top() + 5, h - 3] {
            let hit = hit_chrome(20.0, y as f32, w, h, 2);
            match y {
                1 => assert_eq!(hit, ChromeHit::Tab(0)),
                y if y < page_top() => assert_eq!(hit, ChromeHit::Address),
                y if y >= h - STATUS_H => assert_eq!(hit, ChromeHit::Status),
                _ => assert_eq!(hit, ChromeHit::Page),
            }
        }
    }

    #[test]
    fn compose_dimensions_and_chrome_pixels() {
        let v = test_view();
        let f = compose(Some(&v), 0, 500, 400, &Chrome::plain(&v));
        assert_eq!(f.rgba.len(), 500 * 400 * 4);
        assert_eq!(&f.rgba[0..3], &[BG_TABS.0, BG_TABS.1, BG_TABS.2]);
        let toolbar_row = ((TAB_H + 2) as usize) * 500 * 4;
        assert_eq!(
            &f.rgba[toolbar_row..toolbar_row + 3],
            &[BG_TOOLBAR.0, BG_TOOLBAR.1, BG_TOOLBAR.2]
        );
        let last = f.rgba.len() - 4;
        assert_eq!(
            &f.rgba[last..last + 3],
            &[BG_STATUS.0, BG_STATUS.1, BG_STATUS.2]
        );
        assert!(f.to_u32().iter().all(|p| p & 0xFF00_0000 == 0));
    }

    #[test]
    fn focused_frame_differs_and_caret_blinks() {
        let v = test_view();
        let base = Chrome::plain(&v);
        let mut focused = base.clone();
        focused.focused = true;
        focused.caret_on = true;
        focused.caret = Some(3);
        let a = compose(Some(&v), 0, 600, 420, &base);
        let b = compose(Some(&v), 0, 600, 420, &focused);
        let mut off = focused.clone();
        off.caret_on = false;
        let c = compose(Some(&v), 0, 600, 420, &off);
        assert_ne!(a.rgba, b.rgba);
        assert_ne!(b.rgba, c.rgba);
    }

    #[test]
    fn select_all_highlights_the_url() {
        let v = test_view();
        let mut c = Chrome::plain(&v);
        c.focused = true;
        c.caret_on = false;
        c.select_all = false;
        let plain = compose(Some(&v), 0, 600, 420, &c);
        c.select_all = true;
        let sel = compose(Some(&v), 0, 600, 420, &c);
        assert_ne!(plain.rgba, sel.rgba, "selection must paint behind the URL");
        // the highlight colour appears at the start of the field text
        let row = (TAB_H as usize + 12) * 600 * 4;
        let i = row + (TEXT_X as usize) * 4;
        assert_eq!(&sel.rgba[i..i + 3], &[SEL.0, SEL.1, SEL.2]);
    }

    #[test]
    fn multi_tab_chrome_draws_each_title() {
        let v = test_view();
        let chrome = Chrome {
            url_text: v.url.clone(),
            tabs: vec![
                TabLook {
                    title: "one".into(),
                    active: true,
                },
                TabLook {
                    title: "two".into(),
                    active: false,
                },
                TabLook {
                    title: "three".into(),
                    active: false,
                },
            ],
            status: "x".into(),
            ..Default::default()
        };
        let f = compose(Some(&v), 0, 900, 600, &chrome);
        assert_eq!(f.w, 900);
        // Active tab background appears at its rect, inactive elsewhere.
        let (a0, a1) = tab_range(0, 900, 3);
        let (b0, _) = tab_range(1, 900, 3);
        let row = (10usize) * 900 * 4;
        let at = |x: f32| {
            let i = row + (x as usize) * 4;
            (f.rgba[i], f.rgba[i + 1], f.rgba[i + 2])
        };
        assert_eq!(at((a0 + a1) / 2.0), BG_TAB_ACTIVE);
        assert_eq!(at(b0 + 10.0), BG_TAB);
    }

    #[test]
    fn scrolling_changes_frame() {
        let v = test_view();
        let c = Chrome::plain(&v);
        let a = compose(Some(&v), 0, 400, 300, &c);
        let b = compose(Some(&v), 120, 400, 300, &c);
        assert_ne!(a.rgba, b.rgba);
    }

    #[test]
    fn hit_test_finds_link_and_misses_gap() {
        let v = test_view();
        assert!(!v.links.is_empty());
        let l = &v.links[0];
        assert!(v.hit_test(l.x + 1.0, l.y + 1.0).is_some());
        assert!(v.hit_test(l.x + l.w + 50.0, l.y + l.h + 50.0).is_none());
    }

    #[test]
    fn page_view_height_arithmetic() {
        assert_eq!(page_view_height(page_top() + STATUS_H + 100), 100);
        assert_eq!(page_view_height(10), 0);
    }

    #[test]
    fn start_page_renders_offline() {
        let v = PageView::from_html(&start_page_html(), 800, false).expect("start page pixels");
        assert!(v.page_height() > 100);
        assert!(v.links.len() >= 3);
        assert!(
            v.links.iter().any(|l| l.href.contains("youtube.com/watch?v=")),
            "start page must offer the YouTube/mpv cards"
        );
    }

    #[test]
    fn youtube_detection_matches_hosts_only() {
        assert!(is_youtube("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(is_youtube("https://youtube.com/watch?v=x"));
        assert!(is_youtube("https://m.youtube.com/watch?v=x"));
        assert!(is_youtube("https://music.youtube.com/watch?v=x"));
        assert!(is_youtube("https://youtu.be/dQw4w9WgXcQ"));
        // Must not match look-alikes or unrelated hosts.
        assert!(!is_youtube("https://notyoutube.com/watch?v=x"));
        assert!(!is_youtube("https://evil-youtube.com/x"));
        assert!(!is_youtube("https://youtube.com.evil.tld/x"));
        assert!(!is_youtube("https://example.com/"));
        assert!(!is_youtube("https://i.ytimg.com/vi/x/hqdefault.jpg"));
        assert!(!is_youtube("not a url"));
    }
}
