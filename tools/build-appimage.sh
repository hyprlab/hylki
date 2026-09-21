#!/usr/bin/env bash
#
# Build the AppImage (#235).
#
#   tools/build-appimage.sh          # builds it in a container, from the host
#
# Output lands in packaging/out/:
#   Hylki-<arch>.AppImage            - the bundle
#   Hylki-<arch>.AppImage.zsync      - the delta file AppImage updaters read
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

APP_ID="co.hyprlab.Hylki"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH="$(uname -m)"
IMAGE="${HYLKI_APPIMAGE_IMAGE:-ghcr.io/pkgforge-dev/archlinux:latest}"

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
        -v "$OUT":/out \
        -v hylki-appimage-target:/build/target \
        -v hylki-appimage-cargo:/root/.cargo \
        "$IMAGE" \
        sh -euc '
            mkdir -p /build
            tar -C /src --exclude=./target --exclude=./temp --exclude=./.git \
                        --exclude=./dist --exclude=./packaging/out -cf - . \
                | tar -C /build -xf -
            exec /build/tools/build-appimage.sh
        '
fi

# ------------------------------------------------------------ container side

# /out is the host's packaging/out when this script re-ran itself above; in CI
# nothing is mounted there and the artifacts stay in the checkout.
OUT="$([ -d /out ] && echo /out || echo "$ROOT/packaging/out")"
mkdir -p "$OUT"

echo "==> Installing build and runtime dependencies"
# Spell-check dictionaries (#114): the languages Arch packages. The four the
# Flatpak has and Arch does not (pt_BR, pt_PT, sv_SE, uk_UA) are fetched
# below, so the two packages spell-check the same languages; el, hu, ro and
# en_GB come free with these packages and are kept. enchant finds them all
# under share/hunspell inside the bundle.
pacman -Syu --noconfirm --needed \
    base-devel git rust pkgconf patchelf wget strace file \
    gtk4 libadwaita webkitgtk-6.0 poppler-glib poppler-data \
    enchant hunspell \
    hunspell-de hunspell-el hunspell-en_us hunspell-en_gb hunspell-es_es \
    hunspell-fr-classical hunspell-hu hunspell-it hunspell-nl hunspell-pl \
    hunspell-ro hunspell-ru \
    gettext desktop-file-utils adwaita-icon-theme hicolor-icon-theme

echo "==> Fetching quick-sharun"
sharun_base="https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/refs/heads/main/useful-tools"
wget -q "$sharun_base/quick-sharun.sh"       -O /usr/bin/quick-sharun
wget -q "$sharun_base/get-debloated-pkgs.sh" -O /usr/bin/get-debloated-pkgs
chmod +x /usr/bin/quick-sharun /usr/bin/get-debloated-pkgs
# Slimmer builds of Mesa, libicu and friends, which are most of what a
# WebKit bundle weighs beyond WebKit itself.
get-debloated-pkgs --add-common --prefer-nano

echo "==> Building the release binary"
cd "$ROOT"
cargo build --release

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

echo "==> Collecting the dependencies"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
export APPDIR="$ROOT/AppDir"
export VERSION ARCH
export ICON="/usr/share/icons/hicolor/256x256/apps/$APP_ID.png"
export DESKTOP="/usr/share/applications/$APP_ID.desktop"
export OUTPATH="$OUT"
export OUTNAME="Hylki-$ARCH.AppImage"
# The window has to match the launcher's StartupWMClass or the desktop shows
# the app twice in the dock; on X11 the class comes out wrong without the shim.
export STARTUPWMCLASS="$APP_ID"
export GTK_CLASS_FIX=1
# What AppImage updaters (AppImageUpdate, AppManager, Gear Lever) read to
# update in place from the GitHub release, using the .zsync file beside the
# bundle. Both have to be attached to the release for this to resolve.
export UPINFO="gh-releases-zsync|hyprlab|hylki|latest|*$ARCH.AppImage.zsync"

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

# Pin the message catalogues rather than leave them to be found: src/i18n.rs
# looks beside the binary, which happens to land in the right place in the
# bundle's layout, and a layout change upstream should not silently turn the
# app monolingual.
echo 'HYLKI_LOCALEDIR=${SHARUN_DIR}/share/locale' >> "$APPDIR/.env"

echo "==> Packing"
quick-sharun --make-appimage

echo "==> AppImage in $OUT:"
ls -lh "$OUT"/Hylki-"$ARCH".AppImage*
