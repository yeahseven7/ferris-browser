//! Style resolution: the cascade (UA sheet -> author sheets -> inline style),
//! specificity ordering, `!important`, unit resolution and inheritance.

use crate::color::{Color, parse_any};
use crate::css::{Declaration, Stylesheet};
use crate::font::Family;
use scraper::ElementRef;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Table,
    TableRow,
    TableCell,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WhiteSpace {
    Normal,
    Pre,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
}

/// A length that may still depend on the containing block width.
#[derive(Clone, Copy, Debug)]
pub enum Length {
    Auto,
    Px(f32),
    Percent(f32),
}

impl Length {
    pub fn resolve(&self, cb: f32, auto: f32) -> f32 {
        match self {
            Length::Auto => auto,
            Length::Px(v) => *v,
            Length::Percent(p) => cb * p / 100.0,
        }
    }
    pub fn is_auto(&self) -> bool {
        matches!(self, Length::Auto)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Edges(pub [Length; 4]); // top, right, bottom, left

impl Edges {
    pub fn zero() -> Edges {
        Edges([Length::Px(0.0); 4])
    }
    pub fn resolved(&self, cb: f32) -> [f32; 4] {
        [
            self.0[0].resolve(cb, 0.0),
            self.0[1].resolve(cb, 0.0),
            self.0[2].resolve(cb, 0.0),
            self.0[3].resolve(cb, 0.0),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Computed {
    pub display: Display,
    pub color: Color,
    pub background: Color,
    pub font_size: f32,
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
    pub line_height: Option<f32>,
    pub margin: Edges,
    pub padding: Edges,
    pub border_w: Edges,
    pub border_c: [Color; 4],
    pub width: Length,
    pub height: Length,
    pub align: Align,
    pub underline: bool,
    pub line_through: bool,
    pub white_space: WhiteSpace,
    pub list_style: ListStyle,
    pub uppercase: bool,
    pub visibility_hidden: bool,
}

impl Computed {
    /// Root defaults: everything the initial containing block starts from.
    pub fn initial() -> Computed {
        Computed {
            // CSS's initial value for `display` is `inline`; block-level boxes
            // get theirs from the user-agent stylesheet.
            display: Display::Inline,
            color: Color::BLACK,
            background: Color::TRANSPARENT,
            font_size: 16.0,
            family: Family::Sans,
            bold: false,
            italic: false,
            line_height: None,
            margin: Edges::zero(),
            padding: Edges::zero(),
            border_w: Edges::zero(),
            border_c: [Color::BLACK; 4],
            width: Length::Auto,
            height: Length::Auto,
            align: Align::Left,
            underline: false,
            line_through: false,
            white_space: WhiteSpace::Normal,
            list_style: ListStyle::None,
            uppercase: false,
            visibility_hidden: false,
        }
    }

    /// Start from the parent, carrying only inherited properties.
    pub fn inherit(parent: &Computed) -> Computed {
        let mut s = Computed::initial();
        s.color = parent.color;
        s.font_size = parent.font_size;
        s.family = parent.family;
        s.bold = parent.bold;
        s.italic = parent.italic;
        s.line_height = parent.line_height;
        s.align = parent.align;
        s.underline = parent.underline;
        s.line_through = parent.line_through;
        s.white_space = parent.white_space;
        s.list_style = parent.list_style;
        s.uppercase = parent.uppercase;
        s.visibility_hidden = parent.visibility_hidden;
        s
    }

    /// Effective line box height for this font size.
    pub fn line_box(&self, ascent: f32, descent: f32) -> f32 {
        match self.line_height {
            Some(lh) => lh.max(1.0),
            None => (ascent + descent).max(self.font_size * 1.2),
        }
    }
}

/// The default user-agent stylesheet, as required by CSS 2.1 Appendix D.
pub const UA_CSS: &str = r#"
html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dt, dd, form,
header, footer, section, article, aside, main, nav, figure, figcaption,
blockquote, pre, address, fieldset, legend, details, summary, hr, table,
caption, colgroup, tbody, thead, tfoot, center, video, audio, canvas, iframe,
svg, form, dialog, dd, dt, menu, dir, search, hgroup, listing, xmp, plaintext,
object, embed, marquee, noframes, frameset, optgroup, multicol, task-list { display: block }
head, style, script, title, meta, link, base, noscript, template, param,
source, track { display: none }
html { color: black; background-color: white }
body { margin: 8px }
h1 { font-size: 2em; margin: 0.67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: 0.83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { margin: 1.33em 0; font-weight: bold }
h5 { font-size: 0.83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: 0.67em; margin: 2.33em 0; font-weight: bold }
p, dl, dd, fieldset, form, address, blockquote, center, figure, pre { margin: 1em 0 }
blockquote { margin: 1em 40px }
ul { margin: 1em 0; padding-left: 40px; list-style-type: disc }
ol { margin: 1em 0; padding-left: 40px; list-style-type: decimal }
li { display: list-item }
ul ul, ol ul { list-style-type: circle }
ol ol, ul ol { list-style-type: decimal }
dt { font-weight: normal }
dd { margin-left: 40px }
hr { margin: 0.5em auto; border-top-width: 1px; border-top-style: solid; border-top-color: gray; height: 0 }
a { color: #0000EE; text-decoration: underline }
a:link, a:visited { color: #0000EE }
strong, b { font-weight: bold }
i, em, cite, var, address, dfn { font-style: italic }
code, kbd, samp, pre, tt { font-family: monospace }
pre { margin: 1em 0; white-space: pre }
small { font-size: 0.83em }
big { font-size: 1.17em }
sub { font-size: 0.83em }
sup { font-size: 0.83em }
mark { background-color: yellow; color: black }
center { text-align: center }
table { display: table; border-spacing: 2px }
caption { display: table-caption; text-align: center }
thead { display: table-header-group }
tbody { display: table-row-group }
tfoot { display: table-footer-group }
tr { display: table-row }
td, th { display: table-cell; padding: 1px; vertical-align: inherit }
th { font-weight: bold; text-align: center }
button, input, select, textarea { display: inline-block; font-family: sans-serif }
input, textarea, select { border-width: 1px; border-style: solid; border-color: #767676; background-color: white; padding: 1px 2px }
button { border-width: 1px; border-style: solid; border-color: #adadad; background-color: #efefef; padding: 1px 6px }
img { display: inline }
br { display: inline }
iframe { border-width: 2px; border-style: inset; border-color: gray }
nobr { white-space: nowrap }
ins, u { text-decoration: underline }
del, s, strike { text-decoration: line-through }
hidden, [hidden] { display: none }
"#;

// ---------------------------------------------------------------------------
// value parsing
// ---------------------------------------------------------------------------

fn font_size_keyword(v: &str, parent_size: f32) -> Option<f32> {
    Some(match v {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 42.0,
        "smaller" => parent_size / 1.2,
        "larger" => parent_size * 1.2,
        _ => return None,
    })
}

/// Parse a length. `em`/`rem` resolve immediately against the given font size;
/// percentages are kept deferred for layout time.
fn parse_length(v: &str, font_size: f32, root_font_size: f32) -> Length {
    let v = v.trim();
    if v.is_empty() {
        return Length::Auto;
    }
    if v == "auto" || v == "inherit" || v == "initial" || v == "unset" {
        return Length::Auto;
    }
    if v == "thin" {
        return Length::Px(1.0);
    }
    if v == "medium" {
        return Length::Px(3.0);
    }
    if v == "thick" {
        return Length::Px(5.0);
    }
    let lower = v.to_ascii_lowercase();
    let num_end = lower
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(lower.len());
    let (num_s, unit) = lower.split_at(num_end);
    let n: f32 = match num_s.trim().parse() {
        Ok(n) => n,
        Err(_) => {
            // unitless zero
            if num_s.trim().is_empty() {
                return Length::Auto;
            }
            return Length::Auto;
        }
    };
    match unit.trim() {
        "" | "px" => Length::Px(n),
        "%" => Length::Percent(n),
        "em" => Length::Px(n * font_size),
        "rem" => Length::Px(n * root_font_size),
        "pt" => Length::Px(n * 96.0 / 72.0),
        "pc" => Length::Px(n * 16.0),
        "in" => Length::Px(n * 96.0),
        "cm" => Length::Px(n * 96.0 / 2.54),
        "mm" => Length::Px(n * 96.0 / 25.4),
        "q" => Length::Px(n * 96.0 / 25.4 / 4.0),
        "vh" | "vw" => Length::Px(n * 8.0),
        _ => Length::Px(n),
    }
}

/// Expand a 1-4 value box shorthand into [top, right, bottom, left].
fn parse_edges(v: &str, font_size: f32, root: f32) -> Edges {
    let parts: Vec<&str> = v.split_whitespace().collect();
    let l = |s: &str| parse_length(s, font_size, root);
    Edges(match parts.len() {
        1 => [l(parts[0]); 4],
        2 => [l(parts[0]), l(parts[1]), l(parts[0]), l(parts[1])],
        3 => [l(parts[0]), l(parts[1]), l(parts[2]), l(parts[1])],
        4 => [l(parts[0]), l(parts[1]), l(parts[2]), l(parts[3])],
        _ => [Length::Px(0.0); 4],
    })
}

fn parse_display(v: &str) -> Option<Display> {
    Some(match v.trim() {
        "block" | "flex" | "grid" | "flow-root" | "list-item" | "table-caption"
        | "table-row-group" | "table-header-group" | "table-footer-group" => Display::Block,
        "inline" => Display::Inline,
        "inline-block" | "inline-flex" | "inline-grid" | "inline-table" => Display::InlineBlock,
        "table" => Display::Table,
        "table-row" => Display::TableRow,
        "table-cell" => Display::TableCell,
        "none" => Display::None,
        _ => return None,
    })
}

fn parse_align(v: &str) -> Option<Align> {
    Some(match v.trim() {
        "left" | "start" => Align::Left,
        "center" | "middle" => Align::Center,
        "right" | "end" => Align::Right,
        "justify" => Align::Justify,
        _ => return None,
    })
}

fn parse_list_style(v: &str) -> Option<ListStyle> {
    Some(match v.trim() {
        "disc" => ListStyle::Disc,
        "circle" => ListStyle::Circle,
        "square" => ListStyle::Square,
        "decimal" | "decimal-leading-zero" => ListStyle::Decimal,
        "none" => ListStyle::None,
        _ => return None,
    })
}

fn parse_color_value(v: &str, current: Color) -> Option<Color> {
    let v = v.trim();
    // `border-color: currentcolor` and similar
    if v.eq_ignore_ascii_case("currentcolor") {
        return Some(current);
    }
    parse_any(v)
}

fn has_border_style(v: &str) -> bool {
    [
        "solid", "dashed", "dotted", "double", "groove", "ridge", "inset", "outset",
    ]
    .iter()
    .any(|s| v.contains(s))
}

/// Apply one declaration to a computed style.
fn apply(style: &mut Computed, d: &Declaration, root_font: f32) {
    let v = d.value.trim();
    let fs = style.font_size;
    let cur_color = style.color;
    let p = d.prop.as_str();

    match p {
        "display" => {
            if let Some(dp) = parse_display(v) {
                style.display = dp;
            }
        }
        "color" => {
            if let Some(c) = parse_color_value(v, cur_color) {
                style.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = parse_color_value(v, cur_color) {
                style.background = c;
            }
        }
        "background" => {
            if let Some(c) = parse_color_value(v, cur_color) {
                style.background = c;
            }
        }
        "font-size" => {
            if let Some(k) = font_size_keyword(&v.to_ascii_lowercase(), fs) {
                style.font_size = k.clamp(1.0, 400.0);
            } else {
                match parse_length(v, fs, root_font) {
                    Length::Px(px) => style.font_size = px.clamp(1.0, 400.0),
                    Length::Percent(pc) => style.font_size = (fs * pc / 100.0).clamp(1.0, 400.0),
                    Length::Auto => {}
                }
            }
        }
        "font-family" => {
            // Take the first family in the stack that we can map.
            let first = v
                .split(',')
                .next()
                .unwrap_or("")
                .trim_matches(|c| c == '"' || c == '\'');
            style.family = Family::from_css(first);
            for cand in v.split(',') {
                let c = cand.trim().trim_matches(|c| c == '"' || c == '\'');
                if c.contains("mono") || c.contains("serif") {
                    style.family = Family::from_css(c);
                    break;
                }
            }
        }
        "font-weight" => {
            let lv = v.to_ascii_lowercase();
            style.bold = match lv.as_str() {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                _ => lv.parse::<u32>().map(|n| n >= 600).unwrap_or(false),
            };
        }
        "font-style" => style.italic = v == "italic" || v == "oblique",
        "font" => {
            // Very small subset of the `font` shorthand.
            let lv = v.to_ascii_lowercase();
            if lv.contains("bold") {
                style.bold = true;
            }
            if lv.contains("italic") {
                style.italic = true;
            }
            for tok in v.split_whitespace() {
                if let Some(k) = font_size_keyword(&tok.to_ascii_lowercase(), fs) {
                    style.font_size = k;
                } else if tok.contains('/')
                    || tok.ends_with("px")
                    || tok.ends_with("em")
                    || tok.ends_with("pt")
                    || tok.ends_with('%')
                {
                    let size_tok = tok.split('/').next().unwrap_or(tok);
                    if let Length::Px(px) = parse_length(size_tok, fs, root_font)
                        && px > 0.0
                        && px < 400.0
                    {
                        style.font_size = px;
                    }
                    if let Some((_, lh)) = tok.split_once('/') {
                        style.line_height = parse_line_height(lh, style.font_size, root_font);
                    }
                }
            }
            let first = v.split_whitespace().last().unwrap_or("");
            let first = first.trim_matches(|c| c == '"' || c == '\'');
            if !first.is_empty() {
                style.family = Family::from_css(first);
            }
        }
        "line-height" => {
            style.line_height = parse_line_height(v, style.font_size, root_font);
        }
        "text-align" => {
            if let Some(a) = parse_align(v) {
                style.align = a;
            }
        }
        "text-decoration" | "text-decoration-line" => {
            let lv = v.to_ascii_lowercase();
            style.underline = lv.contains("underline");
            style.line_through = lv.contains("line-through");
        }
        "text-transform" => style.uppercase = v.eq_ignore_ascii_case("uppercase"),
        "white-space" => {
            style.white_space = if v == "pre" || v == "pre-wrap" || v == "break-spaces" {
                WhiteSpace::Pre
            } else {
                WhiteSpace::Normal
            }
        }
        "visibility" => style.visibility_hidden = v == "hidden" || v == "collapse",
        "margin" => style.margin = parse_edges(v, fs, root_font),
        "margin-top" => style.margin.0[0] = parse_length(v, fs, root_font),
        "margin-right" => style.margin.0[1] = parse_length(v, fs, root_font),
        "margin-bottom" => style.margin.0[2] = parse_length(v, fs, root_font),
        "margin-left" => style.margin.0[3] = parse_length(v, fs, root_font),
        "padding" => style.padding = parse_edges(v, fs, root_font),
        "padding-top" => style.padding.0[0] = parse_length(v, fs, root_font),
        "padding-right" => style.padding.0[1] = parse_length(v, fs, root_font),
        "padding-bottom" => style.padding.0[2] = parse_length(v, fs, root_font),
        "padding-left" => style.padding.0[3] = parse_length(v, fs, root_font),
        "width" => style.width = parse_length(v, fs, root_font),
        "height" => style.height = parse_length(v, fs, root_font),
        "border-width" => {
            let e = parse_edges(v, fs, root_font);
            for i in 0..4 {
                style.border_w.0[i] = e.0[i];
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let idx: Option<usize> = match p {
                "border-top" => Some(0),
                "border-right" => Some(1),
                "border-bottom" => Some(2),
                "border-left" => Some(3),
                _ => None,
            };
            let has_style = has_border_style(v);
            for tok in v.split_whitespace() {
                let lt = tok.to_ascii_lowercase();
                if let Some(c) = parse_any(tok) {
                    match idx {
                        Some(i) => style.border_c[i] = c,
                        None => style.border_c = [c; 4],
                    }
                } else if lt.ends_with("px")
                    || lt.ends_with("em")
                    || lt.ends_with("pt")
                    || lt == "thin"
                    || lt == "medium"
                    || lt == "thick"
                    || lt.parse::<f32>().is_ok()
                {
                    let len = parse_length(tok, fs, root_font);
                    match idx {
                        Some(i) => style.border_w.0[i] = len,
                        None => style.border_w = Edges([len; 4]),
                    }
                }
            }
            // A style keyword with no explicit width implies the medium default.
            if has_style {
                match idx {
                    Some(i) => {
                        if let Length::Px(0.0) = style.border_w.0[i] {
                            style.border_w.0[i] = Length::Px(3.0);
                        }
                    }
                    None => {
                        if style
                            .border_w
                            .0
                            .iter()
                            .all(|l| matches!(l, Length::Px(0.0)))
                        {
                            style.border_w = Edges([Length::Px(3.0); 4]);
                        }
                    }
                }
            } else if !has_style && idx.is_none() {
                // `border: 0` / `border: none` clears the border.
                let lv = v.to_ascii_lowercase();
                if lv.trim() == "none" || lv.trim() == "0" || lv.trim().is_empty() {
                    style.border_w = Edges::zero();
                }
            }
        }
        "border-top-width" => style.border_w.0[0] = parse_length(v, fs, root_font),
        "border-right-width" => style.border_w.0[1] = parse_length(v, fs, root_font),
        "border-bottom-width" => style.border_w.0[2] = parse_length(v, fs, root_font),
        "border-left-width" => style.border_w.0[3] = parse_length(v, fs, root_font),
        "border-color" => {
            if let Some(c) = parse_color_value(v, cur_color) {
                style.border_c = [c; 4];
            }
        }
        "border-top-color" => {
            if let Some(c) = parse_color_value(v, cur_color) {
                style.border_c[0] = c;
            }
        }
        "border-right-color" => {
            if let Some(c) = parse_color_value(v, cur_color) {
                style.border_c[1] = c;
            }
        }
        "border-bottom-color" => {
            if let Some(c) = parse_color_value(v, cur_color) {
                style.border_c[2] = c;
            }
        }
        "border-left-color" => {
            if let Some(c) = parse_color_value(v, cur_color) {
                style.border_c[3] = c;
            }
        }
        "border-style"
        | "border-top-style"
        | "border-right-style"
        | "border-bottom-style"
        | "border-left-style" => {
            // Only `none`/`hidden` matter: they remove the border.
            if v == "none" || v == "hidden" {
                match p {
                    "border-top-style" => style.border_w.0[0] = Length::Px(0.0),
                    "border-right-style" => style.border_w.0[1] = Length::Px(0.0),
                    "border-bottom-style" => style.border_w.0[2] = Length::Px(0.0),
                    "border-left-style" => style.border_w.0[3] = Length::Px(0.0),
                    _ => style.border_w = Edges::zero(),
                }
            } else if has_border_style(v) {
                match p {
                    "border-top-style" => {
                        if let Length::Px(0.0) = style.border_w.0[0] {
                            style.border_w.0[0] = Length::Px(3.0)
                        }
                    }
                    "border-right-style" => {
                        if let Length::Px(0.0) = style.border_w.0[1] {
                            style.border_w.0[1] = Length::Px(3.0)
                        }
                    }
                    "border-bottom-style" => {
                        if let Length::Px(0.0) = style.border_w.0[2] {
                            style.border_w.0[2] = Length::Px(3.0)
                        }
                    }
                    "border-left-style" => {
                        if let Length::Px(0.0) = style.border_w.0[3] {
                            style.border_w.0[3] = Length::Px(3.0)
                        }
                    }
                    _ => {
                        if style
                            .border_w
                            .0
                            .iter()
                            .all(|l| matches!(l, Length::Px(0.0)))
                        {
                            style.border_w = Edges([Length::Px(3.0); 4])
                        }
                    }
                }
            }
        }
        "list-style-type" | "list-style" => {
            for tok in v.split_whitespace() {
                if let Some(ls) = parse_list_style(tok) {
                    style.list_style = ls;
                }
            }
        }
        _ => {}
    }
}

fn parse_line_height(v: &str, font_size: f32, root: f32) -> Option<f32> {
    let v = v.trim();
    if v == "normal" || v.is_empty() {
        return None;
    }
    if let Ok(n) = v.parse::<f32>() {
        return Some(n * font_size);
    }
    match parse_length(v, font_size, root) {
        Length::Px(px) => Some(px),
        Length::Percent(p) => Some(font_size * p / 100.0),
        Length::Auto => None,
    }
}

/// Compute the used style for an element given its parent's style.
pub fn compute(el: ElementRef, parent: &Computed, sheets: &[Stylesheet]) -> Computed {
    let mut style = Computed::inherit(parent);

    // Gather every matching declaration with its cascade key.
    #[derive(Clone)]
    struct Cand {
        important: bool,
        spec: crate::css::Specificity,
        order: usize,
        decl: Declaration,
    }
    let mut cands: Vec<Cand> = Vec::new();

    for sheet in sheets {
        for rule in &sheet.rules {
            let mut best: Option<(crate::css::Specificity, usize)> = None;
            for cs in &rule.selectors {
                if cs.sel.matches(&el) {
                    let key = (cs.spec, cs.order);
                    if best.map(|b| key > b).unwrap_or(true) {
                        best = Some(key);
                    }
                }
            }
            if let Some((spec, order)) = best {
                for d in &rule.decls {
                    cands.push(Cand {
                        important: d.important,
                        spec,
                        order,
                        decl: d.clone(),
                    });
                }
            }
        }
    }

    // Inline style attribute wins over everything except `!important` rules.
    if let Some(attr) = el.attr("style") {
        for d in crate::css::parse_inline_style(attr) {
            cands.push(Cand {
                important: d.important,
                spec: crate::css::Specificity(1, 0, 0),
                order: usize::MAX - 1,
                decl: d,
            });
        }
    }

    cands.sort_by(|a, b| {
        a.important
            .cmp(&b.important)
            .then(a.spec.cmp(&b.spec))
            .then(a.order.cmp(&b.order))
    });

    let root_font = parent.font_size.max(1.0);

    // Two passes: font properties first so `em` units elsewhere resolve against
    // this element's own font size, as the cascade requires.
    for c in cands.iter().filter(|c| {
        matches!(
            c.decl.prop.as_str(),
            "font-size" | "font-family" | "font-weight" | "font-style" | "font"
        )
    }) {
        apply(&mut style, &c.decl, root_font);
    }
    for c in cands.iter().filter(|c| {
        !matches!(
            c.decl.prop.as_str(),
            "font-size" | "font-family" | "font-weight" | "font-style" | "font"
        )
    }) {
        apply(&mut style, &c.decl, root_font);
    }

    // Presentation attributes act as zero-specificity hints.
    apply_presentational(&mut style, el);

    style
}

fn apply_presentational(style: &mut Computed, el: ElementRef) {
    let name = el.value().name();
    match name {
        "b" | "strong" | "th" => {
            if !style.bold {
                style.bold = true;
            }
        }
        "i" | "em" => style.italic = true,
        "tt" | "code" | "kbd" | "samp" | "pre" => {
            if style.family == Family::Sans {
                style.family = Family::Mono;
            }
        }
        "u" | "ins" => style.underline = true,
        "s" | "strike" | "del" => style.line_through = true,
        "center" => style.align = Align::Center,
        "font" => {
            if let Some(color) = el.attr("color").and_then(parse_any) {
                style.color = color;
            }
            if let Some(size) = el.attr("size")
                && let Ok(n) = size.trim().parse::<i32>()
            {
                style.font_size = (8.0 + n as f32 * 3.0).clamp(6.0, 60.0);
            }
            if let Some(face) = el.attr("face") {
                style.family = Family::from_css(face);
            }
        }
        "td" | "table" | "tr" => {
            if let Some(bg) = el.attr("bgcolor").and_then(parse_any) {
                style.background = bg;
            }
            if let Some(al) = el.attr("align").and_then(parse_align) {
                style.align = al;
            }
        }
        _ => {}
    }
    if let Some(bg) = el.attr("background-color").and_then(parse_any) {
        style.background = bg;
    }
    if el.attr("align").is_some()
        && matches!(
            name,
            "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "img"
        )
        && let Some(al) = el.attr("align").and_then(parse_align)
    {
        style.align = al;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths() {
        assert!(
            matches!(parse_length("12px", 16.0, 16.0), Length::Px(v) if (v - 12.0).abs() < 0.01)
        );
        assert!(
            matches!(parse_length("2em", 16.0, 16.0), Length::Px(v) if (v - 32.0).abs() < 0.01)
        );
        assert!(
            matches!(parse_length("50%", 16.0, 16.0), Length::Percent(v) if (v - 50.0).abs() < 0.01)
        );
        assert!(
            matches!(parse_length("1rem", 20.0, 16.0), Length::Px(v) if (v - 16.0).abs() < 0.01)
        );
        assert!(matches!(parse_length("auto", 16.0, 16.0), Length::Auto));
        assert!(
            matches!(parse_length("12pt", 16.0, 16.0), Length::Px(v) if (v - 16.0).abs() < 0.01)
        );
    }

    #[test]
    fn edge_shorthands() {
        let e = parse_edges("10px", 16.0, 16.0);
        assert!(matches!(e.0[2], Length::Px(v) if v == 10.0));
        let e = parse_edges("1px 2px", 16.0, 16.0);
        assert!(matches!((e.0[0], e.0[2]), (Length::Px(a), Length::Px(c)) if a == 1.0 && c == 1.0));
        assert!(matches!(e.0[1], Length::Px(v) if v == 2.0));
        let e = parse_edges("1px 2px 3px 4px", 16.0, 16.0);
        let vals: Vec<f32> =
            e.0.iter()
                .map(|l| match l {
                    Length::Px(v) => *v,
                    _ => -1.0,
                })
                .collect();
        assert_eq!(vals, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn inheritance_carries_font_and_color() {
        let mut p = Computed::initial();
        p.color = Color::rgb(9, 9, 9);
        p.font_size = 20.0;
        let c = Computed::inherit(&p);
        assert_eq!(c.color, Color::rgb(9, 9, 9));
        assert_eq!(c.font_size, 20.0);
        // non-inherited properties reset
        assert!(c.background.is_transparent());
        assert!(matches!(c.margin.0[0], Length::Px(0.0)));
    }
}
