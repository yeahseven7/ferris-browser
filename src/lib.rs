//! Ferris — a browser engine in Rust.
//!
//! Pipeline, mirroring a real browser:
//!
//! ```text
//!   URL
//!    └─> net      HTTP/HTTPS fetch over rustls, redirects, gzip/brotli
//!         └─> DOM      html5ever parse (via scraper) -> tree
//!              └─> css       stylesheet parse, selectors, specificity
//!                   └─> style     cascade + inheritance -> computed styles
//!                        └─> layout    block/inline/table -> display list
//!                             └─> paint      rasterize -> PNG
//!                                  └─> server    HTTP UI with clickable links
//! ```

pub mod color;
#[cfg(feature = "js")]
pub mod corpus;
pub mod css;
pub mod dom;
pub mod engine;
pub mod font;
#[cfg(feature = "js")]
pub mod js;
pub mod layout;
pub mod net;
pub mod paint;
pub mod server;
pub mod shell;
pub mod style;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Parse and cache a CSS selector. Used for the handful of fixed selectors the
/// engine needs (`body`, `title`, `style`, ...) where a parse failure would be
/// a bug rather than hostile input.
pub fn sel(s: &str) -> scraper::Selector {
    static CACHE: OnceLock<Mutex<HashMap<String, scraper::Selector>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(m) = cache.lock()
        && let Some(sel) = m.get(s)
    {
        return sel.clone();
    }
    let parsed = scraper::Selector::parse(s)
        .unwrap_or_else(|e| panic!("built-in selector {s:?} failed to parse: {e}"));
    if let Ok(mut m) = cache.lock() {
        m.insert(s.to_string(), parsed.clone());
    }
    parsed
}

/// Version string reported in the UI and in HTTP responses.
pub const VERSION: &str = "1.1.0";

pub fn banner() -> String {
    format!("Ferris {VERSION} — a browser engine written in Rust")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sel_caches_and_parses() {
        let a = sel("body");
        let b = sel("body");
        // Same selector text must yield a usable (cloned) selector each time.
        let doc = scraper::Html::parse_document("<html><body>x</body></html>");
        assert!(doc.select(&a).next().is_some());
        assert!(doc.select(&b).next().is_some());
        assert!(doc.select(&sel("link[rel~=icon]")).next().is_none());
    }

    #[test]
    fn banner_mentions_rust() {
        assert!(banner().contains("Rust"));
        assert!(banner().contains(VERSION));
    }
}
