//! Paint: turn the display list into pixels with tiny-skia, plus glyph
//! rasterization from ab_glyph. This is the compositor step of the pipeline.

use std::collections::HashMap;

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Rect, Transform};

use crate::color::Color;
use crate::layout::{Cmd, DecodedImage, LayoutOutput, MarkerKind};

/// Hard cap on rendered page height. A full-page canvas is w*h*4 bytes, so this
/// bounds memory per render (1280 x 12000 x 4 = ~61 MB worst case).
pub const MAX_PAGE_HEIGHT: u32 = 12_000;

pub struct Painted {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub clipped: bool,
    /// The rasterized page itself, for consumers (the native window) that want
    /// pixels rather than an encoded PNG.
    pub pixmap: Option<Pixmap>,
}

/// The non-geometric half of a text paint command.
#[derive(Clone, Copy, Debug)]
struct TextSpec {
    size: f32,
    fam: crate::font::Family,
    bold: bool,
    c: Color,
    underline: bool,
    strike: bool,
}

pub struct Painter<'a> {
    out: &'a LayoutOutput,
    pm: Pixmap,
    img_cache: HashMap<usize, Pixmap>,
}

impl<'a> Painter<'a> {
    pub fn new(out: &'a LayoutOutput, viewport_w: u32) -> Option<Painter<'a>> {
        let w = viewport_w.max(1);
        let want_h = out.content_height.ceil().max(1.0) as u32;
        let h = want_h.clamp(1, MAX_PAGE_HEIGHT);
        let pm = Pixmap::new(w, h)?;
        Some(Painter {
            out,
            pm,
            img_cache: HashMap::new(),
        })
    }

    pub fn clipped(&self) -> bool {
        (self.out.content_height.ceil() as u32) > self.pm.height()
    }

    fn ts_color(c: Color) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(c.r, c.g, c.b, (c.a.clamp(0.0, 1.0) * 255.0) as u8)
    }

    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: Color) {
        if c.is_transparent() || w <= 0.0 || h <= 0.0 {
            return;
        }
        let mut paint = Paint::default();
        paint.set_color(Self::ts_color(c));
        paint.anti_alias = true;
        if let Some(r) = Rect::from_xywh(x, y, w, h) {
            self.pm.fill_rect(r, &paint, Transform::identity(), None);
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, c: Color, filled: bool) {
        if r <= 0.0 || c.is_transparent() {
            return;
        }
        // Four cubic arcs approximate a circle to well under a pixel of error.
        let k = r * 0.552_284_8;
        let mut pb = PathBuilder::new();
        pb.move_to(cx - r, cy);
        pb.cubic_to(cx - r, cy - k, cx - k, cy - r, cx, cy - r);
        pb.cubic_to(cx + k, cy - r, cx + r, cy - k, cx + r, cy);
        pb.cubic_to(cx + r, cy + k, cx + k, cy + r, cx, cy + r);
        pb.cubic_to(cx - k, cy + r, cx - r, cy + k, cx - r, cy);
        pb.close();
        let path = match pb.finish() {
            Some(p) => p,
            None => return,
        };
        let mut paint = Paint::default();
        paint.set_color(Self::ts_color(c));
        paint.anti_alias = true;
        if filled {
            self.pm.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        } else {
            let stroke = tiny_skia::Stroke {
                width: (r * 0.5).max(0.8),
                ..tiny_skia::Stroke::default()
            };
            self.pm
                .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }

    /// Convert a decoded RGBA image into a premultiplied pixmap, cached by index.
    fn image_pixmap(&mut self, idx: usize) -> Option<Pixmap> {
        if let Some(pm) = self.img_cache.get(&idx) {
            return Some(pm.clone());
        }
        let img: &DecodedImage = self.out.images.get(idx)?;
        let (w, h) = (img.w, img.h);
        if w == 0 || h == 0 {
            return None;
        }
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for px in img.rgba.as_chunks::<4>().0 {
            let (r, g, b, a) = (px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32);
            data.push((r * a / 255) as u8);
            data.push((g * a / 255) as u8);
            data.push((b * a / 255) as u8);
            data.push(a as u8);
        }
        let pm = Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h)?)?;
        self.img_cache.insert(idx, pm.clone());
        Some(pm)
    }

    fn draw_image(&mut self, idx: usize, x: f32, y: f32, w: f32, h: f32) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let pm = match self.image_pixmap(idx) {
            Some(pm) => pm,
            None => {
                // Broken-image placeholder, as a browser would draw.
                self.fill_rect(x, y, w, h, Color::rgb(238, 238, 238));
                let mut paint = Paint::default();
                paint.set_color(Self::ts_color(Color::rgb(170, 170, 170)));
                let mut pb = PathBuilder::new();
                pb.move_to(x, y);
                pb.line_to(x + w, y + h);
                pb.move_to(x + w, y);
                pb.line_to(x, y + h);
                if let Some(path) = pb.finish() {
                    let stroke = tiny_skia::Stroke {
                        width: 1.0,
                        ..tiny_skia::Stroke::default()
                    };
                    self.pm
                        .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                }
                return;
            }
        };
        let sx = w / pm.width() as f32;
        let sy = h / pm.height() as f32;
        let transform = Transform::from_scale(sx, sy);
        let pp = PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..PixmapPaint::default()
        };
        self.pm
            .draw_pixmap(x as i32, y as i32, pm.as_ref(), &pp, transform, None);
    }

    fn draw_text(&mut self, x: f32, baseline: f32, s: &str, spec: &TextSpec) {
        let TextSpec {
            size,
            fam,
            bold,
            c,
            underline,
            strike,
        } = *spec;
        if s.is_empty() || c.is_transparent() {
            return;
        }
        let db = crate::font::fonts();
        let (pw, ph) = (self.pm.width() as usize, self.pm.height() as usize);

        // Cheap reject: nothing to do if the whole run is off-canvas.
        if baseline < -size * 2.0 || (baseline - size * 2.0) > ph as f32 {
            return;
        }

        {
            let buf = self.pm.data_mut();
            let bs = crate::font::BlitStyle {
                size,
                fam,
                bold,
                rgb: c.rgb_bytes(),
                alpha: (c.a.clamp(0.0, 1.0) * 255.0) as u8,
            };
            let mut pen = x;
            for ch in s.chars() {
                if pen > pw as f32 {
                    break;
                }
                pen += db.blit_char(buf, (pw, ph), ch, (pen, baseline), &bs);
            }
        }

        if underline || strike {
            let w = db.measure(s, size, fam, bold);
            let th = (size * 0.07).max(1.0);
            if underline {
                self.fill_rect(x, baseline + size * 0.10, w, th, c);
            }
            if strike {
                self.fill_rect(x, baseline - size * 0.30, w, th, c);
            }
        }
    }

    pub fn paint(&mut self) -> Painted {
        // Canvas background first.
        self.fill_rect(
            0.0,
            0.0,
            self.pm.width() as f32,
            self.pm.height() as f32,
            self.out.canvas_bg,
        );

        let h = self.pm.height() as f32;
        for cmd in &self.out.cmds {
            match cmd {
                Cmd::Rect { x, y, w, h: hh, c } => {
                    if *y > h || *y + *hh < 0.0 {
                        continue;
                    }
                    self.fill_rect(*x, *y, *w, *hh, *c);
                }
                Cmd::Borders {
                    x,
                    y,
                    w,
                    h: hh,
                    widths,
                    colors,
                } => {
                    if *y > h || *y + *hh < 0.0 {
                        continue;
                    }
                    let (t, r, b, l) = (widths[0], widths[1], widths[2], widths[3]);
                    if t > 0.0 {
                        self.fill_rect(*x, *y, *w, t, colors[0]);
                    }
                    if b > 0.0 {
                        self.fill_rect(*x, *y + *hh - b, *w, b, colors[2]);
                    }
                    if l > 0.0 {
                        self.fill_rect(*x, *y, l, *hh, colors[3]);
                    }
                    if r > 0.0 {
                        self.fill_rect(*x + *w - r, *y, r, *hh, colors[1]);
                    }
                }
                Cmd::Text {
                    x,
                    y,
                    s,
                    size,
                    fam,
                    bold,
                    c,
                    underline,
                    strike,
                    ..
                } => {
                    if *y < -50.0 || *y > h + 50.0 {
                        continue;
                    }
                    let spec = TextSpec {
                        size: *size,
                        fam: *fam,
                        bold: *bold,
                        c: *c,
                        underline: *underline,
                        strike: *strike,
                    };
                    self.draw_text(*x, *y, s, &spec);
                }
                Cmd::Image {
                    x,
                    y,
                    w,
                    h: hh,
                    idx,
                } => {
                    if *y > h || *y + *hh < 0.0 {
                        continue;
                    }
                    self.draw_image(*idx, *x, *y, *w, *hh);
                }
                Cmd::Marker { cx, cy, r, c, kind } => {
                    if *cy > h + 10.0 || *cy < -10.0 {
                        continue;
                    }
                    self.circle(
                        *cx,
                        *cy,
                        *r,
                        *c,
                        *kind == MarkerKind::Disc || *kind == MarkerKind::Square,
                    );
                }
            }
        }

        let png = self.pm.encode_png().unwrap_or_default();
        let (width, height) = (self.pm.width(), self.pm.height());
        let clipped = self.clipped();
        Painted {
            png,
            width,
            height,
            clipped,
            pixmap: Some(self.pm.clone()),
        }
    }
}

/// Convenience wrapper: display list -> PNG bytes.
pub fn render_to_png(out: &LayoutOutput, viewport_w: u32) -> Option<Painted> {
    let mut p = Painter::new(out, viewport_w)?;
    Some(p.paint())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::Stylesheet;
    use crate::layout::Layout;
    use scraper::Html;
    use url::Url;

    fn layout_of(html: &str) -> LayoutOutput {
        let doc = Html::parse_document(html);
        let css: String = doc
            .select(&crate::sel("style"))
            .map(|s| s.text().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        let (ua, order) = Stylesheet::parse(crate::style::UA_CSS, 0);
        let (author, _) = Stylesheet::parse(&css, order);
        let sheets = vec![ua, author];
        let base = Url::parse("https://example.com/").unwrap();
        // Leak the doc/sheets so LayoutOutput outlives them in this test helper.
        let doc: &'static Html = Box::leak(Box::new(doc));
        let sheets: &'static [Stylesheet] = Box::leak(sheets.into_boxed_slice());
        let mut lay = Layout::new(doc, sheets, base, 400.0, false);
        lay.run()
    }

    #[test]
    fn renders_a_nonempty_png() {
        let out = layout_of("<html><body><h1 style='color:#123456'>Hello</h1></body></html>");
        let painted = render_to_png(&out, 400).expect("pixmap allocation");
        assert!(!painted.png.is_empty());
        assert_eq!(painted.width, 400);
        assert!(painted.height > 10);
        // PNG magic bytes
        assert_eq!(
            &painted.png[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn text_actually_changes_pixels() {
        let blank = layout_of("<html><body><p>&nbsp;</p></body></html>");
        let b = render_to_png(&blank, 300).unwrap();
        let text = layout_of("<html><body><p>XXXX gypq 0123</p></body></html>");
        let t = render_to_png(&text, 300).unwrap();
        assert_ne!(
            b.png.len(),
            t.png.len(),
            "text should alter the encoded output"
        );
    }

    #[test]
    fn height_is_capped() {
        // A pathological page must not allocate an unbounded canvas.
        let out = layout_of(&format!(
            "<html><body>{}</body></html>",
            "<div style='height:2000px'>x</div>".repeat(50)
        ));
        let p = render_to_png(&out, 200).unwrap();
        assert!(p.height <= MAX_PAGE_HEIGHT);
        assert!(p.clipped, "50 x 2000px should exceed the cap");
    }

    #[test]
    fn transparent_rect_is_a_no_op() {
        let out = layout_of("<html><body>a</body></html>");
        let mut p = Painter::new(&out, 10).unwrap();
        p.fill_rect(0.0, 0.0, 10.0, 10.0, Color::TRANSPARENT);
        p.circle(5.0, 5.0, 0.0, Color::BLACK, true);
        p.circle(5.0, 5.0, 3.0, Color::TRANSPARENT, false);
        let out = p.paint();
        assert!(!out.png.is_empty());
    }
}
