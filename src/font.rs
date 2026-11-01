//! Font loading, text measurement and glyph rasterization.
//!
//! Fonts are embedded in the binary via `include_bytes!` so the browser is
//! self-contained and does not depend on system fontconfig.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

impl Family {
    pub fn from_css(name: &str) -> Family {
        let n = name.to_ascii_lowercase();
        for pat in ["mono", "courier", "consolas", "menlo"] {
            if n.contains(pat) {
                return Family::Mono;
            }
        }
        for pat in ["serif", "georgia", "times", "garamond", "palatino"] {
            if n.contains(pat) && !n.contains("sans") {
                return Family::Serif;
            }
        }
        Family::Sans
    }
}

/// Everything `blit_char` needs beyond geometry, kept together so the
/// rasterizer call sites stay readable.
#[derive(Clone, Copy, Debug)]
pub struct BlitStyle {
    pub size: f32,
    pub fam: Family,
    pub bold: bool,
    pub rgb: (u8, u8, u8),
    pub alpha: u8,
}

impl Default for FontDb {
    fn default() -> Self {
        FontDb::new()
    }
}

pub struct FontDb {
    sans: FontRef<'static>,
    sans_b: FontRef<'static>,
    serif: FontRef<'static>,
    serif_b: FontRef<'static>,
    mono: FontRef<'static>,
    mono_b: FontRef<'static>,
}

// FontRef<'static> only borrows embedded &'static [u8] slices, so sharing the
// database across threads is sound.
unsafe impl Sync for FontDb {}
unsafe impl Send for FontDb {}

impl FontDb {
    pub fn new() -> FontDb {
        let load = |bytes: &'static [u8]| {
            FontRef::try_from_slice(bytes).expect("embedded font must be valid")
        };
        FontDb {
            sans: load(include_bytes!("../assets/fonts/DejaVuSans.ttf")),
            sans_b: load(include_bytes!("../assets/fonts/DejaVuSans-Bold.ttf")),
            serif: load(include_bytes!("../assets/fonts/DejaVuSerif.ttf")),
            serif_b: load(include_bytes!("../assets/fonts/DejaVuSerif-Bold.ttf")),
            mono: load(include_bytes!("../assets/fonts/DejaVuSansMono.ttf")),
            mono_b: load(include_bytes!("../assets/fonts/DejaVuSansMono-Bold.ttf")),
        }
    }

    pub fn face(&self, fam: Family, bold: bool) -> &FontRef<'static> {
        match (fam, bold) {
            (Family::Sans, false) => &self.sans,
            (Family::Sans, true) => &self.sans_b,
            (Family::Serif, false) => &self.serif,
            (Family::Serif, true) => &self.serif_b,
            (Family::Mono, false) => &self.mono,
            (Family::Mono, true) => &self.mono_b,
        }
    }

    /// Ascent / descent / line-gap in px for a given font size.
    pub fn metrics(&self, size: f32, fam: Family, bold: bool) -> (f32, f32, f32) {
        let f = self.face(fam, bold).as_scaled(PxScale::from(size.max(1.0)));
        (f.ascent(), f.descent().abs(), f.line_gap())
    }

    /// Width of a single character, including kerning against the previous one.
    fn char_advance(
        &self,
        ch: char,
        prev: Option<char>,
        size: f32,
        fam: Family,
        bold: bool,
    ) -> f32 {
        let font = self.face(fam, bold);
        let scaled = font.as_scaled(PxScale::from(size.max(1.0)));
        let gid = font.glyph_id(ch);
        let mut adv = scaled.h_advance(gid);
        if let Some(p) = prev {
            adv += scaled.kern(font.glyph_id(p), gid);
        }
        adv
    }

    /// Total advance width of a string in px.
    pub fn measure(&self, s: &str, size: f32, fam: Family, bold: bool) -> f32 {
        let mut w = 0.0;
        let mut prev = None;
        for ch in s.chars() {
            w += self.char_advance(ch, prev, size, fam, bold);
            prev = Some(ch);
        }
        w
    }

    /// Width of the space character, used for line breaking.
    pub fn space_width(&self, size: f32, fam: Family, bold: bool) -> f32 {
        let font = self.face(fam, bold);
        font.as_scaled(PxScale::from(size.max(1.0)))
            .h_advance(font.glyph_id(' '))
    }

    /// Fallback width for characters the font lacks a glyph for.
    pub fn has_glyph(&self, ch: char, fam: Family, bold: bool) -> bool {
        self.face(fam, bold).glyph_id(ch) != ab_glyph::GlyphId(0)
    }

    /// Rasterize one character, blending coverage into an RGBA premultiplied
    /// pixel buffer. `x`/`y` is the pen position (y = baseline).
    pub fn blit_char(
        &self,
        buf: &mut [u8],
        dims: (usize, usize),
        ch: char,
        pos: (f32, f32),
        st: &BlitStyle,
    ) -> f32 {
        let (buf_w, buf_h) = dims;
        let (x, baseline_y) = pos;
        let size = st.size;
        let fam = st.fam;
        let bold = st.bold;
        let font = self.face(fam, bold);
        let scale = PxScale::from(size.max(1.0));

        // Fall back to the sans face when the requested family lacks the glyph,
        // so CJK / symbol text still renders instead of turning into blanks.
        let font = if self.has_glyph(ch, fam, bold) {
            font
        } else if self.has_glyph(ch, Family::Sans, bold) {
            self.face(Family::Sans, bold)
        } else {
            self.face(Family::Sans, false)
        };

        let glyph = font
            .glyph_id(ch)
            .with_scale_and_position(scale, point(x, baseline_y));

        let advance = font.as_scaled(scale).h_advance(glyph.id);

        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            // `draw` yields coordinates relative to px_bounds.min, so the bound
            // origin must be added back to place the glyph absolutely.
            let ox = bounds.min.x;
            let oy = bounds.min.y;
            let (r, g, b) = st.rgb;
            outlined.draw(|gx, gy, cov| {
                if cov <= 0.001 {
                    return;
                }
                let px = ox + gx as f32;
                let py = oy + gy as f32;
                let ix = px.round() as i64;
                let iy = py.round() as i64;
                if ix < 0 || iy < 0 || ix >= buf_w as i64 || iy >= buf_h as i64 {
                    return;
                }
                let a = (cov.min(1.0) * st.alpha as f32) as u32;
                if a == 0 {
                    return;
                }
                let idx = ((iy as usize) * buf_w + (ix as usize)) * 4;
                // Source-over with an opaque source at coverage `a`.
                let inv = 255 - a;
                buf[idx] = ((r as u32 * a + buf[idx] as u32 * inv) / 255) as u8;
                buf[idx + 1] = ((g as u32 * a + buf[idx + 1] as u32 * inv) / 255) as u8;
                buf[idx + 2] = ((b as u32 * a + buf[idx + 2] as u32 * inv) / 255) as u8;
                buf[idx + 3] = (a + (buf[idx + 3] as u32 * inv) / 255).min(255) as u8;
            });
        }
        advance
    }
}

pub fn fonts() -> &'static FontDb {
    static DB: std::sync::OnceLock<FontDb> = std::sync::OnceLock::new();
    DB.get_or_init(FontDb::new)
}
