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

## Tuta helper

Tuta accounts go through `hylki-tuta`, a separate program built from
`native/tuta` (GPL-3.0-or-later). Hylki looks for it next to the `hylki`
executable first, then on `PATH`, so every package installs it in the same
`bin` directory as `hylki`. Without it, everything but Tuta accounts works.

The helper builds against the Tuta SDK, which is generated from two
submodules, `native/tuta/upstream` (tutabridge) and the tutanota repository
inside it, plus Hylki's own SDK patches in `native/tuta/sdk-patches`. Check
the submodules out and prepare the SDK once, and again whenever the
submodule or a patch changes:

```sh
git submodule update --init --recursive
native/tuta/prepare.sh
```

The script needs `git` and `python3`, no network. It runs tutabridge's
`sdk-prepare.py`, which verifies the generated tree in
`native/tuta/upstream/sdk/generated` against the digests the submodule pins,
then writes that tree with Hylki's patches applied, beside tutabridge's
`tutabridge-tuta` crate, to `native/tuta/prepared` (untracked), which the
helper builds from. On a repeat run it only checks. Then build the helper and
put it beside `hylki`:

```sh
(cd native/tuta && cargo build --release --locked)
install -Dm755 native/tuta/target/release/hylki-tuta ~/.local/bin/hylki-tuta
```

`native/tuta` has its own `Cargo.lock`, and its tests run with
`cargo test --locked` there (`.github/workflows/tuta-helper.yml`).

## Proton helper

Proton Mail accounts that sign in natively (rather than through Proton Mail
Bridge) go through `hylki-proton`, a separate Go program built from
`native/proton` (GPL-3.0-or-later). It is found the same way as the Tuta
helper: next to `hylki`, then on `PATH`. Without it, everything but native
Proton accounts works.

It needs Go 1.26.1 or newer (the `go` line in `native/proton/go.mod`) and
nothing from the network: its dependencies are committed in
`native/proton/vendor`.

```sh
(cd native/proton && go build -mod=vendor -trimpath -o hylki-proton .)
install -Dm755 native/proton/hylki-proton ~/.local/bin/hylki-proton
```

Its tests run offline against go-proton-api's in-process server with
`go test -mod=vendor ./...` in `native/proton`
(`.github/workflows/proton-helper.yml`). After changing `go.mod`, run
`go mod tidy && go mod vendor` there and commit both. `native/proton/README.md`
describes the JSON it speaks.

## Translations

`tools/build-locale.sh` compiles `po/*.po` so a source-tree run picks them up;
the Flatpak, the RPM, the Debian package and `install.sh` all do it as part of
their own build.
See [po/README.md](../po/README.md).

## Flatpak

The manifest is `co.hyprlab.Hylki.yml`, built with `flatpak-builder` (or
`org.flatpak.Builder`). Its Rust dependencies come from `cargo-sources.json`,
and the Tuta helper's from `native/tuta/cargo-sources.json`. Neither is
committed; regenerate them whenever the matching `Cargo.lock` changes:

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o cargo-sources.json
python3 flatpak-cargo-generator.py native/tuta/Cargo.lock -o native/tuta/cargo-sources.json
```

The `hylki-tuta` module builds the helper into `/app/bin` beside `hylki`,
and the `hylki-proton` module builds the Proton helper there with the
`org.freedesktop.Sdk.Extension.golang//25.08` SDK extension (install it
beside the Rust one), offline from `native/proton/vendor`.
Prepare the Tuta SDK first, as above: the generated tree is not in git, so
the manifest takes it from the checkout it is built in. To build the working
tree rather than the tagged release, write a local manifest:

```sh
tools/local-manifest.sh /tmp/hylki-local.yml
flatpak-builder --user --install --force-clean build-dir /tmp/hylki-local.yml
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
`dpkg-shlibdeps` works out the package's dependencies from the binaries.
Both packages carry the Tuta helper as `/usr/bin/hylki-tuta`; the script
prepares the Tuta SDK and builds the helper itself, so the submodules have
to be checked out. They carry the Proton helper as `/usr/bin/hylki-proton`
too, so the RPM build needs `go` on the host; the Debian container installs
Debian's, and a Go older than `native/proton/go.mod` asks for fetches the
right one (`GOTOOLCHAIN=auto`).

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
the message view falls back to when the system's fonts hang it (#296), and the
Tuta and Proton helpers beside `hylki`, built the same way as for the packages.

`.github/workflows/build-appimage.yml` builds it for x86_64 and aarch64 on
every tag. Nothing is published from CI: the release attaches the two
AppImages, and Flatout on hylki.hyprlab.co serves each channel's newest one
with the `.zsync` file AppImage updaters read.

## Your own OAuth client

Google and Dropbox clients can be compiled in at build time; see
[Configuration](DOCUMENTATION.md#oauth-google--microsoft).
