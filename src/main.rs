//! Ferris — a browser engine written in Rust.
//!
//! ```text
//! ferris-browser [url]                        open the browser window
//! ferris-browser serve [--port 8080]          run the browser UI
//! ferris-browser shot <url> [-o out.png]      rasterize one page to a PNG
//! ferris-browser info <url>                   print render metadata to stdout
//! ```

use std::time::Instant;

/// Subcommands; anything else that looks like a URL opens the window at it,
/// and no arguments at all opens the browser on its start page.
const KNOWN_CMDS: &[&str] = &[
    "serve",
    "shot",
    "info",
    "window",
    "gui",
    "shelltest",
    "jscorpus",
    "version",
    "--version",
    "-V",
    "help",
    "--help",
    "-h",
];

fn looks_like_url(s: &str) -> bool {
    s.contains("://") || (s.contains('.') && !s.contains(' ') && !s.starts_with('-'))
}

fn main() {
    let mut argv: Vec<String> = std::env::args().collect();
    let cmd: &str = match argv.get(1).map(String::as_str) {
        None => "window",
        Some(c) if KNOWN_CMDS.contains(&c) => {
            // borrow-friendly: the slice is stable for the program's life
            Box::leak(c.to_string().into_boxed_str())
        }
        Some(c) if looks_like_url(c) => {
            argv.insert(1, "window".to_string());
            "window"
        }
        Some(c) => Box::leak(c.to_string().into_boxed_str()),
    };
    let args = argv;

    match cmd {
        "serve" => {
            let port = flag_u16(&args, "--port").unwrap_or(8080);
            println!("{}", ferris::banner());
            let srv = ferris::server::Server::new(port);
            if let Err(e) = srv.run() {
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    eprintln!("server error: port {port} is already in use.");
                    eprintln!(
                        "  Another process (perhaps an earlier `ferris-browser serve`) holds it."
                    );
                    eprintln!(
                        "  Use another port:   ferris-browser serve --port {}",
                        port + 1
                    );
                    eprintln!("  Or find the holder:  ss -tlnp | grep :{port}");
                } else {
                    eprintln!("server error: {e}");
                }
                std::process::exit(1);
            }
        }
        "shot" => {
            let url = positional(&args[1..]);
            if url.is_empty() {
                eprintln!(
                    "usage: ferris-browser shot <url> [-o out.png] [--width N] [--no-images]"
                );
                std::process::exit(2);
            }
            let out = flag_str(&args, "-o").or_else(|| flag_str(&args, "--out"));
            let width = flag_u32(&args, "--width").unwrap_or(1280);
            let images = !args.iter().any(|a| a == "--no-images");

            let started = Instant::now();
            let r = ferris::engine::render(&url, width, images);
            let path = out.unwrap_or_else(|| "ferris-shot.png".to_string());
            match std::fs::write(&path, &r.png) {
                Ok(()) => {
                    println!("wrote {path} ({} bytes)", r.png.len());
                    println!(
                        "  url:     {}\n  title:   {}\n  status:  {}\n  canvas:  {}x{}{}\n  sheets:  {}\n  links:   {}\n  stats:   {} nodes, {} lines, {} paint ops, {} images ({} failed)\n  timings: fetch {:.0}ms parse {:.0}ms css {:.0}ms layout {:.0}ms paint {:.0}ms total {:.0}ms\n  elapsed: {:.0}ms",
                        r.final_url,
                        r.title,
                        r.status,
                        r.width,
                        r.height,
                        if r.clipped { " (clipped)" } else { "" },
                        r.sheets,
                        r.links.len(),
                        r.stats.nodes,
                        r.stats.lines,
                        r.stats.cmds,
                        r.stats.images,
                        r.stats.images_failed,
                        ms(r.timings.fetch),
                        ms(r.timings.parse),
                        ms(r.timings.css),
                        ms(r.timings.layout),
                        ms(r.timings.paint),
                        ms(r.timings.total),
                        started.elapsed().as_secs_f64() * 1000.0,
                    );
                    if let Some(e) = &r.error {
                        println!("  ERROR:   {} ({})", e.headline, e.code);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("could not write {path}: {e}");
                    std::process::exit(1);
                }
            }
        }
        "info" => {
            let url = positional(&args[1..]);
            if url.is_empty() {
                eprintln!("usage: ferris-browser info <url> [--width N]");
                std::process::exit(2);
            }
            let width = flag_u32(&args, "--width").unwrap_or(1280);
            let images = !args.iter().any(|a| a == "--no-images");
            let r = ferris::engine::render(&url, width, images);
            println!("url:      {}", r.final_url);
            println!("status:   {}", r.status);
            println!("title:    {:?}", r.title);
            println!("content:  {} ({} byte PNG)", r.content_type, r.png.len());
            println!("canvas:   {}x{}", r.width, r.height);
            println!("sheets:   {}", r.sheets);
            println!("links:    {}", r.links.len());
            println!("nodes:    {}", r.stats.nodes);
            println!("lines:    {}", r.stats.lines);
            println!("paintops: {}", r.stats.cmds);
            println!(
                "images:   {} ok, {} failed",
                r.stats.images, r.stats.images_failed
            );
            println!("truncat:  {}", r.stats.truncated);
            if let Some(e) = &r.error {
                println!("error:    {} ({})", e.headline, e.code);
            }
        }
        "window" | "gui" => {
            let url = positional(&args[1..]);
            let images = !args.iter().any(|a| a == "--no-images");
            println!("{}", ferris::banner());
            match ferris::shell::run(if url.is_empty() { None } else { Some(url) }, images) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        "shelltest" => {
            let url = positional(&args[1..]);
            if url.is_empty() {
                eprintln!(
                    "usage: ferris-browser shelltest <url> [--width N] [--height N] [-o f.png]"
                );
                std::process::exit(2);
            }
            let width = flag_u32(&args, "--width").unwrap_or(1000);
            let height = flag_u32(&args, "--height").unwrap_or(700);
            let out = flag_str(&args, "-o").or_else(|| flag_str(&args, "--out"));
            let extras = positional_all(&args[1..])
                .into_iter()
                .skip(1)
                .collect::<Vec<_>>();
            match ferris::shell::selftest(&url, &extras, width, height, out.as_deref()) {
                Ok(report) => print!("{report}"),
                Err(e) => {
                    eprintln!("selftest failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "jscorpus" => {
            #[cfg(feature = "js")]
            {
                let live = positional_all(&args[1..]);
                let (results, all_ok) = ferris::corpus::run(&live);
                print!("{}", ferris::corpus::report(&results, all_ok));
                if !all_ok {
                    std::process::exit(1);
                }
            }
            #[cfg(not(feature = "js"))]
            {
                eprintln!(
                    "this build has no JavaScript; rebuild with: cargo build --release --features js"
                );
                std::process::exit(2);
            }
        }
        "version" | "--version" | "-V" => println!("{}", ferris::banner()),
        "help" | "--help" | "-h" => println!("{}", USAGE),
        other => {
            eprintln!("unknown command: {other}\n\n{USAGE}");
            std::process::exit(2);
        }
    }
}

const USAGE: &str = "\
Ferris — a browser engine written in Rust

USAGE:
  ferris-browser [url]                         Open the browser window (start page or url)
  ferris-browser serve [--port N]              Start the browser UI (default port 8080)
  ferris-browser shot <url> [-o FILE]          Rasterize a page to PNG
                     [--width N]       Viewport width (default 1280)
                     [--no-images]     Skip fetching <img> resources
  ferris-browser info <url> [--width N]        Print render metadata
  ferris-browser window [url]                  Open the native window (winit+softbuffer)
  ferris-browser shelltest <url> [more urls]   Headless check of the window's interactive
                     [-o f.png]        logic; writes the exact frames it would show
                                       (extra urls appear as extra tabs)
  ferris-browser jscorpus [live urls]          Run the JS compat corpus (needs --features js)
  ferris-browser version                       Print the version
  ferris-browser help                          This message

PIPELINE:
  fetch (rustls) -> html5ever DOM -> CSS parse + cascade -> layout -> paint -> PNG
";

fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// First non-flag argument, i.e. the URL. `args` must already have the
/// program name stripped, so index 0 is the subcommand and is skipped.
/// Every non-flag argument, in order (subcommand excluded by the caller).
fn positional_all(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for (i, a) in args.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "-o" || a == "--out" || a == "--width" || a == "--height" || a == "--port" {
            skip_next = true;
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        out.push(a.clone());
    }
    out
}

fn positional(args: &[String]) -> String {
    let mut skip_next = false;
    for (i, a) in args.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "-o" || a == "--out" || a == "--width" || a == "--port" {
            skip_next = true;
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        return a.clone();
    }
    String::new()
}

fn flag_str(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn flag_u32(args: &[String], name: &str) -> Option<u32> {
    flag_str(args, name).and_then(|v| v.parse().ok())
}

fn flag_u16(args: &[String], name: &str) -> Option<u16> {
    flag_str(args, name).and_then(|v| v.parse().ok())
}
