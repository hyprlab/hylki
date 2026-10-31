# Building from source

Hylki needs the Rust toolchain ([rustup](https://rustup.rs)), a C compiler,
and the GTK 4 / libadwaita / WebKitGTK 6 development libraries, plus
OpenSSL, gettext and Wayland headers. At runtime it needs a Secret Service
provider (e.g. gnome-keyring).

## Dependencies

**Fedora**

```sh
sudo dnf install gcc gcc-c++ gtk4-devel libadwaita-devel webkitgtk6.0-devel \
    poppler-glib-devel openssl-devel gettext-devel wayland-devel
```

**Debian / Ubuntu**

```sh
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
    libwebkitgtk-6.0-dev libpoppler-glib-dev libssl-dev gettext libwayland-dev
```

## Build and install

```sh
git clone https://github.com/hyprlab/hylki.git
cd hylki
cargo build --release
./install.sh          # installs the binary, icon and .desktop file into ~/.local
./uninstall.sh        # removes them again (--purge also removes settings and the mail cache)
```

## Translations

`tools/build-locale.sh` compiles `po/*.po` so a source-tree run picks them up;
the Flatpak, the RPM, the Debian package and `install.sh` all do it as part of
their own build.
See [po/README.md](../po/README.md).

## Flatpak

The manifest is `co.hyprlab.Hylki.yml`, built with `flatpak-builder` (or
`org.flatpak.Builder`). Its Rust dependencies come from `cargo-sources.json`,
which has to be regenerated whenever `Cargo.lock` changes:

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o cargo-sources.json
```

## RPM and Debian packages

`tools/build-packages.sh rpm` and `tools/build-packages.sh deb` produce
`packaging/out/hylki-<version>-1.fc44.x86_64.rpm` and
`packaging/out/hylki_<version>-1_amd64.deb`; with no argument it builds both.
A beta's version is written `1.45.0~beta.1` in both, which sorts before
`1.45.0`. Each is built for the machine it runs on: the ARM64 ones
(`.aarch64.rpm`, `_arm64.deb`) come from `.github/workflows/build-arm64.yml`,
which builds them on GitHub's arm64 runners and installs each in a clean
Fedora 44 or Debian 13 container to check it.

The RPM wraps a release build from the host, so it is built on Fedora. The
Debian package is built inside a Debian 13 container (the script starts one
with podman), because a binary built on Fedora needs a newer C library than
Debian has, and Debian 13 is the oldest release with libadwaita 1.7. On a
Debian host with the dependencies above and `dpkg-dev`, it builds in place.
`dpkg-shlibdeps` works out the package's dependencies from the binary.

## AppImage

`tools/build-appimage.sh` produces `packaging/out/Hylki-<arch>.AppImage`, or
`Hylki-Beta-<arch>.AppImage` when the version is a beta, which is then built
with the `beta` feature as the Flatpak beta is (#235). It runs inside an Arch
container (the script starts one with podman; in CI it is already in one),
because the bundle carries every library down to the C library and those have
to be current. The library collecting is [pkgforge's
`quick-sharun`](https://github.com/pkgforge-dev/Anylinux-AppImages), fetched
by the script rather than vendored. `HYLKI_APPIMAGE_SRC` names another
checkout to build with this script, such as an older tag.

The bundle also carries the spell-check dictionaries and a few fonts with a
fontconfig file of their own (`packaging/appimage/fonts-fallback.conf`), which
the message view falls back to when the system's fonts hang it (#296).

`.github/workflows/build-appimage.yml` builds it for x86_64 and aarch64 on
every tag. Nothing is published from CI: the release attaches the two
AppImages, and Flatout on hylki.hyprlab.co serves each channel's newest one
with the `.zsync` file AppImage updaters read.

## Your own OAuth client

Google and Dropbox clients can be compiled in at build time; see
[Configuration](DOCUMENTATION.md#oauth-google--microsoft).
