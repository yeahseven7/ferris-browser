//! A hand-written CSS parser: comments, at-rules, selector lists,
//! declarations, `!important`, and specificity calculation.
//!
//! Selector *matching* is delegated to `scraper`/`selectors` (the same crate
//! family Servo uses), while specificity is computed here because that crate
//! does not expose it.

use scraper::Selector;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Specificity(pub u32, pub u32, pub u32);

#[derive(Clone, Debug)]
pub struct Declaration {
    pub prop: String,
    pub value: String,
    pub important: bool,
}

#[derive(Clone)]
pub struct CompiledSelector {
    pub sel: Selector,
    pub spec: Specificity,
    /// Global source order across all sheets, used as the cascade tiebreak.
    pub order: usize,
}

#[derive(Clone)]
pub struct Rule {
    pub selectors: Vec<CompiledSelector>,
    pub decls: Vec<Declaration>,
}

#[derive(Default, Clone)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    pub parse_errors: usize,
}

/// Strip `/* ... */` comments without disturbing string contents.
fn strip_comments(css: &str) -> String {
    let b: Vec<char> = css.chars().collect();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < b.len() {
        if let Some(q) = quote {
            out.push(b[i]);
            if b[i] == '\\' && i + 1 < b.len() {
                i += 1;
                out.push(b[i]);
            } else if b[i] == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match b[i] {
            '"' | '\'' => {
                quote = Some(b[i]);
                out.push(b[i]);
                i += 1;
            }
            '/' if i + 1 < b.len() && b[i + 1] == '*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                out.push(' ');
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Find the index of the `}` matching the `{` at `open`.
fn match_brace(s: &[char], open: usize) -> usize {
    let mut depth = 0i32;
    let mut i = open;
    let mut quote: Option<char> = None;
    while i < s.len() {
        let c = s[i];
        if let Some(q) = quote {
            if c == '\\' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
        } else {
            match c {
                '"' | '\'' => quote = Some(c),
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    s.len().saturating_sub(1)
}

/// Split a selector list on top-level commas (ignoring commas inside `:not(...)`).
fn split_selectors(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.retain(|s| !s.is_empty());
    out
}

/// Split a declaration block on top-level semicolons.
fn split_decls(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    for c in s.chars() {
        if let Some(q) = quote {
            cur.push(c);
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                cur.push(c);
            }
            '(' | '[' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' => {
                depth -= 1;
                cur.push(c);
            }
            ';' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Parse `prop: value [!important]`.
pub fn parse_declaration(s: &str) -> Option<Declaration> {
    let (prop, value) = s.split_once(':')?;
    let prop = prop.trim().to_ascii_lowercase();
    let mut value = value.trim().to_string();
    if prop.is_empty() || value.is_empty() {
        return None;
    }
    // CSS custom properties are not applied by this engine.
    if prop.starts_with("--") {
        return None;
    }
    let mut important = false;
    let lowered = value.to_ascii_lowercase();
    if let Some(idx) = lowered.rfind("!important") {
        important = true;
        value = value[..idx].trim().to_string();
    }
    if value.is_empty() {
        return None;
    }
    Some(Declaration {
        prop,
        value,
        important,
    })
}

/// Parse a `style="..."` attribute value into declarations.
pub fn parse_inline_style(s: &str) -> Vec<Declaration> {
    split_decls(s)
        .iter()
        .filter_map(|d| parse_declaration(d))
        .collect()
}

/// Compute (a, b, c) specificity from a single compound-selector sequence.
///
/// a = ID selectors, b = class/attribute/pseudo-class, c = type/pseudo-element.
pub fn specificity(sel: &str) -> Specificity {
    let chars: Vec<char> = sel.chars().collect();
    let (mut a, mut b, mut c) = (0u32, 0u32, 0u32);
    let mut i = 0;
    // True when the next identifier in this compound is a type selector.
    let mut expect_type = true;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            '#' => {
                a += 1;
                i += 1;
                while i < chars.len() && (chars[i].is_alphanumeric() || "-_".contains(chars[i])) {
                    i += 1;
                }
                expect_type = false;
            }
            '.' => {
                b += 1;
                i += 1;
                while i < chars.len() && (chars[i].is_alphanumeric() || "-_".contains(chars[i])) {
                    i += 1;
                }
                expect_type = false;
            }
            '[' => {
                b += 1;
                let mut depth = 1;
                i += 1;
                while i < chars.len() && depth > 0 {
                    if chars[i] == '[' {
                        depth += 1;
                    } else if chars[i] == ']' {
                        depth -= 1;
                    }
                    i += 1;
                }
                expect_type = false;
            }
            ':' => {
                i += 1;
                let mut pseudo_element = false;
                if i < chars.len() && chars[i] == ':' {
                    pseudo_element = true;
                    i += 1;
                }
                // Read the pseudo name.
                while i < chars.len() && (chars[i].is_alphanumeric() || "-_".contains(chars[i])) {
                    i += 1;
                }
                // Skip an argument list; per spec, :not() takes the specificity
                // of its most specific argument, approximated here as b += 1.
                if i < chars.len() && chars[i] == '(' {
                    let mut depth = 1;
                    i += 1;
                    while i < chars.len() && depth > 0 {
                        match chars[i] {
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                        i += 1;
                    }
                }
                if pseudo_element {
                    c += 1;
                } else {
                    b += 1;
                }
                expect_type = false;
            }
            '*' => {
                i += 1;
                expect_type = false;
            }
            ' ' | '\t' | '\n' | '\r' | '>' | '+' | '~' => {
                i += 1;
                expect_type = true;
            }
            _ if ch.is_alphanumeric() || ch == '-' || ch == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || "-_".contains(chars[i])) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                // Pseudo-classes written without a leading colon are not valid
                // selectors; anything else in type position counts as a type.
                if expect_type && !word.is_empty() {
                    c += 1;
                }
                expect_type = false;
            }
            _ => i += 1,
        }
    }
    Specificity(a, b, c)
}

impl Stylesheet {
    /// Parse a stylesheet. Rules are tagged with a running source order.
    pub fn parse(css: &str, start_order: usize) -> (Stylesheet, usize) {
        let cleaned = strip_comments(css);
        let chars: Vec<char> = cleaned.chars().collect();
        let mut sheet = Stylesheet::default();
        let mut order = start_order;
        let mut i = 0;

        while i < chars.len() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }

            if chars[i] == '@' {
                let prelude_start = i;
                while i < chars.len() && chars[i] != '{' && chars[i] != ';' {
                    i += 1;
                }
                let prelude: String = chars[prelude_start..i].iter().collect();
                let prelude_l = prelude.to_ascii_lowercase();
                if i < chars.len() && chars[i] == ';' {
                    // @import / @charset / @namespace — not applied.
                    i += 1;
                    continue;
                }
                if i >= chars.len() {
                    break;
                }
                let close = match_brace(&chars, i);
                let inner: String = chars[i + 1..close].iter().collect();

                // Media queries: apply only if the query can match a screen.
                let applies = if prelude_l.trim_start().starts_with("@media") {
                    let q = prelude_l.replace("@media", "");
                    let q = q.trim();
                    q.is_empty()
                        || q.contains("all")
                        || q.contains("screen")
                        || (q.contains("min-width") || q.contains("max-width"))
                } else {
                    prelude_l.trim_start().starts_with("@supports")
                        || prelude_l.trim_start().starts_with("@layer")
                };

                if applies {
                    let (sub, next_order) = Stylesheet::parse_block(&inner, order);
                    sheet.rules.extend(sub.rules);
                    sheet.parse_errors += sub.parse_errors;
                    order = next_order;
                } else {
                    // Still advance the order counter so cascade stays stable.
                    order += inner.matches('{').count();
                }
                i = close + 1;
                continue;
            }

            // Ordinary rule: selector-list { decls }
            let sel_start = i;
            while i < chars.len() && chars[i] != '{' && chars[i] != '}' {
                i += 1;
            }
            if i >= chars.len() || chars[i] != '{' {
                i += 1;
                continue;
            }
            let sel_text: String = chars[sel_start..i].iter().collect();
            let close = match_brace(&chars, i);
            let body: String = chars[i + 1..close].iter().collect();
            i = close + 1;

            if let Some(rule) = Self::make_rule(&sel_text, &body, &mut order) {
                sheet.rules.push(rule);
            }
        }
        (sheet, order)
    }

    fn parse_block(css: &str, start_order: usize) -> (Stylesheet, usize) {
        Stylesheet::parse(css, start_order)
    }

    fn make_rule(sel_text: &str, body: &str, order: &mut usize) -> Option<Rule> {
        let decls: Vec<Declaration> = split_decls(body)
            .iter()
            .filter_map(|d| parse_declaration(d))
            .collect();
        if decls.is_empty() {
            return None;
        }
        let mut selectors = Vec::new();
        for s in split_selectors(sel_text) {
            // Skip selectors this engine cannot represent.
            if s.contains("::before")
                || s.contains("::after")
                || s.contains(":hover")
                || s.contains(":focus")
                || s.contains(":active")
                || s.contains(":visited")
            {
                continue;
            }
            match Selector::parse(&s) {
                Ok(sel) => selectors.push(CompiledSelector {
                    spec: specificity(&s),
                    sel,
                    order: *order,
                }),
                Err(_) => continue,
            }
        }
        if selectors.is_empty() {
            return None;
        }
        *order += 1;
        Some(Rule { selectors, decls })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_rule() {
        let (sheet, _) = Stylesheet::parse("p { color: red; margin: 0 }", 0);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].decls.len(), 2);
        assert_eq!(sheet.rules[0].decls[0].prop, "color");
        assert_eq!(sheet.rules[0].decls[1].value, "0");
    }

    #[test]
    fn strips_comments_and_strings() {
        let (sheet, _) = Stylesheet::parse("/* } { */ a { content: \"}\"; color: blue }", 0);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].decls.len(), 2);
    }

    #[test]
    fn specificity_ordering() {
        assert_eq!(specificity("*"), Specificity(0, 0, 0));
        assert_eq!(specificity("p"), Specificity(0, 0, 1));
        assert_eq!(specificity("p.x"), Specificity(0, 1, 1));
        assert_eq!(specificity("#id"), Specificity(1, 0, 0));
        assert_eq!(specificity("div p > a.link"), Specificity(0, 1, 3));
        assert!(specificity("#id") > specificity("p.x.y.z"));
    }

    #[test]
    fn important_flag() {
        let d = parse_declaration("color: red !important").unwrap();
        assert!(d.important);
        assert_eq!(d.value, "red");
    }

    #[test]
    fn media_block_is_parsed() {
        let (sheet, _) = Stylesheet::parse(
            "@media screen { p { color: red } } @media print { p { color: blue } }",
            0,
        );
        assert_eq!(sheet.rules.len(), 1, "print rules should be dropped");
    }

    #[test]
    fn selector_list_expands() {
        let (sheet, _) = Stylesheet::parse("h1, h2, .x { color: red }", 0);
        assert_eq!(sheet.rules[0].selectors.len(), 3);
        assert_eq!(sheet.rules[0].selectors[0].spec, Specificity(0, 0, 1));
        assert_eq!(sheet.rules[0].selectors[2].spec, Specificity(0, 1, 0));
    }
}
