#!/usr/bin/env bash
# Package Ferris into distributable artifacts.
#
#   AppImage  ferris-browser-<ver>-x86_64.AppImage      (desktop; glibc build)
#   Tarball   ...-x86_64-linux-gnu.tar.gz               (desktop; glibc build)
#   Tarball   ...-x86_64-static-musl.tar.gz             (headless/X11; static)
#   SHA256SUMS + md5 of the AppImage (for the AppImage release feed)
#
# The desktop artifacts use the glibc build on purpose: winit's Wayland backend
# dlopens libwayland-client at runtime, and loading host glibc libraries into a
# static musl process fails on some systems (e.g. very new glibc hosts). The
# static musl build remains as the portable headless/X11 artifact.
#
# Requirements (Debian/Ubuntu sandbox): apt-get install -y musl-tools file
#   rustup target add x86_64-unknown-linux-musl
# Optional: appimagetool (https://github.com/AppImage/appimagetool/releases) on
#   PATH or at dist/appimagetool; otherwise the AppImage step is skipped.
#
# Usage:  scripts/package.sh [extra cargo features]     (e.g. "js")
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="ferris-browser"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
FEATURES="${1:-}"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

mkdir -p dist

# --- builds ---
log "Building glibc desktop binary..."
cargo build --release ${FEATURES:+--features "$FEATURES"}
GNU="target/release/$NAME"
test -x "$GNU" || die "gnu build failed: $GNU missing"

MUSL=""
log "Building static musl binary (headless/X11 artifact)..."
if rustup target list --installed 2>/dev/null | grep -q x86_64-unknown-linux-musl \
   || rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1; then
    if cargo build --release ${FEATURES:+--features "$FEATURES"} --target x86_64-unknown-linux-musl; then
        MUSL="target/x86_64-unknown-linux-musl/release/$NAME"
    fi
fi
[ -n "$MUSL" ] || log "  (musl build skipped/unavailable)"
command -v file >/dev/null 2>&1 && file "$GNU" || true

# --- AppDir ---
log "Assembling AppDir..."
APPDIR="dist/AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
         "$APPDIR/usr/share/man/man1" \
         "$APPDIR/usr/share/doc/$NAME"
cp "$GNU" "$APPDIR/usr/bin/$NAME"
cp LICENSE LICENSE-MIT LICENSE-APACHE assets/fonts/LICENSE-DejaVu "$APPDIR/usr/share/doc/$NAME/"
cp packaging/ferris-browser.desktop "$APPDIR/usr/share/applications/"
cp packaging/ferris-browser.desktop "$APPDIR/"   # appimagetool requires it at the AppDir root
cp "packaging/hicolor/256x256/apps/$NAME.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/"
cp "packaging/$NAME.1" "$APPDIR/usr/share/man/man1/"
cp "packaging/$NAME.png" "$APPDIR/$NAME.png"   # AppImage icon (must be at AppDir root)
cat > "$APPDIR/AppRun" <<APPRUN
#!/bin/sh
HERE="\$(dirname "\$(readlink -f "\$0")")"
exec "\$HERE/usr/bin/$NAME" "\$@"
APPRUN
chmod +x "$APPDIR/AppRun"

# --- AppImage ---
APPIMAGETOOL=""
for cand in appimagetool dist/appimagetool; do
    if command -v "$cand" >/dev/null 2>&1; then APPIMAGETOOL="$cand"; break; fi
done
if [ -n "$APPIMAGETOOL" ]; then
    log "Building AppImage with $APPIMAGETOOL..."
    # extract-and-run mode (works without FUSE) needs a clean ./squashfs-root
    rm -rf squashfs-root
    ARCH=x86_64 "$APPIMAGETOOL" --appimage-extract-and-run \
        "$APPDIR" "dist/$NAME-$VERSION-x86_64.AppImage" 2>/dev/null \
        || ARCH=x86_64 "$APPIMAGETOOL" "$APPDIR" "dist/$NAME-$VERSION-x86_64.AppImage" 2>/dev/null \
        || log "AppImage step failed (AppDir is still usable in dist/AppDir)"
    rm -rf squashfs-root
else
    log "appimagetool not found -- downloading..."
    URL="https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$URL" -o dist/appimagetool || true
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$URL" -O dist/appimagetool || true
    elif command -v python3 >/dev/null 2>&1; then
        python3 -c "import urllib.request; urllib.request.urlretrieve('$URL','dist/appimagetool')" || true
    fi
    if [ -s dist/appimagetool ]; then
        chmod +x dist/appimagetool
        rm -rf squashfs-root
        ARCH=x86_64 dist/appimagetool --appimage-extract-and-run \
            "$APPDIR" "dist/$NAME-$VERSION-x86_64.AppImage" 2>/dev/null \
            || log "AppImage step failed (AppDir is still usable in dist/AppDir)"
        rm -rf squashfs-root
    else
        log "Could not obtain appimagetool; skipping AppImage (AppDir is still in dist/)."
    fi
fi

# --- tarballs ---
mk_tar() {  # $1 = binary, $2 = tarball name, $3 = build note
    mkdir -p "dist/pkgroot/$NAME/share/doc/$NAME" "dist/pkgroot/$NAME/share/man/man1"
    cp "$1" "dist/pkgroot/$NAME/$NAME"
    cp README.md LICENSE LICENSE-MIT LICENSE-APACHE assets/fonts/LICENSE-DejaVu \
       "dist/pkgroot/$NAME/share/doc/$NAME/"
    cp "$3" "dist/pkgroot/$NAME/share/doc/$NAME/BUILD-NOTE.txt"
    cp "packaging/$NAME.1" "dist/pkgroot/$NAME/share/man/man1/"
    cp "packaging/ferris-browser.desktop" "packaging/$NAME.png" "dist/pkgroot/$NAME/"
    tar -C dist/pkgroot -czf "dist/$2" "$NAME"
    rm -rf dist/pkgroot
}

log "Standalone tarballs..."
cat > dist/gnu-note.txt <<'NOTE'
Ferris -- desktop (glibc) build
===============================
Dynamically linked against glibc (needs glibc 2.36+; any distro from ~2023
onward). Wayland works by dlopening your system libwayland-client, exactly
like a distro-packaged browser. X11 needs no external libraries at all.

Install:  copy ferris-browser into ~/.local/bin (or /usr/local/bin),
          the .desktop file + icon into your hicolor theme dirs,
          the man page into share/man/man1.
NOTE
cat > dist/musl-note.txt <<'NOTE'
Ferris -- static (musl) build
=============================
Zero runtime dependencies. Ideal for servers, containers and headless use
(serve / shot / info / jscorpus). The window shell works on X11 desktops;
on Wayland it must dlopen the host libwayland-client into a static binary,
which some systems refuse (notably hosts with very new glibc). For Wayland
desktops use the glibc build instead.
NOTE
mk_tar "$GNU" "$NAME-$VERSION-x86_64-linux-gnu.tar.gz" dist/gnu-note.txt
[ -n "$MUSL" ] && mk_tar "$MUSL" "$NAME-$VERSION-x86_64-static-musl.tar.gz" dist/musl-note.txt
rm -f dist/gnu-note.txt dist/musl-note.txt

# --- checksums ---
log "Checksums..."
( cd dist
  { sha256sum "$NAME-$VERSION-x86_64.AppImage" \
              "$NAME-$VERSION-x86_64-linux-gnu.tar.gz"
    [ -f "$NAME-$VERSION-x86_64-static-musl.tar.gz" ] && \
        sha256sum "$NAME-$VERSION-x86_64-static-musl.tar.gz"
    md5sum "$NAME-$VERSION-x86_64.AppImage"
  } > SHA256SUMS )

log "Done:"
ls -lh dist/*.AppImage dist/*.tar.gz dist/SHA256SUMS 2>/dev/null || true
