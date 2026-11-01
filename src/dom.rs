//! A small mutable DOM tree ("the mirror") that JavaScript runs against.
//!
//! The engine's render pipeline is immutable (fetch → parse → cascade →
//! layout → paint), so scripts cannot poke at it live. Instead, when the `js`
//! feature is on, the fetched HTML is first mirrored into this arena tree,
//! Boa executes the page's scripts against the mirror through the bindings in
//! `js.rs`, and the mutated mirror is serialized back to HTML and fed through
//! the normal pipeline. The mirror is deliberately a plain Rust data
//! structure: the JS bindings are a thin, swappable layer on top of it.
//!
//! Only available with `--features js`.

use scraper::{Html, Node as ScraperNode};

#[derive(Debug, Clone)]
pub enum Kind {
    /// The synthetic document node (index 0).
    Document,
    Element {
        tag: String,
        attrs: Vec<(String, String)>,
    },
    Text(String),
    Comment(String),
    Doctype,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: Kind,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

/// Tags serialized without a closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

fn is_void(tag: &str) -> bool {
    VOID.contains(&tag.to_ascii_lowercase().as_str())
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

#[derive(Debug, Default)]
pub struct Doc {
    pub nodes: Vec<Node>,
}

impl Doc {
    pub fn new() -> Doc {
        Doc {
            nodes: vec![Node {
                kind: Kind::Document,
                parent: None,
                children: vec![],
            }],
        }
    }

    /// Mirror a parsed HTML document into the arena.
    pub fn from_html(html: &str) -> Doc {
        let mut doc = Doc::new();
        let parsed = Html::parse_document(html);
        let root = parsed.root_element();
        // Mirror the root element (html) and everything below it.
        let idx = doc.mirror_element(root);
        doc.nodes[0].children.push(idx);
        doc.nodes[idx].parent = Some(0);
        doc
    }

    fn mirror_element(&mut self, el: scraper::ElementRef) -> usize {
        let tag = el.value().name().to_ascii_lowercase();
        let attrs = el
            .value()
            .attrs()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.to_string()))
            .collect();
        let me = self.push(Node {
            kind: Kind::Element { tag, attrs },
            parent: None,
            children: vec![],
        });
        for child in el.children() {
            self.mirror_node(child, me);
        }
        me
    }

    fn mirror_node(&mut self, node: ego_tree::NodeRef<ScraperNode>, parent: usize) {
        use scraper::Node as S;
        match node.value() {
            S::Element(_) => {
                if let Some(el) = scraper::ElementRef::wrap(node) {
                    let idx = self.mirror_element(el);
                    self.nodes[idx].parent = Some(parent);
                    self.nodes[parent].children.push(idx);
                }
            }
            S::Text(t) => {
                let idx = self.push(Node {
                    kind: Kind::Text(t.text.to_string()),
                    parent: Some(parent),
                    children: vec![],
                });
                self.nodes[parent].children.push(idx);
            }
            S::Comment(c) => {
                let idx = self.push(Node {
                    kind: Kind::Comment(c.comment.to_string()),
                    parent: Some(parent),
                    children: vec![],
                });
                self.nodes[parent].children.push(idx);
            }
            S::Doctype(_) => {
                let idx = self.push(Node {
                    kind: Kind::Doctype,
                    parent: Some(parent),
                    children: vec![],
                });
                self.nodes[parent].children.push(idx);
            }
            _ => {}
        }
    }

    fn push(&mut self, n: Node) -> usize {
        self.nodes.push(n);
        self.nodes.len() - 1
    }

    // -- accessors ---------------------------------------------------------

    pub fn tag(&self, i: usize) -> Option<&str> {
        match &self.nodes.get(i)?.kind {
            Kind::Element { tag, .. } => Some(tag),
            _ => None,
        }
    }

    pub fn attr(&self, i: usize, name: &str) -> Option<&str> {
        match &self.nodes.get(i)?.kind {
            Kind::Element { attrs, .. } => attrs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str()),
            _ => None,
        }
    }

    pub fn set_attr(&mut self, i: usize, name: &str, value: &str) {
        if let Kind::Element { attrs, .. } = &mut self.nodes[i].kind {
            match attrs.iter_mut().find(|(k, _)| k == name) {
                Some(slot) => slot.1 = value.to_string(),
                None => attrs.push((name.to_string(), value.to_string())),
            }
        }
    }

    pub fn remove_attr(&mut self, i: usize, name: &str) {
        if let Kind::Element { attrs, .. } = &mut self.nodes[i].kind {
            attrs.retain(|(k, _)| k != name);
        }
    }

    pub fn new_element(&mut self, tag: &str) -> usize {
        self.push(Node {
            kind: Kind::Element {
                tag: tag.to_ascii_lowercase(),
                attrs: vec![],
            },
            parent: None,
            children: vec![],
        })
    }

    pub fn new_text(&mut self, s: &str) -> usize {
        self.push(Node {
            kind: Kind::Text(s.to_string()),
            parent: None,
            children: vec![],
        })
    }

    pub fn append(&mut self, parent: usize, child: usize) {
        // detach from any previous parent
        if let Some(p) = self.nodes[child].parent
            && p != parent
        {
            self.nodes[p].children.retain(|c| *c != child);
        }
        self.nodes[child].parent = Some(parent);
        self.nodes[parent].children.push(child);
    }

    pub fn remove(&mut self, i: usize) {
        if let Some(p) = self.nodes[i].parent {
            self.nodes[p].children.retain(|c| *c != i);
            self.nodes[i].parent = None;
        }
    }

    /// Indices of the `<html>` element and `<body>` element, creating them if
    /// the mirrored document lacks them (fragment-ish pages).
    pub fn html_element(&mut self) -> usize {
        if let Some(i) = self.nodes[0]
            .children
            .iter()
            .copied()
            .find(|i| self.tag(*i) == Some("html"))
        {
            return i;
        }
        let i = self.new_element("html");
        self.append(0, i);
        i
    }

    pub fn body_element(&mut self) -> usize {
        let html = self.html_element();
        if let Some(i) = self.find_child_tag(html, "body") {
            return i;
        }
        let i = self.new_element("body");
        self.append(html, i);
        i
    }

    pub fn head_element(&mut self) -> usize {
        let html = self.html_element();
        if let Some(i) = self.find_child_tag(html, "head") {
            return i;
        }
        let i = self.new_element("head");
        let body = self.find_child_tag(html, "body");
        let pos = match body {
            Some(b) => self.nodes[html]
                .children
                .iter()
                .position(|c| *c == b)
                .unwrap(),
            None => self.nodes[html].children.len(),
        };
        self.nodes[i].parent = Some(html);
        self.nodes[html].children.insert(pos, i);
        i
    }

    fn find_child_tag(&self, parent: usize, tag: &str) -> Option<usize> {
        self.nodes[parent]
            .children
            .iter()
            .copied()
            .find(|i| self.tag(*i) == Some(tag))
    }

    // -- queries -----------------------------------------------------------

    /// True when the node's parent chain reaches the document node. Removed
    /// subtrees stay in the arena but must not answer queries.
    pub fn connected(&self, i: usize) -> bool {
        let mut cur = i;
        loop {
            if cur == 0 {
                return true;
            }
            match self.nodes.get(cur).and_then(|n| n.parent) {
                Some(p) => cur = p,
                None => return false,
            }
        }
    }

    pub fn by_id(&self, id: &str) -> Option<usize> {
        (1..self.nodes.len())
            .filter(|i| self.connected(*i))
            .find(|i| self.attr(*i, "id") == Some(id))
    }

    pub fn by_tag(&self, tag: &str) -> Vec<usize> {
        let tag = tag.to_ascii_lowercase();
        (1..self.nodes.len())
            .filter(|i| self.connected(*i))
            .filter(|i| self.tag(*i) == Some(tag.as_str()))
            .collect()
    }

    pub fn by_class(&self, class: &str) -> Vec<usize> {
        (1..self.nodes.len())
            .filter(|i| self.connected(*i))
            .filter(|i| {
                self.attr(*i, "class")
                    .map(|c| c.split_whitespace().any(|c| c == class))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// descendant-or-self walk of element nodes under `root`, document order.
    pub fn elements_under(&self, root: usize) -> Vec<usize> {
        let mut out = vec![];
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            if matches!(self.nodes[i].kind, Kind::Element { .. }) {
                out.push(i);
            }
            stack.extend(self.nodes[i].children.iter().rev());
        }
        out
    }

    /// One compound simple selector: `tag`, `#id`, `.class`, or combinations
    /// like `div#a.b`. Returns matching elements under root (descendants).
    pub fn match_simple(&self, root: usize, sel: &str) -> Vec<usize> {
        // descendant chains: "a b c" → match last against ancestors matched
        // recursively; supports the common cases without a full selector eng.
        let parts: Vec<&str> = sel.split_whitespace().collect();
        if parts.is_empty() {
            return vec![];
        }
        if parts.len() == 1 {
            return self
                .elements_under(root)
                .into_iter()
                .filter(|i| *i != root && self.matches_compound(*i, parts[0]))
                .collect();
        }
        let (last, rest) = (*parts.last().unwrap(), &parts[..parts.len() - 1]);
        let ancestors = self.match_simple(root, &rest.join(" "));
        let mut out = vec![];
        for a in ancestors {
            for i in self.elements_under(a) {
                if i != a && self.matches_compound(i, last) && !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out
    }

    fn matches_compound(&self, i: usize, sel: &str) -> bool {
        // split into tag / #id / .class chunks
        let mut cur = String::new();
        let mut kind = 't';
        let mut parts: Vec<(char, String)> = vec![];
        for (_, c) in sel.char_indices() {
            if c == '#' || c == '.' {
                if !cur.is_empty() {
                    parts.push((kind, cur.clone()));
                    cur.clear();
                }
                kind = c;
            } else {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            parts.push((kind, cur));
        }
        if parts.is_empty() {
            return false;
        }
        parts.iter().all(|(k, v)| match k {
            't' => self.tag(i) == Some(v.to_ascii_lowercase().as_str()),
            '#' => self.attr(i, "id") == Some(v.as_str()),
            '.' => self
                .attr(i, "class")
                .map(|c| c.split_whitespace().any(|c| c == *v))
                .unwrap_or(false),
            _ => false,
        })
    }

    // -- text / serialization ----------------------------------------------

    pub fn text_content(&self, i: usize) -> String {
        let mut out = String::new();
        self.collect_text(i, &mut out);
        out
    }

    fn collect_text(&self, i: usize, out: &mut String) {
        match &self.nodes[i].kind {
            Kind::Text(t) => out.push_str(t),
            _ => {
                for c in self.nodes[i].children.clone() {
                    self.collect_text(c, out);
                }
            }
        }
    }

    pub fn set_text_content(&mut self, i: usize, text: &str) {
        for c in std::mem::take(&mut self.nodes[i].children) {
            self.nodes[c].parent = None;
        }
        let t = self.new_text(text);
        self.append(i, t);
    }

    /// Replace children with a parsed HTML fragment.
    pub fn set_inner_html(&mut self, i: usize, html: &str) {
        for c in std::mem::take(&mut self.nodes[i].children) {
            self.nodes[c].parent = None;
        }
        let frag = Html::parse_fragment(html);
        for child in frag.root_element().children() {
            self.mirror_node(child, i);
        }
    }

    pub fn inner_html(&self, i: usize) -> String {
        let mut out = String::new();
        for c in self.nodes[i].children.clone() {
            self.serialize(c, &mut out);
        }
        out
    }

    pub fn to_html(&self) -> String {
        let mut out = String::new();
        for c in self.nodes[0].children.clone() {
            self.serialize(c, &mut out);
        }
        out
    }

    fn serialize_raw(&self, i: usize, out: &mut String) {
        match &self.nodes[i].kind {
            Kind::Text(t) => out.push_str(t),
            _ => {
                for c in self.nodes[i].children.clone() {
                    self.serialize_raw(c, out);
                }
            }
        }
    }

    fn serialize(&self, i: usize, out: &mut String) {
        match &self.nodes[i].kind {
            Kind::Document => {}
            Kind::Doctype => out.push_str("<!DOCTYPE html>"),
            Kind::Text(t) => out.push_str(&escape_text(t)),
            Kind::Comment(c) => {
                out.push_str("<!--");
                out.push_str(c);
                out.push_str("-->");
            }
            Kind::Element { tag, attrs } => {
                out.push('<');
                out.push_str(tag);
                for (k, v) in attrs {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    out.push_str(&escape_attr(v));
                    out.push('"');
                }
                out.push('>');
                if !is_void(tag) {
                    // script/style are raw-text elements: their content must
                    // not be entity-escaped or reparsing changes the code.
                    let raw = tag == "script" || tag == "style";
                    for c in self.nodes[i].children.clone() {
                        if raw {
                            self.serialize_raw(c, out);
                        } else {
                            self.serialize(c, out);
                        }
                    }
                    out.push_str("</");
                    out.push_str(tag);
                    out.push('>');
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<html><head><title>T</title></head>
        <body><div id="a" class="box red">hello <b>world</b></div>
        <p class="box">para</p><img src="x.png"></body></html>"#;

    #[test]
    fn mirror_roundtrip_keeps_structure() {
        let d = Doc::from_html(PAGE);
        let html = d.to_html();
        // scraper yields attributes in sorted order, so the mirror does too
        assert!(html.contains("<div class=\"box red\" id=\"a\">hello <b>world</b></div>"));
        assert!(html.contains("<img src=\"x.png\">"));
        assert!(html.contains("<title>T</title>"));
    }

    #[test]
    fn queries_find_nodes() {
        let d = Doc::from_html(PAGE);
        let a = d.by_id("a").expect("id lookup");
        assert_eq!(d.tag(a), Some("div"));
        assert_eq!(d.text_content(a), "hello world");
        assert_eq!(d.by_tag("p").len(), 1);
        assert_eq!(d.by_class("box").len(), 2);
        let body = d.by_tag("body")[0];
        assert_eq!(d.match_simple(body, "div.box").len(), 1);
        assert_eq!(d.match_simple(body, ".red").len(), 1);
        assert_eq!(d.match_simple(body, "div .red").len(), 0);
        assert_eq!(d.match_simple(body, "div b").len(), 1);
    }

    #[test]
    fn mutation_ops() {
        let mut d = Doc::from_html(PAGE);
        let body = d.by_tag("body")[0];
        let el = d.new_element("span");
        d.set_attr(el, "id", "new");
        d.set_text_content(el, "hi");
        d.append(body, el);
        assert!(d.to_html().contains("<span id=\"new\">hi</span>"));
        d.set_inner_html(el, "<i>it</i>");
        assert!(d.to_html().contains("<span id=\"new\"><i>it</i></span>"));
        d.remove(el);
        assert!(!d.to_html().contains("span"));
    }

    #[test]
    fn text_escaping_survives_roundtrip() {
        let mut d = Doc::from_html("<html><body><p>a &lt; b &amp; c</p></body></html>");
        let p = d.by_tag("p")[0];
        assert_eq!(d.text_content(p), "a < b & c");
        d.set_text_content(p, "1 < 2 & 3");
        assert!(d.to_html().contains("1 &lt; 2 &amp; 3"));
    }

    #[test]
    fn detached_subtrees_stop_answering_queries() {
        let mut d = Doc::from_html(PAGE);
        let a = d.by_id("a").expect("attached");
        assert!(d.connected(a));
        d.remove(a);
        assert!(!d.connected(a));
        assert!(d.by_id("a").is_none(), "removed nodes must not be found");
        assert!(d.by_class("box").len() == 1);
    }

    #[test]
    fn script_and_style_text_serializes_raw() {
        let d = Doc::from_html(
            "<html><body><script>if (a < b && c > d) { x = '&'; }</script>\
             <style>p > em { color: red }</style></body></html>",
        );
        let html = d.to_html();
        assert!(html.contains("if (a < b && c > d) { x = '&'; }"), "{html}");
        assert!(html.contains("p > em { color: red }"), "{html}");
    }

    #[test]
    fn body_and_head_accessors_create_missing() {
        let mut d = Doc::new();
        let body = d.body_element();
        let head = d.head_element();
        assert_ne!(body, head);
        assert_eq!(d.tag(body), Some("body"));
        assert_eq!(d.tag(head), Some("head"));
        let html = d.to_html();
        assert!(html.contains("<html><head></head><body></body></html>"));
    }
}
