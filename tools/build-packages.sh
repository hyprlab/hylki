#!/usr/bin/env bash
# Build native packages from a release binary built on this host.
# Usage: tools/build-packages.sh [rpm|deb|all] (default: rpm)
# Build on the oldest distribution release you intend to support: the binary
# and its generated shared-library dependencies reflect the build host.
set -euo pipefail

APP_ID="co.hyprlab.Hylki"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/packaging/out"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
WHAT="${1:-rpm}"

case "$WHAT" in
    rpm|deb|all) ;;
    *) echo "usage: $0 [rpm|deb|all]" >&2; exit 1 ;;
esac

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
mkdir -p "$OUT"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STAGE="$WORK/hylki-$VERSION-bin"

echo "==> Building release binary (host)"
( cd "$ROOT" && cargo build --release )

echo "==> Staging package payload"
mkdir -p "$STAGE/icons/256x256" "$STAGE/icons/512x512" "$STAGE/icons/scalable" "$STAGE/icons/symbolic"
cp "$ROOT/target/release/hylki" "$STAGE/hylki"
cp "$ROOT/LICENSE" "$STAGE/LICENSE"
# Merge translated launcher and metainfo fields, and compile message catalogues.
msgfmt --desktop --template="$ROOT/data/$APP_ID.desktop" -d "$ROOT/po" -o "$STAGE/$APP_ID.desktop"
msgfmt --xml --template="$ROOT/data/$APP_ID.metainfo.xml" -d "$ROOT/po" -o "$STAGE/$APP_ID.metainfo.xml"
for po in "$ROOT"/po/*.po; do
    [ -e "$po" ] || continue
    lang=$(basename "$po" .po)
    mkdir -p "$STAGE/locale/$lang/LC_MESSAGES"
    msgfmt -o "$STAGE/locale/$lang/LC_MESSAGES/hylki.mo" "$po"
done
for size in 256x256 512x512; do
    cp "$ROOT/data/icons/hicolor/$size/apps/$APP_ID.png" "$STAGE/icons/$size/$APP_ID.png"
done
cp "$ROOT/data/icons/hicolor/scalable/apps/$APP_ID.svg" "$STAGE/icons/scalable/$APP_ID.svg"
cp "$ROOT/data/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg" "$STAGE/icons/symbolic/$APP_ID-symbolic.svg"

build_rpm() {
    # Keep the version in the spec file in lockstep with Cargo.toml.
    sed -i "s/^Version:.*/Version:        $VERSION/" "$ROOT/packaging/fedora/hylki.spec"
    mkdir -p "$WORK/rpmbuild/SOURCES"
    tar -C "$WORK" -cf "$WORK/rpmbuild/SOURCES/hylki-$VERSION-bin.tar" "hylki-$VERSION-bin"
    echo "==> rpmbuild"
    rpmbuild -bb --define "_topdir $WORK/rpmbuild" --define "_tmppath $WORK" \
        "$ROOT/packaging/fedora/hylki.spec"
    find "$WORK/rpmbuild/RPMS" -type f -name 'hylki-*.rpm' -exec cp {} "$OUT/" \;
    echo "==> RPM done"
}

build_deb() {
    command -v dpkg-deb >/dev/null
    command -v dpkg-shlibdeps >/dev/null
    local pkg arch deps mo lang
    pkg="$WORK/deb"
    arch="$(dpkg --print-architecture)"
    install -Dm755 "$STAGE/hylki" "$pkg/usr/bin/hylki"
    install -Dm644 "$STAGE/LICENSE" "$pkg/usr/share/doc/hylki/copyright"
    install -Dm644 "$STAGE/$APP_ID.desktop" "$pkg/usr/share/applications/$APP_ID.desktop"
    install -Dm644 "$STAGE/$APP_ID.metainfo.xml" "$pkg/usr/share/metainfo/$APP_ID.metainfo.xml"
    for size in 256x256 512x512; do
        install -Dm644 "$STAGE/icons/$size/$APP_ID.png" \
            "$pkg/usr/share/icons/hicolor/$size/apps/$APP_ID.png"
    done
    install -Dm644 "$STAGE/icons/scalable/$APP_ID.svg" \
        "$pkg/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
    install -Dm644 "$STAGE/icons/symbolic/$APP_ID-symbolic.svg" \
        "$pkg/usr/share/icons/hicolor/symbolic/apps/$APP_ID-symbolic.svg"
    for mo in "$STAGE"/locale/*/LC_MESSAGES/hylki.mo; do
        [ -e "$mo" ] || continue
        lang="$(basename "$(dirname "$(dirname "$mo")")")"
        install -Dm644 "$mo" "$pkg/usr/share/locale/$lang/LC_MESSAGES/hylki.mo"
    done

    # dpkg-shlibdeps reads the binary to calculate package names and minimum
    # shared-library versions. It requires a minimal source control file.
    mkdir -p "$WORK/debian" "$pkg/DEBIAN"
    printf 'Source: hylki\n\nPackage: hylki\nArchitecture: any\nDescription: Hylki\n' > "$WORK/debian/control"
    deps="$(cd "$WORK" && dpkg-shlibdeps -O -e"$pkg/usr/bin/hylki")"
    deps="${deps#shlibs:Depends=}"
    cat > "$pkg/DEBIAN/control" <<EOF
Package: hylki
Version: $VERSION
Section: mail
Priority: optional
Architecture: $arch
Maintainer: Hyprlab <hyprlab@proton.me>
Depends: $deps
Recommends: gnome-keyring, gnupg, python3-nautilus
Homepage: https://hylki.hyprlab.co
Description: Clean, fast GNOME-native email client
 Hylki talks IMAP and SMTP directly, keeps mail and credentials on the
 user's machine, and blocks trackers by default.
EOF
    echo "==> dpkg-deb"
    dpkg-deb --build --root-owner-group "$pkg" "$OUT/hylki-${VERSION}-${arch}.deb"
    echo "==> DEB done"
}

case "$WHAT" in
    rpm) build_rpm ;;
    deb) build_deb ;;
    all) build_rpm; build_deb ;;
esac

echo "==> Packages in $OUT:"
ls -1 "$OUT"
