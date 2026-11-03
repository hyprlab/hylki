#!/usr/bin/env bash
#
# Build the AppImage (#235).
#
#   tools/build-appimage.sh          # builds it in a container, from the host
#   HYLKI_APPIMAGE_SRC=<checkout> tools/build-appimage.sh
#                                    # builds another checkout (a tag, the
#                                    # beta worktree) with this script
#
# Output lands in packaging/out/:
#   Hylki-<arch>.AppImage            - the stable bundle
#   Hylki-Beta-<arch>.AppImage       - the beta, when the version is X.Y.Z-beta.N
#
# A beta is the beta app, as in the Flatpak: built with the `beta` feature,
# app ID co.hyprlab.Hylki.Beta, its own launcher and icon. Outside the
# Flatpak both read ~/.config/hylki, so a beta tester keeps their accounts.
#
# Both update from hylki.hyprlab.co, which serves the newest AppImage of each
# channel and its .zsync file. Flatout writes the same address in again when
# the bundle is published, and makes the .zsync itself.
#
# Unlike the RPM, this carries every library the app needs, down to glibc and
# the dynamic linker, so one file runs on any distribution and any libc. The
# machinery is pkgforge's: `quick-sharun` collects the libraries (including the
# dlopened ones, which ldd never sees), rewrites the binaries to load them from
# inside the bundle, and packs the result with appimagetool. See
# https://github.com/pkgforge-dev/Anylinux-AppImages.
#
# The build happens inside Arch, because the libraries have to be current
# (GTK 4.14+, WebKitGTK 6.0) and because Arch keeps 32-bit libraries out of
# /usr/lib, which the collector relies on. Run from the host, this script
# re-runs itself in a container; in CI it is already inside one and builds in
# place. Either way `cargo build --release` runs against Arch's libraries, not
# the host's or the Flatpak runtime's.
set -euo pipefail

# HERE is the checkout this script is in, for its own files under
# packaging/appimage; ROOT is the source being built. They differ when an
# older tag is built with today's script (CI does that for a hand-started run).
HERE="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="${HYLKI_APPIMAGE_SRC:-$HERE}"
ARCH="$(uname -m)"
IMAGE="${HYLKI_APPIMAGE_IMAGE:-ghcr.io/pkgforge-dev/archlinux:latest}"
SITE="https://hylki.hyprlab.co"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
case "$VERSION" in
    *-beta.*)
        APP_ID="co.hyprlab.Hylki.Beta"
        NAME="Hylki-Beta"
        FEATURES=(--features beta)
        UPDATES="$SITE/download/beta/$NAME-$ARCH.AppImage.zsync"
        ;;
    *)
        APP_ID="co.hyprlab.Hylki"
        NAME="Hylki"
        FEATURES=()
        UPDATES="$SITE/download/$NAME-$ARCH.AppImage.zsync"
        ;;
esac

# ---------------------------------------------------------------- host side

if [ ! -f /etc/arch-release ]; then
    OUT="$ROOT/packaging/out"
    mkdir -p "$OUT"
    echo "==> Building in $IMAGE (podman)"
    # The tree is copied in rather than built in place: the build installs
    # itself into the container's /usr, and target/ must not be shared with
    # the host's own builds (different libraries, different linker). A named
    # volume keeps the container's target/ between runs so a second build is
    # incremental.
    #
    # `label=disable`: SELinux would otherwise deny the container access to
    # the repository rather than relabel it, and relabelling a working tree
    # to please one build is not a trade worth making.
    exec podman run --rm \
        --security-opt label=disable \
        -v "$ROOT":/src:ro \
        -v "$HERE":/here:ro \
        -v "$OUT":/out \
        -v hylki-appimage-target:/build/target \
        -v hylki-appimage-cargo:/root/.cargo \
        "$IMAGE" \
        sh -euc '
            mkdir -p /build
            tar -C /src --exclude=./target --exclude=./temp --exclude=./.git \
                        --exclude=./dist --exclude=./packaging/out -cf - . \
                | tar -C /build -xf -
            HYLKI_APPIMAGE_SRC=/build exec /here/tools/build-appimage.sh
        '
fi

# ------------------------------------------------------------ container side

# /out is the host's packaging/out when this script re-ran itself above; in CI
# nothing is mounted there and the artifacts stay in the checkout.
OUT="$([ -d /out ] && echo /out || echo "$ROOT/packaging/out")"
mkdir -p "$OUT"

echo "==> Installing build and runtime dependencies"
# Xvfb gives quick-sharun a display to start the app on under strace, which is
# how it finds the libraries loaded with dlopen; without one it skips that
# step and bundles only what the binaries link.
#
# Spell-check dictionaries (#114): the languages Arch packages. The four the
# Flatpak has and Arch does not (pt_BR, pt_PT, sv_SE, uk_UA) are fetched
# below, so the two packages spell-check the same languages; el, hu, ro and
# en_GB come free with these packages and are kept. enchant finds them all
# under share/hunspell inside the bundle.
pacman -Syu --noconfirm --needed \
    base-devel git rust pkgconf patchelf wget strace file xorg-server-xvfb \
    gtk4 libadwaita webkitgtk-6.0 poppler-glib poppler-data \
    enchant hunspell \
    hunspell-de hunspell-el hunspell-en_us hunspell-en_gb hunspell-es_es \
    hunspell-fr-classical hunspell-hu hunspell-it hunspell-nl hunspell-pl \
    hunspell-ro hunspell-ru \
    ttf-dejavu ttf-liberation adwaita-fonts python \
    gettext desktop-file-utils adwaita-icon-theme hicolor-icon-theme

echo "==> Fetching quick-sharun"
sharun_base="https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/refs/heads/main/useful-tools"
wget -q "$sharun_base/quick-sharun.sh"       -O /usr/bin/quick-sharun
wget -q "$sharun_base/get-debloated-pkgs.sh" -O /usr/bin/get-debloated-pkgs
chmod +x /usr/bin/quick-sharun /usr/bin/get-debloated-pkgs
# Slimmer builds of Mesa, libicu and friends, which are most of what a
# WebKit bundle weighs beyond WebKit itself.
get-debloated-pkgs --add-common --prefer-nano

echo "==> Building the release binary ($APP_ID $VERSION)"
cd "$ROOT"
cargo build --release "${FEATURES[@]}"

echo "==> Installing into /usr"
# The same layout the RPM installs (packaging/fedora/hylki.spec): the
# collector reads the app out of /usr, and the app's own paths are relative to
# the binary, so what works there works inside the bundle.
install -Dm755 target/release/hylki /usr/bin/hylki
install -d /usr/share/applications /usr/share/metainfo
msgfmt --desktop --template="data/$APP_ID.desktop" -d po -o "/usr/share/applications/$APP_ID.desktop"
msgfmt --xml --template="data/$APP_ID.metainfo.xml" -d po -o "/usr/share/metainfo/$APP_ID.metainfo.xml"
for po in po/*.po; do
    [ -e "$po" ] || continue
    lang="$(basename "$po" .po)"
    install -d "/usr/share/locale/$lang/LC_MESSAGES"
    msgfmt -o "/usr/share/locale/$lang/LC_MESSAGES/hylki.mo" "$po"
done
for size in 256x256 512x512; do
    install -Dm644 "data/icons/hicolor/$size/apps/$APP_ID.png" \
        "/usr/share/icons/hicolor/$size/apps/$APP_ID.png"
done
install -Dm644 "data/icons/hicolor/scalable/apps/$APP_ID.svg" \
    "/usr/share/icons/hicolor/scalable/apps/$APP_ID.svg"
desktop-file-validate "/usr/share/applications/$APP_ID.desktop"

echo "==> Collecting the dependencies"
export APPDIR="$ROOT/AppDir"
export VERSION ARCH
export ICON="/usr/share/icons/hicolor/256x256/apps/$APP_ID.png"
export DESKTOP="/usr/share/applications/$APP_ID.desktop"
export OUTPATH="$OUT"
export OUTNAME="$NAME-$ARCH.AppImage"
# The window has to match the launcher's StartupWMClass or the desktop shows
# the app twice in the dock; on X11 the class comes out wrong without the shim.
export STARTUPWMCLASS="$APP_ID"
export GTK_CLASS_FIX=1
# What AppImage updaters (AppImageUpdate, AppManager, Gear Lever) read to
# update in place: the address of the newest .zsync on this channel. Written
# here so that a bundle taken from the GitHub release updates from the same
# place as one downloaded from the site, and so that Flatout, finding it
# already there, publishes the file unchanged.
export UPINFO="zsync|$UPDATES"

# gpg is deliberately *not* bundled: OpenPGP (#133) drives the user's own
# keyring and gpg-agent, and a second gpg of a different version talking to
# the running agent is a worse answer than using the one the host already has
# (which is what a source or RPM install does — see docs/INSTALLING.md).
#
# The two libraries named after the binary are dlopened, so nothing in the
# app points at them for the collector to follow: libgiognomeproxy is the GIO
# module that makes WebKit honour the desktop's proxy settings, and
# enchant_hunspell is the only spell-check backend we want (the aspell,
# hspell, nuspell and voikko ones would each drag in a library of their own).
quick-sharun /usr/bin/hylki \
    /usr/lib/gio/modules/libgiognomeproxy.so \
    /usr/lib/enchant-2/enchant_hunspell.so

# The dictionaries themselves (#114). enchant reads them from share/hunspell
# under XDG_DATA_DIRS, which the bundle's own share directory is part of —
# the same arrangement the Flatpak uses.
# One dictionary per language, as the Flatpak ships: Arch installs the
# regional variants of several of them (de_AT, de_CH, en_US-large and so on)
# and they are megabytes each for no gain here.
install -d "$APPDIR/share/hunspell"
for lang in de_DE el_GR en_GB en_US es_ES fr_FR hu_HU it_IT nl_NL pl_PL ro_RO ru_RU; do
    for part in aff dic; do
        [ -f "/usr/share/hunspell/$lang.$part" ] || continue
        cp "/usr/share/hunspell/$lang.$part" "$APPDIR/share/hunspell/"
    done
done

# The four the Flatpak ships that Arch packages none of, from the same
# LibreOffice collection and the same pinned commit as the manifest's
# hunspell-dictionaries module, so the two packages spell-check alike.
# 18 MB of word lists, 3 MB once packed.
LIBREOFFICE_DICTS="32b006a2c22a4ac7e8ed3f03346f7b3d85a970a4"
for spec in pt_BR:pt_BR pt_PT:pt_PT sv_SE:sv_SE/dictionaries uk_UA:uk_UA; do
    lang="${spec%%:*}"
    dir="${spec#*:}"
    for part in aff dic; do
        wget -q -O "$APPDIR/share/hunspell/$lang.$part" \
            "https://raw.githubusercontent.com/LibreOffice/dictionaries/$LIBREOFFICE_DICTS/$dir/$lang.$part"
    done
done

# Downloaded rather than packaged, so say what they have to be.
( cd "$APPDIR/share/hunspell" && sha256sum -c <<'SUMS'
21d8ad2a769a60e17e2b5ea4ef11d4d593a58b9e2a82d642ef82d6a4c5523865  pt_BR.aff
a38bfb26b68ece2834e79fe83e48d5792652970ace12db89d1b9674bf9933183  pt_BR.dic
975a209fcc892cb382fa5f34a28c391a39668661ce373ae071287809c5fcae24  pt_PT.aff
e29ba2d7aa8a2ad43e9cb46ac6473064b661545c87002aea90e18899d98d3cc9  pt_PT.dic
b721c9d44bee912feb182b601a1bc2ae3e7dffef660f4130cf2751867488a9dd  sv_SE.aff
384a2126eff333f5f6f9790ae892554546f53948d2988c600397cb5ad6ce66e8  sv_SE.dic
2219dd15e9802adebc45722c60943b1472640260491af38dd3e43b07e75585e6  uk_UA.aff
2e5a9e67be63bdb089b3459addb5d71113319d13768e277bcae20f3cc1ad5a93  uk_UA.dic
SUMS
)

echo "    dictionaries: $(ls "$APPDIR/share/hunspell" | sed 's/\..*//' | sort -u | tr '\n' ' ')"

# The fonts WebKit falls back to when the host's hang it (#296): the
# Flatpak has the runtime's for this, the AppImage carries its own. Sans,
# serif and monospace in DejaVu, Liberation for the Arial, Times and Courier
# that mail asks for by name, Adwaita for the GNOME look. About 20 MB raw.
echo "==> Fallback fonts"
FC="$APPDIR/share/hylki/fontconfig"
install -d "$FC" "$APPDIR/share/hylki/fonts"
cp -L /usr/share/fonts/TTF/DejaVuSans{,-Bold,-Oblique,-BoldOblique}.ttf \
      /usr/share/fonts/TTF/DejaVuSerif{,-Bold,-Italic,-BoldItalic}.ttf \
      /usr/share/fonts/TTF/DejaVuSansMono{,-Bold,-Oblique,-BoldOblique}.ttf \
      /usr/share/fonts/liberation/Liberation{Sans,Serif,Mono}-*.ttf \
      /usr/share/fonts/Adwaita/*.ttf \
      "$APPDIR/share/hylki/fonts/"
# The rules of the build's conf.d, written into the file rather than
# included: an include of conf.d resolves to the host's /etc/fonts. Left
# out, as in the Flatpak's: the ones that add folders (the user's, the
# local admin's) and the README.
python3 - "$HERE/packaging/appimage/fonts-fallback.conf" "$FC/fallback.conf" /etc/fonts/conf.d <<'PY'
# Text, not an XML parser: some of these files use prefixes they never
# declare (xsi:nil, its:rules), which fontconfig reads and a strict parser
# refuses. fontconfig itself checks the result below.
import os, re, sys
template, dest, confd = sys.argv[1:]
skip = {"50-user.conf", "51-local.conf"}
rules = []
for name in sorted(os.listdir(confd)):
    if not name.endswith(".conf") or name in skip or name.startswith("05-"):
        continue
    text = open(os.path.join(confd, name)).read()
    found = re.search(r"<fontconfig[^>]*>(.*)</fontconfig>", text, re.S)
    if not found:
        continue
    body = re.sub(r"<!--.*?-->", "", found.group(1), flags=re.S)
    for tag in ("its:rules", "description", "dir", "cachedir", "include"):
        body = re.sub(rf"<{tag}\b[^>]*/>|<{tag}\b.*?</{tag}>", "", body, flags=re.S)
    body = "\n".join(line.rstrip() for line in body.splitlines() if line.strip())
    if body:
        rules += [f"  <!-- {name} -->", body]
text = open(template).read().replace("  <!-- @RULES@ -->", "\n".join(rules))
open(dest, "w").write(text)
PY
# The fallback has to find these fonts and no others, wherever the bundle is
# mounted: tried from a second, moved copy.
probe="$(mktemp -d)/share/hylki"
install -d "$probe"; cp -r "$APPDIR/share/hylki/." "$probe/"
fonts="$(HOME=$(mktemp -d) FONTCONFIG_FILE="$probe/fontconfig/fallback.conf" fc-list : file 2>"$probe/fc.err")"
grep -i "fontconfig error" "$probe/fc.err" && { echo "fontconfig refuses the fallback config" >&2; exit 1; }
echo "$fonts" | grep -v "^$probe/fonts/" | grep . && { echo "fallback fonts reach outside the bundle" >&2; exit 1; }
echo "    fallback fonts: $(echo "$fonts" | grep -c .), sans-serif is $(HOME=$(mktemp -d) FONTCONFIG_FILE="$probe/fontconfig/fallback.conf" fc-match -f '%{family[0]}' sans-serif)"
echo 'HYLKI_FONT_FALLBACK_CONF=${SHARUN_DIR}/share/hylki/fontconfig/fallback.conf' >> "$APPDIR/.env"

# Pin the message catalogues rather than leave them to be found: src/i18n.rs
# looks beside the binary, which happens to land in the right place in the
# bundle's layout, and a layout change upstream should not silently turn the
# app monolingual.
echo 'HYLKI_LOCALEDIR=${SHARUN_DIR}/share/locale' >> "$APPDIR/.env"

echo "==> Packing"
quick-sharun --make-appimage

# The .zsync quick-sharun writes is for its own address; Flatout makes the
# one that is served.
rm -f "$OUT/$NAME-$ARCH.AppImage.zsync"

echo "==> AppImage in $OUT:"
ls -lh "$OUT/$NAME-$ARCH.AppImage"
