#!/usr/bin/env bash
#
# Build the native (non-Flatpak) packages for Fedora and Debian/Ubuntu.
#
#   tools/build-packages.sh            # builds both
#   tools/build-packages.sh rpm        # builds the RPM
#   tools/build-packages.sh deb        # builds the Debian package
#
# Output lands in packaging/out/:
#   hylki-<ver>-1.fc44.<arch>.rpm       - packaged from the host (Fedora) cargo build
#   hylki_<ver>-1_<arch>.deb            - packaged from a Debian 13 cargo build
#                                         (a beta's <ver> is 1.43.0~beta.8 in both)
#
# Each package wraps a release binary built against its own distribution's
# libraries. The RPM's comes from the host. The Debian package's comes from a
# Debian 13 container (podman), because a binary built on Fedora links against
# a newer C library than Debian has; Debian 13 is the oldest release with the
# libadwaita 1.7 the app needs, and what builds there runs on the Ubuntu
# releases that have it too. Run on a Debian host with the build dependencies
# from docs/BUILDING.md and dpkg-dev, the deb is built in place.
set -euo pipefail

APP_ID="co.hyprlab.Hylki"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/packaging/out"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
# RPM and Debian versions can't hold a dash in that place: a beta's
# 1.43.0-beta.8 becomes 1.43.0~beta.8, which both sort before 1.43.0, so the
# stable release replaces the beta.
PKG_VERSION="${VERSION//-/\~}"
WHAT="${1:-all}"
DEB_IMAGE="${HYLKI_DEB_IMAGE:-docker.io/library/rust:1-trixie}"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
# /out is the host's packaging/out when the deb build re-ran this script in
# its container.
[ -n "${HYLKI_DEB_CONTAINER:-}" ] && OUT=/out
mkdir -p "$OUT"

# The payload both packages carry, staged into $1: the release binary, the
# launcher and metainfo with their translated fields merged in, a message
# catalogue per po/<lang>.po (see po/README.md), and the icons.
stage_payload() {
    local stage="$1" po lang size
    mkdir -p "$stage/icons/256x256" "$stage/icons/512x512" "$stage/icons/scalable" "$stage/icons/symbolic"
    cp "$ROOT/target/release/hylki"              "$stage/hylki"
    cp "$ROOT/LICENSE"                          "$stage/LICENSE"
    msgfmt --desktop --template="$ROOT/data/$APP_ID.desktop" -d "$ROOT/po" -o "$stage/$APP_ID.desktop"
    msgfmt --xml --template="$ROOT/data/$APP_ID.metainfo.xml" -d "$ROOT/po" -o "$stage/$APP_ID.metainfo.xml"
    for po in "$ROOT"/po/*.po; do
        [ -e "$po" ] || continue
        lang=$(basename "$po" .po)
        mkdir -p "$stage/locale/$lang/LC_MESSAGES"
        msgfmt -o "$stage/locale/$lang/LC_MESSAGES/hylki.mo" "$po"
    done
    for size in 256x256 512x512; do
        cp "$ROOT/data/icons/hicolor/$size/apps/$APP_ID.png" "$stage/icons/$size/$APP_ID.png"
    done
    cp "$ROOT/data/icons/hicolor/scalable/apps/$APP_ID.svg" "$stage/icons/scalable/$APP_ID.svg"
    cp "$ROOT/data/icons/hicolor/symbolic/apps/co.hyprlab.Hylki-symbolic.svg" "$stage/icons/symbolic/$APP_ID-symbolic.svg"
}

build_rpm() {
    # Keep the version in the spec file in lockstep with Cargo.toml
    sed -i "s/^Version:.*/Version:        $PKG_VERSION/" "$ROOT/packaging/fedora/hylki.spec"

    echo "==> Building release binary (host)"
    ( cd "$ROOT" && cargo build --release )

    echo "==> Staging RPM payload"
    local work stage
    work="$(mktemp -d)"
    stage="$work/hylki-$PKG_VERSION-bin"
    stage_payload "$stage"
    # The dnf repository on hylki.hyprlab.co, so an RPM installed from a
    # download keeps updating with the rest of the system.
    cp "$ROOT/packaging/fedora/hylki.repo"      "$stage/hylki.repo"
    cp "$ROOT/packaging/fedora/hylki-beta.repo" "$stage/hylki-beta.repo"
    mkdir -p "$work/rpmbuild/SOURCES"
    tar -C "$work" -cf "$work/rpmbuild/SOURCES/hylki-$PKG_VERSION-bin.tar" "hylki-$PKG_VERSION-bin"

    echo "==> rpmbuild"
    rpmbuild -bb --define "_topdir $work/rpmbuild" "$ROOT/packaging/fedora/hylki.spec"
    # x86_64 here; aarch64 when the arm64 CI job runs this on Fedora.
    cp "$work/rpmbuild/RPMS/$(rpm --eval '%{_arch}')/"hylki-*.rpm "$OUT/"
    rm -rf "$work"
    echo "==> RPM done"
}

build_deb() {
    if [ ! -f /etc/debian_version ]; then
        echo "==> Building the Debian package in $DEB_IMAGE (podman)"
        # As for the AppImage (tools/build-appimage.sh): the tree is copied in
        # so target/ is never shared with the host's builds, named volumes keep
        # the container's target/ and crate downloads between runs, and
        # `label=disable` spares relabelling the working tree for SELinux.
        podman run --rm \
            --security-opt label=disable \
            -e HYLKI_DEB_CONTAINER=1 \
            -v "$ROOT":/src:ro \
            -v "$OUT":/out \
            -v hylki-deb-target:/build/target \
            -v hylki-deb-cargo:/usr/local/cargo/registry \
            "$DEB_IMAGE" \
            sh -euc '
                mkdir -p /build
                tar -C /src --exclude=./target --exclude=./temp --exclude=./.git \
                            --exclude=./dist --exclude=./packaging/out \
                            --exclude=./.flatpak-builder -cf - . \
                    | tar -C /build -xf -
                exec /build/tools/build-packages.sh deb
            '
        return
    fi

    if [ -n "${HYLKI_DEB_CONTAINER:-}" ]; then
        echo "==> Installing build dependencies"
        apt-get update -qq
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
            build-essential pkg-config dpkg-dev gettext \
            libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libpoppler-glib-dev \
            libssl-dev libwayland-dev >/dev/null
    fi

    echo "==> Building release binary ($(. /etc/os-release && echo "$PRETTY_NAME"))"
    ( cd "$ROOT" && cargo build --release )

    echo "==> Staging Debian package"
    local work stage pkg arch deps mo
    work="$(mktemp -d)"
    stage="$work/payload"
    pkg="$work/hylki"
    arch="$(dpkg --print-architecture)"
    stage_payload "$stage"
    install -Dm755 "$stage/hylki" "$pkg/usr/bin/hylki"
    install -Dm644 "$stage/$APP_ID.desktop" "$pkg/usr/share/applications/$APP_ID.desktop"
    install -Dm644 "$stage/$APP_ID.metainfo.xml" "$pkg/usr/share/metainfo/$APP_ID.metainfo.xml"
    for size in 256x256 512x512 scalable symbolic; do
        for f in "$stage/icons/$size"/*; do
            install -Dm644 "$f" "$pkg/usr/share/icons/hicolor/$size/apps/$(basename "$f")"
        done
    done
    # The binary looks for its catalogues under share/locale beside itself
    # (src/i18n.rs).
    for mo in "$stage"/locale/*/LC_MESSAGES/hylki.mo; do
        [ -e "$mo" ] || continue
        install -Dm644 "$mo" "$pkg/usr/share/${mo#"$stage"/}"
    done
    # The apt repository on hylki.hyprlab.co, the same files the site serves
    # at /deb/hylki.sources and /deb/hylki-beta.sources (key included), so a
    # package installed from a download keeps updating with the rest of the
    # system. The beta's is switched off, as in the RPM: a tester turns it on
    # once, and every package, stable or beta, carries it, so the first stable
    # to overtake a beta does not remove it. Both are conffiles: a user's
    # edits are kept on upgrade.
    install -Dm644 "$ROOT/packaging/debian/hylki.sources" "$pkg/etc/apt/sources.list.d/hylki.sources"
    install -Dm644 "$ROOT/packaging/debian/hylki-beta.sources" "$pkg/etc/apt/sources.list.d/hylki-beta.sources"
    mkdir -p "$pkg/DEBIAN" "$pkg/usr/share/doc/hylki"
    printf '/etc/apt/sources.list.d/hylki.sources\n/etc/apt/sources.list.d/hylki-beta.sources\n' > "$pkg/DEBIAN/conffiles"
    {
        printf 'Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n'
        printf 'Upstream-Name: Hylki\nSource: https://github.com/hyprlab/hylki\n\n'
        printf 'Files: *\nCopyright: Hyprlab <hyprlab@proton.me>\nLicense: AGPL-3.0-or-later\n'
        sed 's/^[[:space:]]*$/./; s/^/ /' "$stage/LICENSE"
    } > "$pkg/usr/share/doc/hylki/copyright"

    # dpkg-shlibdeps turns the libraries the binary links into package names
    # and minimum versions; it wants a source control file to exist.
    mkdir -p "$work/debian"
    printf 'Source: hylki\n\nPackage: hylki\nArchitecture: any\n' > "$work/debian/control"
    deps="$(cd "$work" && dpkg-shlibdeps -O -e"$pkg/usr/bin/hylki" 2>/dev/null)"
    deps="${deps#shlibs:Depends=}"
    [ -n "$deps" ] || { echo "dpkg-shlibdeps found no dependencies" >&2; exit 1; }
    # The app's icons are SVG, which GTK reads through a gdk-pixbuf loader
    # that no linked library names: Debian's GTK only recommends it, so an
    # install without recommends drew warnings and blank icons (#547). Newer
    # releases load images through glycin instead.
    deps="$deps, librsvg2-common | glycin-loaders"
    # Renamed from "veem" in 1.6.0 and from "vireo" in 1.35.0; the Debian
    # packages of either (up to 1.7.0) are replaced, and user data carries
    # over on first start.
    cat > "$pkg/DEBIAN/control" <<EOF
Package: hylki
Version: $PKG_VERSION-1
Section: mail
Priority: optional
Architecture: $arch
Maintainer: Hyprlab <hyprlab@proton.me>
Installed-Size: $(du -sk --exclude=DEBIAN "$pkg" | cut -f1)
Depends: $deps
Recommends: gnome-keyring, gnupg, python3-nautilus
Conflicts: veem, vireo
Replaces: veem, vireo
Homepage: https://hylki.hyprlab.co
Description: Clean, fast GNOME-native email client
 Hylki is a desktop email client for Wayland that feels at home in GNOME. It
 talks IMAP/SMTP directly, keeps your mail and credentials on your machine,
 and blocks trackers by default - no telemetry, no analytics.
EOF

    echo "==> dpkg-deb"
    dpkg-deb --build --root-owner-group "$pkg" "$OUT/hylki_${PKG_VERSION}-1_${arch}.deb"
    rm -rf "$work"
    echo "==> DEB done"
}

case "$WHAT" in
    rpm) build_rpm ;;
    deb) build_deb ;;
    all) build_rpm; build_deb ;;
    *)   echo "usage: $0 [rpm|deb|all]" >&2; exit 1 ;;
esac

[ -n "${HYLKI_DEB_CONTAINER:-}" ] && exit 0
echo "==> Packages in $OUT:"
ls -1 "$OUT"
