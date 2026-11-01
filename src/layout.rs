//! Layout: block formatting, inline formatting with line breaking, a basic
//! table algorithm, list markers and replaced elements — producing a display
//! list.
//!
//! This mirrors the real browser pipeline: DOM + computed style -> layout ->
//! display list -> paint. Layout never touches pixels; it only emits commands.

use std::collections::HashMap;
use std::time::Duration;

use ego_tree::{NodeId, NodeRef};
use scraper::{ElementRef, Html, Node};
use url::Url;

use crate::color::Color;
use crate::css::Stylesheet;
use crate::font::{FontDb, fonts};
use crate::net;
use crate::style::{Align, Computed, Display, Length, ListStyle, WhiteSpace};

// ---------------------------------------------------------------------------
// display list
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Cmd {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        c: Color,
    },
    /// Four independent edge rectangles so per-side widths/colours work.
    Borders {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        widths: [f32; 4],
        colors: [Color; 4],
    },
    Text {
        x: f32,
        y: f32,
        s: String,
        size: f32,
        fam: crate::font::Family,
        bold: bool,
        c: Color,
        underline: bool,
        strike: bool,
        width: f32,
    },
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        idx: usize,
    },
    Marker {
        cx: f32,
        cy: f32,
        r: f32,
        c: Color,
        kind: MarkerKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkerKind {
    Disc,
    Circle,
    Square,
}

#[derive(Clone, Debug)]
pub struct LinkRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub href: String,
}

#[derive(Clone)]
pub struct DecodedImage {
    pub w: u32,
    pub h: u32,
    /// Straight (non-premultiplied) RGBA.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub nodes: usize,
    pub cmds: usize,
    pub images: usize,
    pub images_failed: usize,
    pub chars: usize,
    pub lines: usize,
    pub truncated: bool,
}

pub struct LayoutOutput {
    pub cmds: Vec<Cmd>,
    pub links: Vec<LinkRect>,
    pub images: Vec<DecodedImage>,
    pub canvas_bg: Color,
    pub content_height: f32,
    pub title: String,
    pub favicon: Option<String>,
    pub stats: Stats,
}

// ---------------------------------------------------------------------------
// budgets — a browser must never hang or OOM on hostile markup
// ---------------------------------------------------------------------------

const MAX_CMDS: usize = 250_000;
const MAX_NODES: usize = 60_000;
const MAX_IMAGES: usize = 80;
const MAX_DEPTH: usize = 120;
const MAX_Y: f32 = 60_000.0;
const INFINITY_W: f32 = 20_000.0;
const BORDER_SPACING: f32 = 2.0;

// ---------------------------------------------------------------------------
// inline items
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum Item {
    Word {
        text: String,
        st: Computed,
        href: Option<String>,
        w: f32,
    },
    Space {
        st: Computed,
        href: Option<String>,
        w: f32,
    },
    ForcedBreak,
    Img {
        idx: usize,
        w: f32,
        h: f32,
        st: Computed,
        href: Option<String>,
    },
    /// An inline-level block: measured up front, emitted at its final position.
    /// Holds a `NodeId` rather than a reference so no lifetime erasure is needed.
    Atomic {
        id: NodeId,
        st: Computed,
        w: f32,
        h: f32,
        href: Option<String>,
    },
}

pub struct Layout<'a> {
    doc: &'a Html,
    sheets: &'a [Stylesheet],
    base: Url,
    cmds: Vec<Cmd>,
    links: Vec<LinkRect>,
    images: Vec<DecodedImage>,
    styles: HashMap<NodeId, Computed>,
    img_index: HashMap<String, usize>,
    dry: bool,
    depth: usize,
    viewport_w: f32,
    stats: Stats,
    fetch_images: bool,
}

impl<'a> Layout<'a> {
    pub fn new(
        doc: &'a Html,
        sheets: &'a [Stylesheet],
        base: Url,
        viewport_w: f32,
        fetch_images: bool,
    ) -> Layout<'a> {
        Layout {
            doc,
            sheets,
            base,
            cmds: Vec::new(),
            links: Vec::new(),
            images: Vec::new(),
            styles: HashMap::new(),
            img_index: HashMap::new(),
            dry: false,
            depth: 0,
            viewport_w,
            stats: Stats::default(),
            fetch_images,
        }
    }

    // -- helpers ----------------------------------------------------------

    fn fonts(&self) -> &'static FontDb {
        fonts()
    }

    fn emit(&mut self, c: Cmd) {
        if self.dry {
            return;
        }
        if self.cmds.len() >= MAX_CMDS {
            self.stats.truncated = true;
            return;
        }
        self.cmds.push(c);
    }

    /// Reserve a display-list slot to fill in once the box height is known,
    /// because a background must paint *before* its children.
    fn reserve(&mut self) -> usize {
        if self.dry || self.cmds.len() >= MAX_CMDS {
            return usize::MAX;
        }
        self.cmds.push(Cmd::Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            c: Color::TRANSPARENT,
        });
        self.cmds.len() - 1
    }

    fn fill(&mut self, slot: usize, c: Cmd) {
        if slot != usize::MAX && slot < self.cmds.len() {
            self.cmds[slot] = c;
        }
    }

    fn push_link(&mut self, r: LinkRect) {
        if self.dry || r.w <= 0.5 || r.h <= 0.5 {
            return;
        }
        if self.links.len() >= MAX_CMDS {
            return;
        }
        // Merge with the previous rect when it is the same link continuing on
        // the same line, so a long link is one clickable region.
        if let Some(last) = self.links.last_mut()
            && last.href == r.href
            && (last.y - r.y).abs() < 1.0
            && (last.h - r.h).abs() < 1.0
            && r.x >= last.x + last.w - 1.5
        {
            let right = (last.x + last.w).max(r.x + r.w);
            last.w = right - last.x;
            last.h = last.h.max(r.h);
            return;
        }
        self.links.push(r);
    }

    fn style_of(&mut self, el: ElementRef<'a>, parent: &Computed) -> Computed {
        let id = el.id();
        if let Some(s) = self.styles.get(&id) {
            return *s;
        }
        let s = crate::style::compute(el, parent, self.sheets);
        self.styles.insert(id, s);
        s
    }

    fn tag(el: &ElementRef) -> String {
        el.value().name().to_ascii_lowercase()
    }

    fn children_of(&self, el: &ElementRef<'a>) -> Vec<NodeRef<'a, Node>> {
        el.children().collect()
    }

    fn element_at(&self, id: NodeId) -> Option<ElementRef<'a>> {
        self.doc.tree.get(id).and_then(ElementRef::wrap)
    }

    // -- inline item collection -------------------------------------------

    fn collect_items(
        &mut self,
        nodes: &[NodeRef<'a, Node>],
        st: &Computed,
        href: Option<String>,
        out: &mut Vec<Item>,
    ) {
        if self.depth > MAX_DEPTH || self.stats.nodes > MAX_NODES {
            self.stats.truncated = true;
            return;
        }
        for node in nodes {
            self.stats.nodes += 1;
            match node.value() {
                Node::Text(t) => {
                    let raw: &str = t.text.as_ref();
                    self.push_text_items(raw, st, href.clone(), out);
                }
                Node::Element(_) => {
                    let el = match ElementRef::wrap(*node) {
                        Some(e) => e,
                        None => continue,
                    };
                    let tag = Self::tag(&el);
                    let cst = self.style_of(el, st);
                    if cst.display == Display::None || cst.visibility_hidden {
                        continue;
                    }
                    if tag == "br" {
                        out.push(Item::ForcedBreak);
                        continue;
                    }
                    let child_href = if tag == "a" {
                        el.attr("href")
                            .map(str::trim)
                            .filter(|h| !h.is_empty())
                            .and_then(|h| self.resolve(h))
                            .or(href.clone())
                    } else {
                        href.clone()
                    };

                    match cst.display {
                        Display::Inline => {
                            if tag == "img" {
                                match self.make_image_item(&el, &cst, child_href.clone()) {
                                    Some(item) => out.push(item),
                                    None => {
                                        // Show alt text like a real browser does
                                        // when an image cannot be loaded.
                                        if let Some(alt) = el.attr("alt")
                                            && !alt.trim().is_empty()
                                        {
                                            self.push_text_items(alt, &cst, child_href, out);
                                        }
                                    }
                                }
                            } else {
                                self.depth += 1;
                                let kids = self.children_of(&el);
                                self.collect_items(&kids, &cst, child_href, out);
                                self.depth -= 1;
                            }
                        }
                        Display::InlineBlock => {
                            if let Some(item) = self.make_atomic(&el, &cst, child_href) {
                                out.push(item);
                            }
                        }
                        // A block inside inline content splits the run.
                        _ => {
                            out.push(Item::ForcedBreak);
                            self.depth += 1;
                            let kids = self.children_of(&el);
                            self.collect_items(&kids, &cst, child_href, out);
                            self.depth -= 1;
                            out.push(Item::ForcedBreak);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Split a text node into word/space items, honouring `white-space`.
    fn push_text_items(
        &mut self,
        raw: &str,
        st: &Computed,
        href: Option<String>,
        out: &mut Vec<Item>,
    ) {
        if raw.is_empty() {
            return;
        }
        let fam = st.family;
        let size = st.font_size;
        let bold = st.bold;
        let space_w = self.fonts().space_width(size, fam, bold);

        let transform = |s: String| -> String { if st.uppercase { s.to_uppercase() } else { s } };

        if st.white_space == WhiteSpace::Pre {
            for (li, line) in raw.split('\n').enumerate() {
                if li > 0 {
                    out.push(Item::ForcedBreak);
                }
                if line.is_empty() {
                    continue;
                }
                let text = transform(line.to_string());
                let w = self.fonts().measure(&text, size, fam, bold);
                self.stats.chars += text.chars().count();
                out.push(Item::Word {
                    text,
                    st: *st,
                    href: href.clone(),
                    w,
                });
            }
            return;
        }

        // Normal white-space: collapse every run of whitespace to one space.
        // The line-breaking pass drops spaces at line start/end.
        let mut cur = String::new();
        for ch in raw.chars() {
            if ch.is_whitespace() {
                if !cur.is_empty() {
                    let text = transform(std::mem::take(&mut cur));
                    let w = self.fonts().measure(&text, size, fam, bold);
                    self.stats.chars += text.chars().count();
                    out.push(Item::Word {
                        text,
                        st: *st,
                        href: href.clone(),
                        w,
                    });
                }
                out.push(Item::Space {
                    st: *st,
                    href: href.clone(),
                    w: space_w,
                });
            } else {
                cur.push(ch);
            }
        }
        if !cur.is_empty() {
            let text = transform(cur);
            let w = self.fonts().measure(&text, size, fam, bold);
            self.stats.chars += text.chars().count();
            out.push(Item::Word {
                text,
                st: *st,
                href,
                w,
            });
        }
    }

    // -- replaced elements ------------------------------------------------

    fn resolve(&self, href: &str) -> Option<String> {
        let h = href.trim();
        if h.is_empty() {
            return None;
        }
        let lower = h.to_ascii_lowercase();
        if lower.starts_with("javascript:")
            || lower.starts_with("mailto:")
            || lower.starts_with("data:")
        {
            return None;
        }
        self.base.join(h).ok().map(|u| u.to_string())
    }

    fn load_image(&mut self, src: &str) -> Option<usize> {
        if !self.fetch_images {
            return None;
        }
        if let Some(idx) = self.img_index.get(src) {
            return Some(*idx);
        }
        if self.images.len() >= MAX_IMAGES {
            self.stats.images_failed += 1;
            return None;
        }

        let data: Option<Vec<u8>> = if let Some(rest) = src.strip_prefix("data:") {
            let (meta, payload) = rest.split_once(',')?;
            if meta.to_ascii_lowercase().contains("base64") {
                base64_decode(payload)
            } else {
                Some(percent_decode(payload))
            }
        } else {
            let url = self.base.join(src).ok()?;
            if url.scheme() != "http" && url.scheme() != "https" {
                return None;
            }
            match net::fetch_cached(&url, net::MAX_IMAGE_BYTES, Duration::from_secs(600)) {
                Ok(r) if r.status < 400 => Some(r.body.clone()),
                _ => None,
            }
        };

        let bytes = data?;
        let mut rgba = match image::load_from_memory(&bytes) {
            Ok(i) => i.to_rgba8(),
            Err(_) => {
                self.stats.images_failed += 1;
                return None;
            }
        };
        // Bound memory: downscale very large images before keeping them.
        const CAP: u32 = 1600;
        if rgba.width() > CAP || rgba.height() > CAP {
            let longest = rgba.width().max(rgba.height()) as f32;
            let scale = (CAP as f32 / longest).min(1.0);
            let nw = ((rgba.width() as f32 * scale) as u32).max(1);
            let nh = ((rgba.height() as f32 * scale) as u32).max(1);
            rgba = image::imageops::resize(&rgba, nw, nh, image::imageops::FilterType::Triangle);
        }
        if rgba.width() == 0 || rgba.height() == 0 {
            return None;
        }
        let dec = DecodedImage {
            w: rgba.width(),
            h: rgba.height(),
            rgba: rgba.into_raw(),
        };
        let idx = self.images.len();
        self.images.push(dec);
        self.img_index.insert(src.to_string(), idx);
        self.stats.images += 1;
        Some(idx)
    }

    fn make_image_item(
        &mut self,
        el: &ElementRef<'a>,
        st: &Computed,
        href: Option<String>,
    ) -> Option<Item> {
        let src = el.attr("src")?.trim();
        if src.is_empty() {
            return None;
        }
        let idx = self.load_image(src)?;
        let (iw, ih) = {
            let im = &self.images[idx];
            (im.w as f32, im.h as f32)
        };
        let attr_px = |name: &str| -> Option<f32> {
            el.attr(name)
                .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
                .filter(|v| *v > 0.0 && *v < 10_000.0)
        };
        let css_w = match st.width {
            Length::Px(v) if v > 0.0 => Some(v),
            Length::Percent(p) => Some(self.viewport_w * p / 100.0),
            _ => None,
        };
        let css_h = match st.height {
            Length::Px(v) if v > 0.0 => Some(v),
            Length::Percent(p) => Some(self.viewport_w * p / 100.0),
            _ => None,
        };
        let w = css_w.or_else(|| attr_px("width")).unwrap_or(iw);
        let h =
            css_h
                .or_else(|| attr_px("height"))
                .unwrap_or(if iw > 0.0 { w * ih / iw } else { ih });
        // Never let an image overflow the viewport.
        let max_w = (self.viewport_w - 16.0).max(16.0);
        let (w, h) = if w > max_w && w > 0.0 {
            (max_w, h * max_w / w)
        } else {
            (w.max(0.0), h.max(0.0))
        };
        Some(Item::Img {
            idx,
            w,
            h,
            st: *st,
            href,
        })
    }

    /// Measure an inline-block up front so its line box knows the height.
    fn make_atomic(
        &mut self,
        el: &ElementRef<'a>,
        st: &Computed,
        href: Option<String>,
    ) -> Option<Item> {
        let explicit_w = match st.width {
            Length::Px(v) if v > 0.0 => Some(v),
            Length::Percent(p) => Some((self.viewport_w * p / 100.0).max(1.0)),
            _ => None,
        };
        let w = explicit_w
            .unwrap_or_else(|| self.max_content_width(el, st).min(self.viewport_w).max(1.0))
            .min(self.viewport_w.max(1.0));
        let h = self.measure_block_height(el, st, w);
        Some(Item::Atomic {
            id: el.id(),
            st: *st,
            w,
            h,
            href,
        })
    }

    // -- measurement (dry runs) -------------------------------------------

    /// Preferred (max-content) width of an element's inline content.
    fn max_content_width(&mut self, el: &ElementRef<'a>, st: &Computed) -> f32 {
        let saved = self.dry;
        self.dry = true;
        let kids = self.children_of(el);
        let mut items = Vec::new();
        self.collect_items(&kids, st, None, &mut items);
        let mut best = 0.0f32;
        let mut line_w = 0.0f32;
        for it in &items {
            if matches!(it, Item::ForcedBreak) {
                best = best.max(line_w);
                line_w = 0.0;
            } else {
                line_w += item_width(it);
            }
        }
        best = best.max(line_w);
        let pad = st.padding.resolved(INFINITY_W);
        let bw = st.border_w.resolved(INFINITY_W);
        let m = st.margin.resolved(INFINITY_W);
        self.dry = saved;
        best + pad[1] + pad[3] + bw[1] + bw[3] + m[1] + m[3]
    }

    /// Height a block would occupy at a given width, without emitting.
    fn measure_block_height(&mut self, el: &ElementRef<'a>, st: &Computed, width: f32) -> f32 {
        let saved = self.dry;
        self.dry = true;
        let h = self.layout_block(*el, *st, 0.0, 0.0, width, None);
        self.dry = saved;
        h
    }

    // -- block layout ------------------------------------------------------

    /// Lay out a block-level box. Returns the total outer height consumed,
    /// including vertical margins.
    fn layout_block(
        &mut self,
        el: ElementRef<'a>,
        st: Computed,
        x: f32,
        y: f32,
        avail: f32,
        list_marker: Option<(ListStyle, usize)>,
    ) -> f32 {
        if self.depth > MAX_DEPTH || y > MAX_Y || self.stats.nodes > MAX_NODES {
            self.stats.truncated = true;
            return 0.0;
        }
        if st.display == Display::None || st.visibility_hidden {
            return 0.0;
        }

        let tag = Self::tag(&el);
        if st.display == Display::Table || (tag == "table" && st.display == Display::Block) {
            return self.layout_table(el, st, x, y, avail);
        }

        let margin = st.margin.resolved(avail);
        let pad = st.padding.resolved(avail);
        let bw = st.border_w.resolved(avail);

        let border_w = match st.width {
            Length::Px(w) => w + pad[1] + pad[3] + bw[1] + bw[3],
            Length::Percent(p) => avail * p / 100.0,
            Length::Auto => avail - margin[1] - margin[3],
        };
        let border_w = border_w.max(0.0).min(avail.max(0.0));

        let bx = x + margin[3];
        let by = y + margin[0];
        let content_w = (border_w - bw[1] - bw[3] - pad[1] - pad[3]).max(0.0);
        let cx = bx + bw[3] + pad[3];
        let cy = by + bw[0] + pad[0];

        let bg_slot = self.reserve();
        let border_slot = self.reserve();

        self.depth += 1;
        let content_h = self.layout_children(&el, &st, cx, cy, content_w);
        self.depth -= 1;

        let height = match st.height {
            Length::Px(h) if h > 0.0 => h,
            _ => content_h,
        };
        let box_h = height + pad[0] + pad[2] + bw[0] + bw[2];

        if !st.background.is_transparent() {
            self.fill(
                bg_slot,
                Cmd::Rect {
                    x: bx,
                    y: by,
                    w: border_w,
                    h: box_h,
                    c: st.background,
                },
            );
        }
        if bw.iter().any(|v| *v > 0.05) && st.border_c.iter().any(|c| !c.is_transparent()) {
            self.fill(
                border_slot,
                Cmd::Borders {
                    x: bx,
                    y: by,
                    w: border_w,
                    h: box_h,
                    widths: bw,
                    colors: st.border_c,
                },
            );
        }

        // List markers are painted into the left padding gutter of the item.
        if let Some((kind, index)) = list_marker
            && kind != ListStyle::None
        {
            let (asc, _desc, _gap) = self.fonts().metrics(st.font_size, st.family, st.bold);
            let baseline = cy + asc;
            match kind {
                ListStyle::Decimal => {
                    let s = format!("{}.", index + 1);
                    let w = self.fonts().measure(&s, st.font_size, st.family, st.bold);
                    self.emit(Cmd::Text {
                        x: (cx - 8.0 - w).max(2.0),
                        y: baseline,
                        s,
                        size: st.font_size,
                        fam: st.family,
                        bold: false,
                        c: st.color,
                        underline: false,
                        strike: false,
                        width: w,
                    });
                }
                other => {
                    let mk = match other {
                        ListStyle::Circle => MarkerKind::Circle,
                        ListStyle::Square => MarkerKind::Square,
                        _ => MarkerKind::Disc,
                    };
                    self.emit(Cmd::Marker {
                        cx: (cx - 14.0).max(4.0),
                        cy: baseline - st.font_size * 0.3,
                        r: (st.font_size * 0.15).clamp(1.5, 5.0),
                        c: st.color,
                        kind: mk,
                    });
                }
            }
        }

        margin[0] + box_h + margin[2]
    }

    /// Lay out a block box's children, splitting them into block-level boxes
    /// and anonymous inline runs.
    fn layout_children(
        &mut self,
        el: &ElementRef<'a>,
        st: &Computed,
        cx: f32,
        cy: f32,
        content_w: f32,
    ) -> f32 {
        let kids = self.children_of(el);
        if kids.is_empty() {
            return 0.0;
        }
        let parent_tag = Self::tag(el);
        let is_list = matches!(parent_tag.as_str(), "ul" | "ol" | "menu");
        let ordered = parent_tag == "ol";

        // Classify children (styles are cached, so this pass is cheap).
        let mut classified: Vec<(NodeRef<'a, Node>, bool)> = Vec::with_capacity(kids.len());
        for k in &kids {
            let blockish = match k.value() {
                Node::Element(_) => {
                    let e = match ElementRef::wrap(*k) {
                        Some(e) => e,
                        None => continue,
                    };
                    let cs = self.style_of(e, st);
                    !matches!(cs.display, Display::Inline | Display::InlineBlock)
                }
                Node::Text(t) => {
                    let raw: &str = t.text.as_ref();
                    raw.trim().is_empty()
                }
                _ => true,
            };
            classified.push((*k, blockish));
        }

        let mut cursor = cy;
        let mut inline_buf: Vec<NodeRef<'a, Node>> = Vec::new();
        let mut prev_bottom_margin = 0.0f32;
        let mut first_block = true;
        let mut li_index = 0usize;

        for (node, blockish) in classified {
            let is_element = matches!(node.value(), Node::Element(_));
            if !(blockish && is_element) {
                inline_buf.push(node);
                continue;
            }

            if !inline_buf.is_empty() {
                cursor += self.layout_inline(&inline_buf, st, cx, cursor, content_w, None);
                inline_buf.clear();
                prev_bottom_margin = 0.0;
                first_block = false;
            }

            let child_el = match ElementRef::wrap(node) {
                Some(e) => e,
                None => continue,
            };
            let cst = self.style_of(child_el, st);
            if cst.display == Display::None {
                continue;
            }
            let child_tag = Self::tag(&child_el);

            // Adjacent-sibling margin collapsing.
            let mt = cst.margin.0[0].resolve(content_w, 0.0);
            if !first_block {
                let collapsed = mt.max(prev_bottom_margin);
                let overshoot = (prev_bottom_margin + mt) - collapsed;
                cursor -= overshoot.max(0.0);
            }

            let marker = if is_list && child_tag == "li" {
                let kind = if ordered {
                    ListStyle::Decimal
                } else {
                    cst.list_style
                };
                li_index += 1;
                Some((kind, li_index - 1))
            } else {
                None
            };

            self.stats.nodes += 1;
            cursor += self.layout_block(child_el, cst, cx, cursor, content_w, marker);
            prev_bottom_margin = cst.margin.0[2].resolve(content_w, 0.0);
            first_block = false;
        }

        if !inline_buf.is_empty() {
            cursor += self.layout_inline(&inline_buf, st, cx, cursor, content_w, None);
        }

        (cursor - cy).max(0.0)
    }

    // -- inline layout -----------------------------------------------------

    fn layout_inline(
        &mut self,
        nodes: &[NodeRef<'a, Node>],
        st: &Computed,
        x: f32,
        y: f32,
        width: f32,
        href: Option<String>,
    ) -> f32 {
        let mut items: Vec<Item> = Vec::new();
        self.collect_items(nodes, st, href, &mut items);
        self.render_items(&items, st, x, y, width)
    }

    /// Break items into line boxes and emit text / image / decoration commands.
    fn render_items(&mut self, items: &[Item], st: &Computed, x: f32, y: f32, width: f32) -> f32 {
        if items.is_empty() || width <= 0.0 {
            return 0.0;
        }

        // Pass 1: greedy line breaking.
        let mut lines: Vec<Vec<&Item>> = vec![Vec::new()];
        let mut line_w = 0.0f32;
        let mut pending_space: Option<&Item> = None;
        let mut saw_content = false;

        for it in items {
            match it {
                Item::ForcedBreak => {
                    lines.push(Vec::new());
                    line_w = 0.0;
                    pending_space = None;
                    saw_content = false;
                }
                Item::Space { .. } => {
                    if saw_content {
                        pending_space = Some(it);
                    }
                }
                _ => {
                    let w = item_width(it);
                    let extra = pending_space.map(item_width).unwrap_or(0.0);
                    if saw_content && line_w + extra + w > width {
                        // Wrap: the pending space is dropped at the break.
                        lines.push(Vec::new());
                        line_w = w;
                    } else {
                        if let Some(sp) = pending_space {
                            if let Some(cur) = lines.last_mut() {
                                cur.push(sp);
                            }
                            line_w += extra;
                        }
                        line_w += w;
                    }
                    pending_space = None;
                    if let Some(cur) = lines.last_mut() {
                        cur.push(it);
                    }
                    saw_content = true;
                }
            }
        }

        // Pass 2: position and emit.
        let mut cursor_y = y;
        for line in &lines {
            if line.is_empty() {
                // An empty line still consumes one line box (trailing <br>).
                let (asc, desc, _g) = self.fonts().metrics(st.font_size, st.family, st.bold);
                cursor_y += st.line_box(asc, desc);
                self.stats.lines += 1;
                continue;
            }

            let mut total_w = 0.0f32;
            let mut max_asc = 0.0f32;
            let mut max_desc = 0.0f32;
            let mut max_line_h = 0.0f32;
            for it in line {
                total_w += item_width(it);
                let (asc, desc) = item_metrics(it, self.fonts());
                max_asc = max_asc.max(asc);
                max_desc = max_desc.max(desc);
                let lh = item_style(it)
                    .map(|s| s.line_box(asc, desc))
                    .unwrap_or(asc + desc);
                max_line_h = max_line_h.max(lh);
            }
            let line_h = max_line_h.max(max_asc + max_desc);

            let slack = (width - total_w).max(0.0);
            let start_x = match st.align {
                Align::Left | Align::Justify => x,
                Align::Center => x + slack / 2.0,
                Align::Right => x + slack,
            };
            let half_leading = ((line_h - (max_asc + max_desc)) / 2.0).max(0.0);
            let baseline = cursor_y + half_leading + max_asc;
            let line_top = cursor_y;

            let mut pen = start_x;
            for it in line {
                let w = item_width(it);
                match it {
                    Item::Word {
                        text,
                        st: ws,
                        href,
                        w: tw,
                    } => {
                        if !ws.background.is_transparent() {
                            self.emit(Cmd::Rect {
                                x: pen,
                                y: line_top,
                                w: *tw,
                                h: line_h,
                                c: ws.background,
                            });
                        }
                        self.emit(Cmd::Text {
                            x: pen,
                            y: baseline,
                            s: text.clone(),
                            size: ws.font_size,
                            fam: ws.family,
                            bold: ws.bold,
                            c: ws.color,
                            underline: ws.underline,
                            strike: ws.line_through,
                            width: *tw,
                        });
                        if let Some(h) = href {
                            self.push_link(LinkRect {
                                x: pen,
                                y: line_top,
                                w: *tw,
                                h: line_h,
                                href: h.clone(),
                            });
                        }
                    }
                    Item::Space { href, .. } => {
                        if let Some(h) = href {
                            self.push_link(LinkRect {
                                x: pen,
                                y: line_top,
                                w,
                                h: line_h,
                                href: h.clone(),
                            });
                        }
                    }
                    Item::Img {
                        idx,
                        w: iw,
                        h: ih,
                        href,
                        ..
                    } => {
                        let top = baseline - ih;
                        self.emit(Cmd::Image {
                            x: pen,
                            y: top,
                            w: *iw,
                            h: *ih,
                            idx: *idx,
                        });
                        if let Some(h) = href {
                            self.push_link(LinkRect {
                                x: pen,
                                y: top,
                                w: *iw,
                                h: *ih,
                                href: h.clone(),
                            });
                        }
                    }
                    Item::Atomic {
                        id,
                        st: ast,
                        w: aw,
                        h: ah,
                        href,
                    } => {
                        let top = baseline - ah;
                        if let Some(el) = self.element_at(*id) {
                            let saved = self.depth;
                            self.depth = 0;
                            self.layout_block(el, *ast, pen, top, *aw, None);
                            self.depth = saved;
                        }
                        if let Some(h) = href {
                            self.push_link(LinkRect {
                                x: pen,
                                y: top,
                                w: *aw,
                                h: *ah,
                                href: h.clone(),
                            });
                        }
                    }
                    Item::ForcedBreak => {}
                }
                pen += w;
            }

            cursor_y += line_h;
            self.stats.lines += 1;
        }
        (cursor_y - y).max(0.0)
    }

    // -- tables -----------------------------------------------------------

    fn layout_table(
        &mut self,
        el: ElementRef<'a>,
        st: Computed,
        x: f32,
        y: f32,
        avail: f32,
    ) -> f32 {
        let margin = st.margin.resolved(avail);
        let pad = st.padding.resolved(avail);
        let bw = st.border_w.resolved(avail);
        let bx = x + margin[3];
        let by = y + margin[0];
        let table_w = match st.width {
            Length::Px(w) => w + pad[1] + pad[3] + bw[1] + bw[3],
            Length::Percent(p) => avail * p / 100.0,
            Length::Auto => avail - margin[1] - margin[3],
        }
        .max(0.0)
        .min(avail.max(0.0));
        let inner_w = (table_w - bw[1] - bw[3] - pad[1] - pad[3]).max(0.0);
        let cx = bx + bw[3] + pad[3];
        let cy = by + bw[0] + pad[0];

        let bg_slot = self.reserve();
        let border_slot = self.reserve();

        let mut rows: Vec<Vec<(ElementRef<'a>, usize)>> = Vec::new();
        let mut caption: Option<ElementRef<'a>> = None;
        self.gather_rows(&el, &mut rows, &mut caption, 0);

        if rows.is_empty() {
            let h = pad[0] + pad[2] + bw[0] + bw[2];
            return margin[0] + h + margin[2];
        }

        let ncols = rows
            .iter()
            .map(|r| r.iter().map(|(_, k)| *k).sum::<usize>())
            .max()
            .unwrap_or(1)
            .max(1);
        let avail_for_cols = (inner_w - BORDER_SPACING * (ncols as f32 + 1.0)).max(ncols as f32);

        // Column sizing: max-content measurement, then proportional fit.
        let mut pref = vec![0.0f32; ncols];
        let mut minw = vec![0.0f32; ncols];
        for row in &rows {
            let mut col = 0usize;
            for (cell, k) in row {
                if col >= ncols {
                    break;
                }
                if *k == 1 {
                    let cst = self.style_of(*cell, &st);
                    let cw = self.cell_content_width(cell, &cst);
                    pref[col] = pref[col].max(cw.preferred);
                    minw[col] = minw[col].max(cw.min);
                }
                col += *k;
            }
        }
        let sum_pref: f32 = pref.iter().sum();
        let mut widths = vec![0.0f32; ncols];
        if sum_pref <= 0.0 {
            widths
                .iter_mut()
                .for_each(|c| *c = avail_for_cols / ncols as f32);
        } else if sum_pref <= avail_for_cols {
            widths.clone_from(&pref);
            let leftover = avail_for_cols - sum_pref;
            for j in 0..ncols {
                widths[j] += leftover * (pref[j] / sum_pref);
            }
        } else {
            let scale = avail_for_cols / sum_pref;
            for j in 0..ncols {
                widths[j] = (pref[j] * scale).max(minw[j].min(avail_for_cols));
            }
            let sum_after: f32 = widths.iter().sum();
            if sum_after > avail_for_cols && sum_after > 0.0 {
                let f = avail_for_cols / sum_after;
                widths.iter_mut().for_each(|w| *w *= f);
            }
        }

        let mut cursor = cy + BORDER_SPACING;

        if let Some(cap) = caption {
            let cst = self.style_of(cap, &st);
            cursor += self.layout_block(cap, cst, cx, cursor, inner_w, None);
        }

        for row in rows {
            let mut col_x = cx + BORDER_SPACING;
            let row_top = cursor;
            let mut row_h = 0.0f32;
            let mut col = 0usize;
            for (cell, span) in row {
                if col >= ncols {
                    break;
                }
                // A spanning cell occupies k columns and the gaps between them.
                let k = span.min(ncols - col).max(1);
                let cw: f32 =
                    widths[col..col + k].iter().sum::<f32>() + BORDER_SPACING * (k as f32 - 1.0);
                let cst = self.style_of(cell, &st);
                row_h = row_h.max(self.layout_block(cell, cst, col_x, row_top, cw, None));
                col_x += cw + BORDER_SPACING;
                col += k;
            }
            if row_h <= 0.0 {
                row_h = st.font_size * 0.5;
            }
            cursor += row_h + BORDER_SPACING;
        }

        let content_h = (cursor - cy).max(0.0);
        let box_h = content_h + pad[0] + pad[2] + bw[0] + bw[2];
        if !st.background.is_transparent() {
            self.fill(
                bg_slot,
                Cmd::Rect {
                    x: bx,
                    y: by,
                    w: table_w,
                    h: box_h,
                    c: st.background,
                },
            );
        }
        if bw.iter().any(|v| *v > 0.05) && st.border_c.iter().any(|c| !c.is_transparent()) {
            self.fill(
                border_slot,
                Cmd::Borders {
                    x: bx,
                    y: by,
                    w: table_w,
                    h: box_h,
                    widths: bw,
                    colors: st.border_c,
                },
            );
        }
        margin[0] + box_h + margin[2]
    }

    fn gather_rows(
        &mut self,
        el: &ElementRef<'a>,
        rows: &mut Vec<Vec<(ElementRef<'a>, usize)>>,
        caption: &mut Option<ElementRef<'a>>,
        depth: usize,
    ) {
        if depth > 8 {
            return;
        }
        for child in el.children() {
            let e = match ElementRef::wrap(child) {
                Some(e) => e,
                None => continue,
            };
            match Self::tag(&e).as_str() {
                "tr" => {
                    let mut cells = Vec::new();
                    self.gather_cells(&e, &mut cells);
                    if !cells.is_empty() {
                        rows.push(cells);
                    }
                }
                "caption" => {
                    if caption.is_none() {
                        *caption = Some(e);
                    }
                }
                // Row groups are transparent to row collection.
                "tbody" | "thead" | "tfoot" | "colgroup" => {
                    self.gather_rows(&e, rows, caption, depth + 1);
                }
                // Anything else (a nested table) is left to its own layout pass.
                _ => {}
            }
        }
    }

    fn gather_cells(&mut self, row: &ElementRef<'a>, cells: &mut Vec<(ElementRef<'a>, usize)>) {
        for child in row.children() {
            if let Some(e) = ElementRef::wrap(child)
                && matches!(Self::tag(&e).as_str(), "td" | "th")
            {
                let k = e
                    .attr("colspan")
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 1000);
                cells.push((e, k));
            }
        }
    }

    fn cell_content_width(&mut self, cell: &ElementRef<'a>, cst: &Computed) -> CellWidth {
        let saved = self.dry;
        self.dry = true;
        let kids = self.children_of(cell);
        let mut items = Vec::new();
        self.collect_items(&kids, cst, None, &mut items);
        let mut preferred = 0.0f32;
        let mut line_w = 0.0f32;
        let mut min = 0.0f32;
        for it in &items {
            let w = item_width(it);
            if matches!(it, Item::ForcedBreak) {
                preferred = preferred.max(line_w);
                line_w = 0.0;
            } else {
                line_w += w;
                min = min.max(w);
            }
        }
        preferred = preferred.max(line_w);
        let pad = cst.padding.resolved(INFINITY_W);
        let bw = cst.border_w.resolved(INFINITY_W);
        let extra = pad[1] + pad[3] + bw[1] + bw[3];
        self.dry = saved;
        CellWidth {
            preferred: preferred + extra,
            min: min + extra,
        }
    }

    // -- entry point -------------------------------------------------------

    pub fn run(&mut self) -> LayoutOutput {
        let root = self.doc.root_element();
        let root_style = Computed::initial();

        let body_sel = crate::sel("body");
        let title_sel = crate::sel("title");
        let icon_sel = crate::sel("link[rel~=icon], link[rel*='icon']");

        let body = self.doc.select(&body_sel).next().unwrap_or(root);

        let title = self
            .doc
            .select(&title_sel)
            .next()
            .map(|t| collapse_ws(&t.text().collect::<String>()))
            .unwrap_or_default();

        let favicon = self
            .doc
            .select(&icon_sel)
            .next()
            .and_then(|l| l.attr("href"))
            .and_then(|h| self.base.join(h.trim()).ok())
            .map(|u| u.to_string());

        let html_style = self.style_of(root, &root_style);
        let body_style = self.style_of(body, &root_style);

        // The canvas background comes from <html>, falling back to <body>.
        let canvas_bg = if !html_style.background.is_transparent() {
            html_style.background
        } else if !body_style.background.is_transparent() {
            body_style.background
        } else {
            Color::WHITE
        };

        let vw = self.viewport_w;
        let content_height = self.layout_block(body, body_style, 0.0, 0.0, vw, None);

        let cmds = std::mem::take(&mut self.cmds);
        let mut stats = self.stats;
        stats.cmds = cmds.len();

        LayoutOutput {
            cmds,
            links: std::mem::take(&mut self.links),
            images: std::mem::take(&mut self.images),
            canvas_bg,
            content_height: content_height.max(1.0),
            title,
            favicon,
            stats,
        }
    }
}

struct CellWidth {
    preferred: f32,
    min: f32,
}

fn item_width(it: &Item) -> f32 {
    match it {
        Item::Word { w, .. } | Item::Space { w, .. } => *w,
        Item::Img { w, .. } => *w,
        Item::Atomic { w, .. } => *w,
        Item::ForcedBreak => 0.0,
    }
}

fn item_style(it: &Item) -> Option<&Computed> {
    match it {
        Item::Word { st, .. }
        | Item::Space { st, .. }
        | Item::Img { st, .. }
        | Item::Atomic { st, .. } => Some(st),
        Item::ForcedBreak => None,
    }
}

fn item_metrics(it: &Item, f: &FontDb) -> (f32, f32) {
    match it {
        Item::Word { st, .. } | Item::Space { st, .. } => {
            let (a, d, _g) = f.metrics(st.font_size, st.family, st.bold);
            (a, d)
        }
        Item::Img { h, .. } => (*h, 2.0),
        Item::Atomic { h, .. } => (*h, 2.0),
        Item::ForcedBreak => (0.0, 0.0),
    }
}

/// Collapse whitespace runs (used for `<title>`).
pub fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out.trim().to_string()
}

/// Minimal percent-decoding for `data:` URIs.
fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// base64 decode (padding tolerant, accepts the URL-safe alphabet).
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        })
    }
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::with_capacity(input.len() / 4 * 3 + 4);
    for &b in input.as_bytes() {
        if b == b'=' || b.is_ascii_whitespace() {
            continue;
        }
        buf = (buf << 6) | val(b)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello".to_vec());
        assert_eq!(base64_decode("YQ==").unwrap(), b"a".to_vec());
        assert_eq!(base64_decode("YWJj").unwrap(), b"abc".to_vec());
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b"), b"a b".to_vec());
        assert_eq!(percent_decode("%FF%00"), vec![0xff, 0x00]);
        assert_eq!(percent_decode("plain"), b"plain".to_vec());
    }

    #[test]
    fn whitespace_collapse() {
        assert_eq!(collapse_ws("  a \n\t b  "), "a b");
        assert_eq!(collapse_ws(""), "");
        assert_eq!(collapse_ws("\n\n"), "");
    }

    #[test]
    fn forced_break_has_no_width() {
        assert_eq!(item_width(&Item::ForcedBreak), 0.0);
        assert_eq!(item_metrics(&Item::ForcedBreak, fonts()), (0.0, 0.0));
        assert!(item_style(&Item::ForcedBreak).is_none());
    }

    /// A real end-to-end layout: HTML + CSS in, display list out.
    #[test]
    fn lays_out_a_simple_document() {
        let html = r#"<html><head><title>Hi</title>
            <style>p { color: red; margin: 0 } h1 { font-size: 20px }</style></head>
            <body><h1>Title</h1><p>Some words here</p><a href="/next">link</a></body></html>"#;
        let doc = Html::parse_document(html);
        let (ua, order) = Stylesheet::parse(crate::style::UA_CSS, 0);
        let css = doc
            .select(&crate::sel("style"))
            .map(|s| s.text().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let (author, _) = Stylesheet::parse(&css, order);
        let sheets = vec![ua, author];
        let base = Url::parse("https://example.com/page").unwrap();
        let mut lay = Layout::new(&doc, &sheets, base, 800.0, false);
        let out = lay.run();

        assert_eq!(out.title, "Hi");
        assert!(out.content_height > 0.0);
        assert!(
            out.cmds.len() > 3,
            "expected paints, got {}",
            out.cmds.len()
        );
        // The <a href="/next"> must produce a clickable, absolutised region.
        assert!(
            out.links
                .iter()
                .any(|l| l.href == "https://example.com/next"),
            "links: {:?}",
            out.links
        );
        // Red text from the author stylesheet must have reached the display list.
        let red = out.cmds.iter().any(|c| match c {
            Cmd::Text { c, .. } => c.r > 200 && c.g < 60 && c.b < 60,
            _ => false,
        });
        assert!(red, "author CSS colour was not applied");
    }

    /// Deeply nested markup must hit the depth budget instead of overflowing.
    #[test]
    fn deep_nesting_is_bounded() {
        let n = 400;
        let html = format!(
            "<html><body>{}deep{}</body></html>",
            "<div>".repeat(n),
            "</div>".repeat(n)
        );
        let doc = Html::parse_document(&html);
        let (ua, _) = Stylesheet::parse(crate::style::UA_CSS, 0);
        let sheets = vec![ua];
        let base = Url::parse("https://example.com/").unwrap();
        let mut lay = Layout::new(&doc, &sheets, base, 800.0, false);
        let out = lay.run();
        assert!(out.content_height.is_finite());
        assert!(out.cmds.len() < MAX_CMDS);
    }

    #[test]
    fn table_produces_cells() {
        let html = "<html><body><table><tr><td>a</td><td>bb</td></tr>\
                    <tr><td>ccc</td><td>d</td></tr></table></body></html>";
        let doc = Html::parse_document(html);
        let (ua, _) = Stylesheet::parse(crate::style::UA_CSS, 0);
        let sheets = vec![ua];
        let base = Url::parse("https://example.com/").unwrap();
        let mut lay = Layout::new(&doc, &sheets, base, 600.0, false);
        let out = lay.run();
        let texts = out
            .cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Text { s, .. } => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            texts.contains(&"a") && texts.contains(&"ccc"),
            "got {texts:?}"
        );
    }
}
