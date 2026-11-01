//! CSS color parsing: named colors, `#rgb`, `#rrggbb`, `rgb()`, `rgba()`,
//! `hsl()`, and the `transparent` keyword.

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    /// 0.0 = fully transparent, 1.0 = opaque
    pub a: f32,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 1.0 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
        Color { r, g, b, a }
    }
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0.0,
    };

    pub fn is_transparent(&self) -> bool {
        self.a <= 0.003
    }

    pub fn rgb_bytes(&self) -> (u8, u8, u8) {
        (self.r, self.g, self.b)
    }

    pub fn to_css(&self) -> String {
        if self.a >= 0.999 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("rgba({},{},{},{:.3})", self.r, self.g, self.b, self.a)
        }
    }

    /// Compositing helper: blend `self` over `bg`.
    pub fn over(&self, bg: Color) -> Color {
        if self.a >= 0.999 {
            return *self;
        }
        if self.is_transparent() {
            return bg;
        }
        let a = self.a * bg.a;
        if a <= 0.0 {
            return Color::TRANSPARENT;
        }
        let mix = |c1: u8, c2: u8| -> u8 {
            let v = (c1 as f32 * self.a + c2 as f32 * bg.a * (1.0 - self.a)) / a;
            v.clamp(0.0, 255.0) as u8
        };
        Color {
            r: mix(self.r, bg.r),
            g: mix(self.g, bg.g),
            b: mix(self.b, bg.b),
            a: a.min(1.0),
        }
    }
}

pub fn parse_color(s: &str) -> Option<Color> {
    let t = s.trim().to_ascii_lowercase();
    if t.is_empty() {
        return None;
    }
    if t == "transparent" {
        return Some(Color::TRANSPARENT);
    }
    if t == "currentcolor" {
        return None;
    }

    if let Some(hex) = t.strip_prefix('#') {
        let h: Vec<char> = hex.chars().collect();
        let (r, g, b, a) = match h.len() {
            3 => {
                let d = |c: char| -> Option<u32> { c.to_digit(16) };
                (
                    (d(h[0])? * 17) as u8,
                    (d(h[1])? * 17) as u8,
                    (d(h[2])? * 17) as u8,
                    1.0,
                )
            }
            4 => {
                let d = |c: char| -> Option<u32> { c.to_digit(16) };
                (
                    (d(h[0])? * 17) as u8,
                    (d(h[1])? * 17) as u8,
                    (d(h[2])? * 17) as u8,
                    (d(h[3])? * 17) as f32 / 255.0,
                )
            }
            6 | 8 => {
                let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
                (
                    byte(0)?,
                    byte(2)?,
                    byte(4)?,
                    if h.len() == 8 {
                        byte(6)? as f32 / 255.0
                    } else {
                        1.0
                    },
                )
            }
            _ => return None,
        };
        return Some(Color::rgba(r, g, b, a));
    }

    // Functional notation: rgb()/rgba()/hsl()/hsla()
    let open = t.find('(')?;
    let close = t.rfind(')')?;
    if close <= open {
        return None;
    }
    let fn_name = t[..open].trim();
    let inner = &t[open + 1..close];
    // Legacy comma syntax `rgb(1,2,3)` and modern space syntax `rgb(1 2 3 / 50%)`.
    let (main_part, alpha_part) = match inner.split_once('/') {
        Some((m, a)) => (m, Some(a.trim())),
        None => (inner, None),
    };
    let mut parts: Vec<&str> = if main_part.contains(',') {
        main_part
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect()
    } else {
        main_part.split_whitespace().collect()
    };
    if let Some(a) = alpha_part
        && !a.is_empty()
    {
        parts.push(a);
    }

    let num = |p: &str| -> Option<f32> {
        let p = p.trim();
        if let Some(pct) = p.strip_suffix('%') {
            pct.trim().parse::<f32>().ok().map(|v| v * 2.55)
        } else {
            p.parse::<f32>().ok()
        }
    };
    let alpha_of = |p: Option<&&str>| -> f32 {
        match p {
            Some(s) => {
                let s = s.trim();
                if let Some(pct) = s.strip_suffix('%') {
                    pct.trim().parse::<f32>().map(|v| v / 100.0).unwrap_or(1.0)
                } else {
                    s.parse::<f32>().unwrap_or(1.0)
                }
            }
            None => 1.0,
        }
    };
    let clamp8 = |v: f32| (v.round().clamp(0.0, 255.0)) as u8;

    match fn_name {
        "rgb" | "rgba" => {
            if parts.len() < 3 {
                return None;
            }
            Some(Color::rgba(
                clamp8(num(parts[0])?),
                clamp8(num(parts[1])?),
                clamp8(num(parts[2])?),
                alpha_of(parts.get(3)).clamp(0.0, 1.0),
            ))
        }
        "hsl" | "hsla" => {
            if parts.len() < 3 {
                return None;
            }
            let h = parts[0]
                .trim()
                .trim_end_matches("deg")
                .parse::<f32>()
                .ok()?;
            let s = num(parts[1])? / 2.55;
            let l = num(parts[2])? / 2.55;
            let (r, g, b) = hsl_to_rgb(h, s / 100.0, l / 100.0);
            Some(Color::rgba(r, g, b, alpha_of(parts.get(3)).clamp(0.0, 1.0)))
        }
        _ => None,
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    if s <= 0.0 {
        let v = (l * 255.0).round() as u8;
        return (v, v, v);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let f = |t: f32| -> f32 {
        let mut t = t;
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 1.0 / 2.0 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (
        (f(h + 1.0 / 3.0) * 255.0).round() as u8,
        (f(h) * 255.0).round() as u8,
        (f(h - 1.0 / 3.0) * 255.0).round() as u8,
    )
}

/// Look up a CSS named color.
pub fn named(name: &str) -> Option<Color> {
    let n = name.trim().to_ascii_lowercase();
    let idx = NAMED.binary_search_by_key(&n.as_str(), |e| e.0).ok()?;
    let (_, r, g, b) = NAMED[idx];
    Some(Color::rgb(r, g, b))
}

/// Parse a color from any supported syntax (named included).
pub fn parse_any(s: &str) -> Option<Color> {
    parse_color(s).or_else(|| named(s))
}

/// CSS named colors, sorted alphabetically for binary search.
static NAMED: &[(&str, u8, u8, u8)] = &[
    ("aliceblue", 240, 248, 255),
    ("antiquewhite", 250, 235, 215),
    ("aqua", 0, 255, 255),
    ("aquamarine", 127, 255, 212),
    ("azure", 240, 255, 255),
    ("beige", 245, 245, 220),
    ("bisque", 255, 228, 196),
    ("black", 0, 0, 0),
    ("blanchedalmond", 255, 235, 205),
    ("blue", 0, 0, 255),
    ("blueviolet", 138, 43, 226),
    ("brown", 165, 42, 42),
    ("burlywood", 222, 184, 135),
    ("cadetblue", 95, 158, 160),
    ("chartreuse", 127, 255, 0),
    ("chocolate", 210, 105, 30),
    ("coral", 255, 127, 80),
    ("cornflowerblue", 100, 149, 237),
    ("cornsilk", 255, 248, 220),
    ("crimson", 220, 20, 60),
    ("cyan", 0, 255, 255),
    ("darkblue", 0, 0, 139),
    ("darkcyan", 0, 139, 139),
    ("darkgoldenrod", 184, 134, 11),
    ("darkgray", 169, 169, 169),
    ("darkgreen", 0, 100, 0),
    ("darkgrey", 169, 169, 169),
    ("darkkhaki", 189, 183, 107),
    ("darkmagenta", 139, 0, 139),
    ("darkolivegreen", 85, 107, 47),
    ("darkorange", 255, 140, 0),
    ("darkorchid", 153, 50, 204),
    ("darkred", 139, 0, 0),
    ("darksalmon", 233, 150, 122),
    ("darkseagreen", 143, 188, 143),
    ("darkslateblue", 72, 61, 139),
    ("darkslategray", 47, 79, 79),
    ("darkslategrey", 47, 79, 79),
    ("darkturquoise", 0, 206, 209),
    ("darkviolet", 148, 0, 211),
    ("deeppink", 255, 20, 147),
    ("deepskyblue", 0, 191, 255),
    ("dimgray", 105, 105, 105),
    ("dimgrey", 105, 105, 105),
    ("dodgerblue", 30, 144, 255),
    ("firebrick", 178, 34, 34),
    ("floralwhite", 255, 250, 240),
    ("forestgreen", 34, 139, 34),
    ("fuchsia", 255, 0, 255),
    ("gainsboro", 220, 220, 220),
    ("ghostwhite", 248, 248, 255),
    ("gold", 255, 215, 0),
    ("goldenrod", 218, 165, 32),
    ("gray", 128, 128, 128),
    ("green", 0, 128, 0),
    ("greenyellow", 173, 255, 47),
    ("grey", 128, 128, 128),
    ("honeydew", 240, 255, 240),
    ("hotpink", 255, 105, 180),
    ("indianred", 205, 92, 92),
    ("indigo", 75, 0, 130),
    ("ivory", 255, 255, 240),
    ("khaki", 240, 230, 140),
    ("lavender", 230, 230, 250),
    ("lavenderblush", 255, 240, 245),
    ("lawngreen", 124, 252, 0),
    ("lemonchiffon", 255, 250, 205),
    ("lightblue", 173, 216, 230),
    ("lightcoral", 240, 128, 128),
    ("lightcyan", 224, 255, 255),
    ("lightgoldenrodyellow", 250, 250, 210),
    ("lightgray", 211, 211, 211),
    ("lightgreen", 144, 238, 144),
    ("lightgrey", 211, 211, 211),
    ("lightpink", 255, 182, 193),
    ("lightsalmon", 255, 160, 122),
    ("lightseagreen", 32, 178, 170),
    ("lightskyblue", 135, 206, 250),
    ("lightslategray", 119, 136, 153),
    ("lightslategrey", 119, 136, 153),
    ("lightsteelblue", 176, 196, 222),
    ("lightyellow", 255, 255, 224),
    ("lime", 0, 255, 0),
    ("limegreen", 50, 205, 50),
    ("linen", 250, 240, 230),
    ("magenta", 255, 0, 255),
    ("maroon", 128, 0, 0),
    ("mediumaquamarine", 102, 205, 170),
    ("mediumblue", 0, 0, 205),
    ("mediumorchid", 186, 85, 211),
    ("mediumpurple", 147, 112, 219),
    ("mediumseagreen", 60, 179, 113),
    ("mediumslateblue", 123, 104, 238),
    ("mediumspringgreen", 0, 250, 154),
    ("mediumturquoise", 72, 209, 204),
    ("mediumvioletred", 199, 21, 133),
    ("midnightblue", 25, 25, 112),
    ("mintcream", 245, 255, 250),
    ("mistyrose", 255, 228, 225),
    ("moccasin", 255, 228, 181),
    ("navajowhite", 255, 222, 173),
    ("navy", 0, 0, 128),
    ("oldlace", 253, 245, 230),
    ("olive", 128, 128, 0),
    ("olivedrab", 107, 142, 35),
    ("orange", 255, 165, 0),
    ("orangered", 255, 69, 0),
    ("orchid", 218, 112, 214),
    ("palegoldenrod", 238, 232, 170),
    ("palegreen", 152, 251, 152),
    ("paleturquoise", 175, 238, 238),
    ("palevioletred", 219, 112, 147),
    ("papayawhip", 255, 239, 213),
    ("peachpuff", 255, 218, 185),
    ("peru", 205, 133, 63),
    ("pink", 255, 192, 203),
    ("plum", 221, 160, 221),
    ("powderblue", 176, 224, 230),
    ("purple", 128, 0, 128),
    ("rebeccapurple", 102, 51, 153),
    ("red", 255, 0, 0),
    ("rosybrown", 188, 143, 143),
    ("royalblue", 65, 105, 225),
    ("saddlebrown", 139, 69, 19),
    ("salmon", 250, 128, 114),
    ("sandybrown", 244, 164, 96),
    ("seagreen", 46, 139, 87),
    ("seashell", 255, 245, 238),
    ("sienna", 160, 82, 45),
    ("silver", 192, 192, 192),
    ("skyblue", 135, 206, 235),
    ("slateblue", 106, 90, 205),
    ("slategray", 112, 128, 144),
    ("slategrey", 112, 128, 144),
    ("snow", 255, 250, 250),
    ("springgreen", 0, 255, 127),
    ("steelblue", 70, 130, 180),
    ("tan", 210, 180, 140),
    ("teal", 0, 128, 128),
    ("thistle", 216, 191, 216),
    ("tomato", 255, 99, 71),
    ("turquoise", 64, 224, 208),
    ("violet", 238, 130, 238),
    ("wheat", 245, 222, 179),
    ("white", 255, 255, 255),
    ("whitesmoke", 245, 245, 245),
    ("yellow", 255, 255, 0),
    ("yellowgreen", 154, 206, 50),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_forms() {
        assert_eq!(parse_color("#fff"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(parse_color("#0a0b0c"), Some(Color::rgb(10, 11, 12)));
        assert_eq!(
            parse_color("#FF000080"),
            Some(Color::rgba(255, 0, 0, 128.0 / 255.0))
        );
    }

    #[test]
    fn functional_forms() {
        assert_eq!(parse_color("rgb(1,2,3)"), Some(Color::rgb(1, 2, 3)));
        assert_eq!(
            parse_color("rgba(1,2,3,0.5)"),
            Some(Color::rgba(1, 2, 3, 0.5))
        );
        assert_eq!(parse_color("rgb(100% 0% 0%)"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(parse_color("hsl(0,100%,50%)"), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            parse_color("hsl(120, 100%, 50%)"),
            Some(Color::rgb(0, 255, 0))
        );
    }

    #[test]
    fn named_lookup() {
        assert_eq!(parse_any("rebeccapurple"), Some(Color::rgb(102, 51, 153)));
        assert_eq!(parse_any("Tomato"), Some(Color::rgb(255, 99, 71)));
        assert_eq!(parse_any("transparent"), Some(Color::TRANSPARENT));
        assert_eq!(parse_any("notacolor"), None);
    }

    #[test]
    fn table_is_sorted() {
        // `named()` uses binary search, so ordering is load-bearing.
        for w in NAMED.windows(2) {
            assert!(
                w[0].0 < w[1].0,
                "NAMED out of order: {} >= {}",
                w[0].0,
                w[1].0
            );
        }
    }
}
