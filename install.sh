#!/usr/bin/env bash
#
# Install Hylki for the current user: builds the release binary and installs it
# along with the app icon and desktop entry into the XDG user prefix, so it
# appears (with its icon) in the GNOME dash, overview, and alt-tab.
#
# Usage:  ./install.sh            # installs into ~/.local
#         PREFIX=/usr ./install.sh   # system-wide (run with sudo)
set -euo pipefail

APP_ID="co.hyprlab.Hylki"
PREFIX="${PREFIX:-$HOME/.local}"
ROOT="$(cd "$(dirname "$0")" && pwd)"

echo "==> Building release binary"
( cd "$ROOT" && cargo build --release )

echo "==> Installing binary to $PREFIX/bin/hylki"
install -Dm755 "$ROOT/target/release/hylki" "$PREFIX/bin/hylki"

echo "==> Installing icons"
for size in 256x256 512x512; do
    install -Dm644 "$ROOT/data/icons/hicolor/$size/apps/$APP_ID.png" \
        "$PREFIX/share/icons/hicolor/$size/apps/$APP_ID.png"
done
install -Dm644 "$ROOT/data/icons/hicolor/scalable/apps/$APP_ID.svg" \
    "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
# (Symbolic UI icons are embedded in the binary as a GResource, so nothing to
# install here — they render identically regardless of the host icon theme.)

echo "==> Installing desktop entry"
install -d "$PREFIX/share/applications"
msgfmt --desktop --template="$ROOT/data/$APP_ID.desktop" -d "$ROOT/po" \
    -o "$PREFIX/share/applications/$APP_ID.desktop"

echo "==> Installing translations"
for po in "$ROOT"/po/*.po; do
    [ -e "$po" ] || continue
    lang=$(basename "$po" .po)
    install -d "$PREFIX/share/locale/$lang/LC_MESSAGES"
    msgfmt -o "$PREFIX/share/locale/$lang/LC_MESSAGES/hylki.mo" "$po"
done

echo "==> Updating caches"
gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true

echo "==> Done. Launch 'hylki' (you may need to log out/in for the shell icon)."
