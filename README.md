# Ferris 🦞 — a browser engine written in Rust

Ferris is a from-scratch **browser engine**: it fetches a real web page over
TLS, parses the HTML into a DOM, parses and cascades CSS, runs block / inline /
table layout, rasterizes the result to pixels, and serves the whole thing
behind a clickable browser UI.

There is **no Chromium, no WebKit, no Gecko, and no system browser** involved.
The only things borrowed from the ecosystem are the parts a real engine also
borrows: an HTML5 tokenizer/parser (`html5ever`, the parser Servo uses), a CSS
selector matcher (`selectors`), a rasterizer (`tiny-skia`), a font rasterizer
(`ab_glyph`), TLS (`rustls`), and image decoding (`image`). Everything between
them — the cascade, the layout algorithms, the display list, the painter, the
text shaper-substitute, the HTTP server and the browser chrome — is in this
repo.

```
URL
 └─> net        HTTP/HTTPS over rustls, redirects, gzip/brotli, response cache
      └─> DOM     html5ever parse (via scraper) -> node tree
           └─> css      stylesheet parse: at-rules, specificity, !important
                └─> style    cascade (UA -> author -> inline) + inheritance
                     └─> layout   block, inline w/ line breaking, tables, lists
                          └─> paint     display list -> rasterized pixels
                               └─> server   HTTP UI with clickable link regions
```

## Running it

```bash
cargo build --release

# the browser UI (default port 8080)
./target/release/ferris-browser serve --port 8080

# rasterize one page to a PNG, headless
./target/release/ferris-browser shot https://news.ycombinator.com/ -o hn.png --width 1000

# print render metadata
./target/release/ferris-browser info https://example.com/
```

Then open `http://localhost:8080/`. Type an address, press Enter. Ferris
renders the page itself and overlays an invisible anchor on every link region
it computed during layout, so **links are actually clickable** and navigation
works end-to-end.

## Running it on CachyOS / Arch Linux

One script does the whole thing:

```bash
git clone https://github.com/yeahseven7/ferris-browser.git
cd ferris-browser
./scripts/install-cachyos.sh --run      # installs deps, builds, serves on :8080
```

What it installs and why:

| Package | Why |
|---|---|
| `base-devel` | `gcc`/`make` — the `ring` crate (rustls' crypto) compiles C via `cc` |
| `rust` | rustc + cargo (a rustup toolchain works identically) |
| `mold` *(optional)* | much faster link step; auto-detected if present |

Notably **nothing else is required**: TLS is rustls (no OpenSSL), fonts are
embedded in the binary (no fontconfig), and the UI is served over plain HTTP, so
it runs headless or on a desktop.

The script builds with `-C target-cpu=native` to match CachyOS's own
build-for-your-CPU philosophy. That binary will not run on other CPUs; drop the
flag (or set `RUSTFLAGS=`) if you need portability.

First `cargo build` compiles ~200 dependency crates and takes a few minutes;
incremental rebuilds are seconds.

To run it as a background service instead of a foreground terminal:

```ini
# /etc/systemd/system/ferris.service   (or a --user unit)
[Unit]
Description=Ferris browser engine
After=network-online.target

[Service]
ExecStart=/path/to/ferris/target/release/ferris-browser serve --port 8080
Restart=on-failure
# bind tighter than 0.0.0.0 if you only want localhost:
# Environment=PORT=8080

[Install]
WantedBy=multi-user.target
```

## The native window

`ferris window [url]` opens a real desktop window (winit + softbuffer, Wayland
or X11) showing exactly the pixels this engine rasterized — no other browser
engine renders anything. The whole chrome — tab strip, editable address bar,
status strip — is drawn by Ferris itself with its own glyph rasterizer:
tab titles, URLs, the text caret and the blinking are all engine pixels.

Each tab owns its page, its scroll position and its history. The address bar
is a real text field: click it (or Ctrl+L / F6) to focus with the URL
selected, type over it, move the caret, Enter to go, Esc to cancel.

The new-tab start page offers a few YouTube clips; clicking one — or pasting any
YouTube URL into the address bar — does **not** load YouTube's script-heavy
page. Ferris hands the link to `mpv`, which resolves and plays the stream via
`yt-dlp`, and the status strip reports `opened in mpv: …` (or asks you to
install mpv + yt-dlp). Ferris itself ships no video codec.

| Input | Action |
|---|---|
| click a link | navigate (hit-tests the link rects layout computed) |
| click tab / its × / the + | switch tab / close tab / new tab |
| Ctrl+T / Ctrl+W / Ctrl+Tab / Ctrl+Shift+Tab | new / close / next / previous tab |
| Ctrl+L or F6, click the bar | focus address bar, URL selected |
| typing, ← → Home End, Backspace, Delete, Ctrl+A | edit the address |
| Enter (focused) | normalize + navigate the typed address |
| Esc (focused) | cancel editing, restore the current URL |
| wheel / ↑ ↓ / PgUp PgDn / Home End / Space | scroll |
| Alt+← or Backspace / Alt+→ | back / forward (per tab) |
| F5 | reload |
| Esc or Ctrl+Q (page focused) | quit |

Frame composition, tab geometry, chrome hit-testing, scrolling, history and
address-bar editing are pure functions, so the same code the window runs is
exercised headlessly by `ferris shelltest <url> [more urls…]`, which also
writes the exact frames the window would present:
`shots/window-frame.png` (idle), `…-focus.png` (focused bar + caret),
`…-tabs.png` (one real tab per URL given). On a machine with no display
server, `ferris window` exits with guidance instead of crashing.

## JavaScript (feature `js`)

`cargo build --release --features js` links [Boa](https://boajs.dev), a JS
engine written in Rust, and turns Ferris into a scripting browser:

* the fetched HTML is mirrored into a small **mutable DOM arena**
  (`src/dom.rs`) — pure Rust, no JS types in it;
* `src/js.rs` binds Boa to that mirror: `document` (getElementById,
  querySelector(All), getElementsBy*, createElement/TextNode, write, title,
  body/head/documentElement), element objects (appendChild/removeChild/
  remove, attributes, classList, innerHTML/textContent, children,
  parentElement, `style.setProperty` which feeds our CSS cascade),
  `console.*` capture, `location`, `navigator`, `document.cookie` and
  `localStorage`/`sessionStorage` (wired to the real persistent cookie jar and
  an on-disk per-origin store), `setTimeout` (fires immediately — there is no
  event loop between paints yet);
* classic `<script>` tags (inline and fetched `src`) run **in document order**
  in one context before the immutable pipeline parses the mutated HTML;
* runaway scripts are contained by Boa's loop-iteration and recursion limits;
  script errors are captured per page (`/info` shows them) instead of killing
  the render.

The DOM mirror is the swappable shim: replacing the Boa binding layer (or
making the mirror live, post-paint) does not touch fetch/CSS/layout/paint.

`ferris jscorpus [live urls…]` runs the compatibility corpus: ten offline
fixture pages in `corpus/` (DOM writes, creation, innerHTML+selectors,
classList, attributes, style→cascade, console/navigator/location, runaway-loop
containment, document.write/title, script ordering+timers) through the real
preprocess→render path, checking each marker text lands in the rendered page;
extra URLs are fetched and reported live. Exit code 1 on any failure.

Build notes: the `js` feature needs a machine with ≳2 GB RAM for the Boa
compile (this sandbox has 1 GB and can only `cargo check` it — the corpus and
the `js` unit tests first run where you build). `--features js-date`
additionally enables `Date`/Temporal. Without the feature nothing JS-related
is compiled and the engine behaves exactly as before.

## Packaging (universal, no root needed)

The crate is named **`ferris-browser`** (binary `ferris-browser`) to avoid
colliding with the unrelated `ferris` package in the AUR.

    scripts/package.sh                # glibc desktop + static musl + AppImage
    FEATURES=js scripts/package.sh    # same, with the Boa JavaScript engine

What it produces in `dist/`:

| Artifact | What it is |
|---|---|
| `ferris-browser-<ver>-x86_64.AppImage` | **the desktop artifact**: glibc build in a single executable with desktop integration (AppRun, `.desktop`, hicolor icons, man, licences) — runs on any x86-64 distro with glibc 2.36+ (~2023 onward), Wayland or X11 |
| `ferris-browser-<ver>-x86_64-linux-gnu.tar.gz` | same glibc binary + man/`.desktop`/icon/licences for a `~/.local/bin` install |
| `ferris-browser-<ver>-x86_64-static-musl.tar.gz` | fully static, zero dependencies: servers, containers, headless use (`serve`/`shot`/`info`/`shelltest`/`jscorpus`) and X11 desktops |
| `SHA256SUMS` | sha256 of every artifact + the md5 AppImage release feeds want |

Why two flavors: the Wayland backend `dlopen`s the system
`libwayland-client.so.0` at startup. That works from a normal glibc binary,
but loading host glibc libraries into a *static musl* process fails on some
systems (e.g. hosts with very new glibc refuse it), so static builds are
positioned for headless and X11 use — X11 needs no external libraries at all
(x11rb speaks the protocol in pure Rust). The glibc build is what a desktop
should run; it needs glibc 2.36+ and otherwise nothing beyond the system's
Wayland or X11.

Build requirements: a musl cross toolchain for the static artifact (CachyOS/
Arch: `pacman -S musl`; Debian/Ubuntu: `apt install musl-tools`) and its
rustup target (added automatically; if absent the static artifact is simply
skipped); `appimagetool` is downloaded into `dist/` on first run. On a
no-FUSE system run the AppImage with `--appimage-extract-and-run`. The
embedded DejaVu fonts' licence and both code licences ship inside every
artifact, as the DejaVu licence requires.

For day-to-day development on CachyOS the source path stays
`scripts/install-cachyos.sh`; a PKGBUILD can wrap the static binary later if
you want it in the AUR.

## Roadmap status

- [x] engine: fetch → DOM → CSS → layout → paint → HTTP UI
- [x] native window shell (scroll, click-to-navigate, history, chrome text)
- [x] in-window address bar + tabs (per-tab history/scroll, caret, select-all)
- [x] JavaScript via Boa behind a `js` feature, with a swappable DOM shim
      (`ferris jscorpus` = the compat corpus)
- [x] session layer: persistent cookies + form POST + `document.cookie` /
      `localStorage` / `sessionStorage` (`examples/auth_demo` proves it live)
- [x] YouTube hand-off: start-page cards + any YouTube URL open in `mpv`
  (via `yt-dlp`) instead of loading the script-heavy page
- [ ] replaced-element (`<img>`) line-box/paint alignment so thumbnails sit
  correctly in mixed text flow (enables image thumbnail cards)
- [ ] interactive forms in the window: type into `<input>`/`<textarea>`, click
      submit buttons, file uploads, `<select>`
- [ ] fidelity: floats/positioning, flexbox, shaping (cosmic-text), SVG (resvg)
- [x] packaging: static musl binary + AppImage + licences/icon/.desktop/man
      (`scripts/package.sh`, crate renamed `ferris-browser`)

## HTTP API

| Route | Returns |
|---|---|
| `GET /` | the browser chrome UI (address bar, back/forward/reload, status bar) |
| `GET /view?url=…&w=…&img=0\|1` | the rasterized page as an `<img>` plus one absolutely-positioned `<a>` per link rectangle |
| `GET /render?url=…&w=…` | the raw PNG of the rasterized page |
| `GET /info?url=…&w=…` | JSON: status, title, canvas size, link count, node/line/paint-op stats, per-stage timings |
| `GET /health` | `ok` |

## What it implements

**Network** — HTTP and HTTPS (rustls), redirect following, gzip/brotli
decompression, charset handling, a shared in-memory response cache, browser-style
error classification (DNS / connection / timeout / TLS / HTTP status).

**Sessions & forms** — a persistent **cookie jar**: `Set-Cookie` is captured and
the matching `Cookie` header is attached across requests *and* redirects
(RFC 6265 domain/path/secure rules), saved to
`~/.local/share/ferris-browser/cookies.json` so logins survive restarts
(session-only cookies are correctly *not* persisted). **POST** with
`application/x-www-form-urlencoded` form submission is supported end-to-end.
Scripts get `document.cookie`, `localStorage` (persisted per-origin to
`~/.local/share/ferris-browser/localStorage/`) and `sessionStorage`. Run
`cargo run --release --example auth_demo` for a live proof against httpbin.org.

**CSS** — a hand-written parser (comments, at-rules incl. `@media`/`@supports`,
selector lists, declarations, `!important`), specificity computation, the full
cascade order (user-agent stylesheet → author sheets in document order →
inline `style`), property inheritance, unit resolution (`px em rem pt pc in cm
mm %`), box shorthands (`margin`, `padding`, `border`), `background`,
`font` shorthand, `text-align`, `text-decoration`, `text-transform`,
`white-space`, `list-style-type`, named colours + `#rgb`/`#rrggbb`/`#rrggbbaa`/
`rgb()`/`rgba()`/`hsl()`, and a CSS 2.1 Appendix D user-agent stylesheet.

**Layout** — block formatting contexts with adjacent-sibling margin
collapsing; inline formatting contexts with greedy word wrapping and per-line
baseline/metrics resolution; `text-align: left|center|right`; a real table
algorithm (max-content column measurement, proportional fit, **`colspan`**);
list markers (`disc`/`circle`/`square`/`decimal`); replaced elements
(`<img>` with intrinsic/attribute/CSS sizing and aspect-ratio preservation,
plus `data:` URIs); `inline-block` boxes measured up front for line boxes;
`<br>`, `<hr>`, `white-space: pre`.

**Paint** — a display list consumed by a `tiny-skia` rasterizer: filled rects,
four-sided borders, glyph coverage rasterization from embedded DejaVu fonts
(regular/bold, sans/serif/mono), underlines/strikethroughs, list bullets,
bilinearly-scaled images, and broken-image placeholders. Fonts are
`include_bytes!`-embedded so the binary is self-contained.

**Robustness** — hard budgets on paint ops, node count, recursion depth, page
height, image count and body size, so hostile or enormous pages degrade instead
of hanging or OOMing; error pages are rendered *through the same engine*.

## What it does **not** do (honest limitations)

- **JavaScript is optional and pre-render only.** With `--features js`, scripts
  run once against a DOM *mirror* before paint (see the JS section). There is no
  live event loop, no DOM events (`click`/`input`/`load` handlers don't fire),
  and no re-layout after script mutation, so **single-page apps still won't
  work** — pages that render entirely client-side show a blank shell.
- **Inline `<img>` in mixed text flow can misposition.** Replaced elements are
  placed at `baseline - height` and can overflow their line box, so a thumbnail
  beside/after text may paint offset. Images render correctly in their own block
  (which is why the start-page video cards are text links, not thumbnails, until
  the layout item above lands).
- **No media, no video, no DRM.** There is no `<video>`/`<audio>`, no Media
  Source Extensions, no WebGL/canvas, and no Widevine/Playfair. **YouTube and
  other streaming sites cannot play video** — this needs a licensed DRM module
  that a hobby engine can't ship.
- **Big-tech OAuth (e.g. "Sign in with Google") is blocked at the source.**
  Google and similar providers *deliberately refuse* embedded/non-standard
  browsers ("This browser or app may not be secure"), independent of Ferris.
  Classic form logins (username + password POSTed to a site's own endpoint) do
  work now that cookies + POST exist.
- **No interactive form fields yet.** `<input>`/`<textarea>` render but you
  can't type into them in the window shell, and submit buttons don't POST —
  the network/cookie/POST layer beneath them is done (see `render_post`), the
  on-screen input widgets are the next step.
- **No `float`, no absolute/fixed positioning, no flexbox/grid.** `flex`/`grid`
  fall back to block layout, so modern site chrome can look stacked rather than
  side-by-side.
- **No `max-width`/`min-width`**, only `width`/`height`.
- **No italic synthesis** — no italic font is bundled, so italics render upright.
- **No SVG rendering** (`<img src="*.svg">` and inline `<svg>` fail to decode),
  which is why some sites report "images failed".
- **Basic kerning, no full shaping** — no ligatures, no bidi, no complex-script
  reordering. Missing glyphs fall back to the sans face.
- **No `colspan`-adjacent `rowspan`**, no nested-table-aware column sizing beyond
  skipping nested tables.
- **Page height is capped** at 12 000 px (a full-page canvas is ~4 bytes/pixel);
  longer pages are clipped and the UI says so.
- **`<center>`/`text-align` inheritance is spec-literal**, so sites that wrap
  content in `<center>` (Hacker News does) get centred text inside table cells.

## Verification

```bash
cargo test --release        # 67 tests: unit + layout/paint integration + shell logic + real-socket HTTP
cargo clippy --release --all-targets   # 0 warnings, 0 errors
```

Tests include end-to-end pipeline assertions (HTML+CSS in → display list with
expected colours and absolutised link targets out), depth/budget limits,
table cell emission, colour parsing tables, and an HTTP server test that binds a
real socket and asserts on the wire format.

Measured on real sites (1000–1100 px viewport):

| Page | Nodes | Lines | Paint ops | Links | Total |
|---|---|---|---|---|---|
| news.ycombinator.com | 3 318 | 107 | 1 312 | 198 | ~1.8 s (incl. fetch) |
| www.rust-lang.org | 1 061 | 350 | 847 | 36 | ~0.7 s |
| motherfuckingwebsite.com | 90 | 69 | 630 | 0 | ~1.2 s |
| info.cern.ch (plain HTTP) | 29 | 17 | 64 | 4 | ~0.7 s |
| example.com | 3 | 3 | 29 | 0 | ~30 ms |

Sample renders live in `shots/`.

## Layout of the code

| File | Responsibility |
|---|---|
| `src/net.rs` | fetching, TLS, redirects, cache, error taxonomy, **cookie jar (persistent)**, **POST/form submit** |
| `src/css.rs` | stylesheet/declaration parsing, specificity |
| `src/style.rs` | cascade, computed style, UA stylesheet, value parsing |
| `src/layout.rs` | block/inline/table layout → display list + link rects |
| `src/paint.rs` | display list → pixels (tiny-skia + ab_glyph) |
| `src/font.rs` | embedded fonts, metrics, glyph rasterization |
| `src/color.rs` | colour parsing (named + functional notation) |
| `src/engine.rs` | pipeline orchestration, error pages, `render`/`render_post` (GET + form POST) |
| `src/server.rs` | HTTP/1.1 server, viewport document, browser chrome |
| `src/shell.rs` | native window (winit+softbuffer): tabs, address bar, frame compose, hit-test, history, editing |
| `src/dom.rs` | mutable DOM mirror (arena tree) that scripts mutate; serialize back into the pipeline |
| `src/js.rs` | Boa bindings behind `--features js`: document/element/console globals, `document.cookie`, `localStorage`/`sessionStorage`, limits, script collection |
| `src/corpus.rs` + `corpus/` | JS compat corpus: 10 fixtures + runner (`ferris jscorpus`) |
| `examples/auth_demo.rs` | live proof of the session layer (cookies across redirects + form POST) against httpbin.org |
| `src/main.rs` | CLI (`serve` / `shot` / `info`) |
