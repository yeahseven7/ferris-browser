//! The JavaScript compatibility corpus: ten offline fixture pages that each
//! exercise one slice of the DOM binding, plus optional live URLs.
//!
//! `ferris jscorpus [live urls…]` runs every fixture through the real path
//! (script preprocess → render pipeline) and checks that the marker text the
//! scripts were supposed to produce actually ended up in the rendered page.
//! Only available with `--features js`.

use crate::engine;
use crate::js;

pub struct CorpusResult {
    pub name: String,
    pub scripts: usize,
    pub console: usize,
    pub errors: Vec<String>,
    pub ok: bool,
    pub note: String,
}

struct Fixture {
    name: &'static str,
    html: &'static str,
    /// Substring that must appear in the rendered visible text.
    expect: &'static str,
    /// Exact number of script errors this fixture is *supposed* to produce.
    expect_errors: usize,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "01 getElementById + textContent",
        html: include_str!("../corpus/01-get-element-by-id.html"),
        expect: "PASS-01-DOM-WRITE",
        expect_errors: 0,
    },
    Fixture {
        name: "02 createElement + appendChild",
        html: include_str!("../corpus/02-create-append.html"),
        expect: "PASS-02-APPEND-3",
        expect_errors: 0,
    },
    Fixture {
        name: "03 innerHTML + querySelector(All)",
        html: include_str!("../corpus/03-inner-html-query.html"),
        expect: "PASS-03-INNER-2-alpha",
        expect_errors: 0,
    },
    Fixture {
        name: "04 classList add/remove/toggle",
        html: include_str!("../corpus/04-classlist.html"),
        expect: "PASS-04-CLASS-cz",
        expect_errors: 0,
    },
    Fixture {
        name: "05 attribute get/set/has/remove",
        html: include_str!("../corpus/05-attributes.html"),
        expect: "PASS-05-ATTR",
        expect_errors: 0,
    },
    Fixture {
        name: "06 style.setProperty → cascade",
        html: include_str!("../corpus/06-style-cascade.html"),
        expect: "PASS-06-STYLE",
        expect_errors: 0,
    },
    Fixture {
        name: "07 console + navigator + location",
        html: include_str!("../corpus/07-console-navigator.html"),
        expect: "PASS-07-CONSOLE",
        expect_errors: 0,
    },
    Fixture {
        name: "08 runaway loop contained",
        html: include_str!("../corpus/08-runaway-contained.html"),
        expect: "PASS-08-ALIVE",
        expect_errors: 1,
    },
    Fixture {
        name: "09 document.title + document.write",
        html: include_str!("../corpus/09-title-write.html"),
        expect: "PASS-09-WRITE",
        expect_errors: 0,
    },
    Fixture {
        name: "10 script order + setTimeout",
        html: include_str!("../corpus/10-order-timers.html"),
        expect: "PASS-10-ORDER",
        expect_errors: 0,
    },
];

/// Run every fixture (and any live URLs) and report. Second element is
/// "all passed".
pub fn run(live: &[String]) -> (Vec<CorpusResult>, bool) {
    let mut out = vec![];
    let mut all_ok = true;

    for f in FIXTURES {
        let base = format!(
            "https://corpus.ferris/{}",
            f.name.split(' ').next().unwrap_or("x")
        );
        let (html, rep) = js::preprocess(f.html, &base);
        let pr = engine::render_html(&html, &base, 900, false);
        let marker_ok = pr.text.contains(f.expect);
        let errs_ok = rep.errors.len() == f.expect_errors;
        let ok = marker_ok && errs_ok && rep.scripts >= 1;
        let note = if !marker_ok {
            format!("marker {:?} missing from rendered text", f.expect)
        } else if !errs_ok {
            format!(
                "expected {} errors, got {}",
                f.expect_errors,
                rep.errors.len()
            )
        } else {
            String::new()
        };
        all_ok &= ok;
        out.push(CorpusResult {
            name: f.name.to_string(),
            scripts: rep.scripts,
            console: rep.console.len(),
            errors: rep.errors.clone(),
            ok,
            note,
        });
    }

    for url in live {
        let pr = engine::render(url, 1000, false);
        let ok = pr.error.is_none();
        let note = if ok {
            format!(
                "{} scripts, {} js errors, {} console lines",
                pr.js.scripts,
                pr.js.errors.len(),
                pr.js.console.len()
            )
        } else {
            format!(
                "render failed: {}",
                pr.error
                    .as_ref()
                    .map(|e| e.headline.clone())
                    .unwrap_or_default()
            )
        };
        all_ok &= ok;
        out.push(CorpusResult {
            name: format!("live {url}"),
            scripts: pr.js.scripts,
            console: pr.js.console.len(),
            errors: pr.js.errors.clone(),
            ok,
            note,
        });
    }

    (out, all_ok)
}

/// Human-readable table for the CLI.
pub fn report(results: &[CorpusResult], all_ok: bool) -> String {
    let mut s = String::from("JavaScript compat corpus (Boa + Ferris DOM mirror)\n\n");
    for r in results {
        s.push_str(&format!(
            "[{}] {}\n         scripts:{} console:{} errors:{}{}\n",
            if r.ok { "PASS" } else { "FAIL" },
            r.name,
            r.scripts,
            r.console,
            r.errors.len(),
            if r.note.is_empty() {
                String::new()
            } else {
                format!("  <- {}", r.note)
            },
        ));
        for e in r.errors.iter().take(3) {
            s.push_str(&format!("         ! {e}\n"));
        }
    }
    let passed = results.iter().filter(|r| r.ok).count();
    s.push_str(&format!(
        "\n{}/{} passed — {}\n",
        passed,
        results.len(),
        if all_ok { "OK" } else { "FAILURES" }
    ));
    s
}
