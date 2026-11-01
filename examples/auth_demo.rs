//! Live demonstration of Ferris's session layer: cookies + form POST.
//!
//! Run with:  cargo run --release --example auth_demo
//!
//! It talks to httpbin.org, which echoes exactly what a real server would
//! see, so we can prove the cookie jar and form submission work end-to-end:
//!
//!   1. GET /cookies/set?session=…  -> server sends Set-Cookie and redirects.
//!   2. GET /cookies                -> our jar sends that cookie back.
//!   3. POST /post with form fields -> server echoes the parsed form *and*
//!      the Cookie header it received, so the session spans the whole trip.
//!
//! No credentials, no accounts: this is the transport foundation that every
//! real login is built on.

use ferris::net;

fn body_str(r: &net::FetchResult) -> String {
    r.text()
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("reload") {
        reload();
        return;
    }
    println!("Ferris session demo — cookies + form POST");
    println!("cookie file: {:?}\n", net::data_dir().join("cookies.json"));

    // Start from a clean slate so the demo is deterministic.
    net::clear_cookies();
    assert_eq!(net::cookie_count(), 0);

    // 1) Ask httpbin to set a session cookie. This responds 302 -> /cookies,
    //    and our agent follows the redirect while storing the Set-Cookie.
    let set = net::normalize("https://httpbin.org/cookies/set?session=deadbeef123&user=ada")
        .expect("valid url");
    match net::fetch(&set, net::MAX_BODY_BYTES) {
        Ok(r) => {
            println!("[1] GET /cookies/set -> {} {}", r.status, r.final_url);
            println!("    jar now holds {} cookie(s)", net::cookie_count());
            println!("    document.cookie would read: {}", net::cookies_as_header());
            let _ = body_str(&r);
        }
        Err(e) => {
            eprintln!("[1] network error (is httpbin reachable?): {e}");
            std::process::exit(2);
        }
    }

    // 2) A separate GET to /cookies must carry the session cookie back.
    let get = net::normalize("https://httpbin.org/cookies").unwrap();
    match net::fetch(&get, net::MAX_BODY_BYTES) {
        Ok(r) => {
            let body = body_str(&r);
            println!("\n[2] GET /cookies -> {}", r.status);
            let ok = body.contains("deadbeef123");
            println!("    server saw our cookie: {ok}");
            println!("    body: {}", body.trim());
            assert!(ok, "cookie was NOT sent back — jar is broken");
        }
        Err(e) => {
            eprintln!("[2] network error: {e}");
            std::process::exit(2);
        }
    }

    // 3) Submit a form via POST; httpbin echoes the parsed form AND headers.
    let post = net::normalize("https://httpbin.org/post").unwrap();
    let fields = vec![
        ("username".to_string(), "ada lovelace".to_string()),
        ("password".to_string(), "s3cr&t pw".to_string()),
        ("remember".to_string(), "on".to_string()),
    ];
    println!("\n[3] POST /post  form={}", net::encode_form(&fields));
    match net::submit_form(&post, &fields, net::MAX_BODY_BYTES) {
        Ok(r) => {
            let body = body_str(&r);
            println!("    -> {} {}", r.status, r.content_type);
            let form_ok = body.contains("ada lovelace") && body.contains("remember");
            let cookie_ok = body.contains("deadbeef123");
            println!("    server parsed our form fields: {form_ok}");
            println!("    server also got the session cookie on the POST: {cookie_ok}");
            // Show just the "form" and "headers.Cookie" bits for brevity.
            for line in body.lines() {
                let t = line.trim();
                if t.contains("username")
                    || t.contains("password")
                    || t.contains("remember")
                    || t.contains("\"Cookie\"")
                    || t.contains("deadbeef")
                {
                    println!("      {t}");
                }
            }
            assert!(form_ok, "form fields were not echoed back");
        }
        Err(e) => {
            eprintln!("[3] network error: {e}");
            std::process::exit(2);
        }
    }

    // 4) Persist, so a restart would resume the same session. Session cookies
    //    (no Max-Age/Expires, like httpbin's) are intentionally NOT written to
    //    disk — that matches real browsers. So add a *persistent* cookie and
    //    show that it, and only it, lands in the file.
    let persist_url = net::normalize("https://httpbin.org/").unwrap();
    net::set_cookie_from_script(&persist_url, "login_token=xyz789; path=/; Max-Age=3600");
    net::persist_cookies();
    let path = net::data_dir().join("cookies.json");
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    println!(
        "\n[4] persisted to {path:?}\n    on-disk contents: {}",
        saved.trim()
    );
    let persistent_saved = saved.contains("login_token");
    println!("    persistent cookie written to disk: {persistent_saved}");
    println!("    (session cookies 'session'/'user' correctly skipped: {})",
             !saved.contains("deadbeef123"));
    assert!(persistent_saved, "persistent cookie was not saved to disk");

    println!("\nALL GOOD: cookies flow across redirects and requests, forms POST");
    println!("correctly, and persistent cookies survive to disk. Re-run with");
    println!("`-- reload` in a FRESH process to see the login_token come back.");
}

/// Fresh-process check: don't clear, just let the agent load cookies.json and
/// report what survived the restart.
fn reload() {
    println!("Ferris session demo — RELOAD (fresh process)");
    println!(
        "loaded {} cookie(s) from {:?}",
        net::cookie_count(),
        net::data_dir().join("cookies.json")
    );
    println!("document.cookie reads: {}", net::cookies_as_header());
    let survived = net::cookies_as_header().contains("login_token=xyz789");
    println!("persistent login_token survived the restart: {survived}");
    assert!(survived, "persistent cookie did not reload from disk");
    println!("ALL GOOD: session persisted across process restarts.");
}
