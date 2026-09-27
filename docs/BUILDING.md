# Building from source

Hylki needs the Rust toolchain ([rustup](https://rustup.rs)), a C compiler,
and the GTK 4 / libadwaita / WebKitGTK 6 development libraries, plus
OpenSSL, D-Bus, gettext and Wayland headers. At runtime it needs a Secret
Service provider (e.g. gnome-keyring).

## Dependencies

**Fedora**

```sh
sudo dnf install gcc gcc-c++ gtk4-devel libadwaita-devel webkitgtk6.0-devel \
    poppler-glib-devel openssl-devel dbus-devel gettext-devel wayland-devel
```

**Debian / Ubuntu**

```sh
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
    libwebkitgtk-6.0-dev libpoppler-glib-dev libssl-dev libdbus-1-dev gettext \
    libwayland-dev
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
the Flatpak, the RPM, the DEB and `install.sh` all do it as part of their own build.
See [po/README.md](../po/README.md).

## Native packages

On a Fedora build host, run `tools/build-packages.sh` (or pass `rpm`). On a Debian or
Ubuntu build host, run `tools/build-packages.sh deb`. The resulting packages
are written to `packaging/out/`; the Debian file is named
`hylki-<version>-<architecture>.deb` (for example,
`hylki-1.41.1-amd64.deb`). `tools/build-packages.sh all` builds both from one
release binary when both packaging tools are available. Build on the oldest
distribution release you intend to support, since native binaries use the
host's shared libraries. The DEB records its shared-library dependencies from
the binary with `dpkg-shlibdeps`.

`.github/workflows/build-deb.yml` builds and checks the amd64 DEB on Debian 13
for pull requests, stable release tags and manual runs. It uploads the package
as a CI artifact; the maintainer attaches reviewed packages to releases.

## Flatpak

The manifest is `co.hyprlab.Hylki.yml`, built with `flatpak-builder` (or
`org.flatpak.Builder`). Its Rust dependencies come from `cargo-sources.json`,
which has to be regenerated whenever `Cargo.lock` changes:

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o cargo-sources.json
```

## AppImage

Hylki does not ship an AppImage: the packages it distributes are the Flatpak,
RPM and DEB. The build exists all the same, so the option stays open and
does not rot (#235).

`tools/build-appimage.sh` produces `packaging/out/Hylki-<arch>.AppImage` and
the `.zsync` file that goes with it. It runs inside an Arch container (the
script starts one with podman; in CI it is already in one), because the
bundle carries every library down to the C library and those have to be
current. The library collecting is [pkgforge's
`quick-sharun`](https://github.com/pkgforge-dev/Anylinux-AppImages), fetched
by the script rather than vendored.

`.github/workflows/build-appimage.yml` builds it for x86_64 and aarch64, by
hand only: no tag starts it and nothing it produces is published. What
turning that around would take is written at the top of the workflow.

## Your own OAuth client

Google and Dropbox clients can be compiled in at build time; see
[Configuration](DOCUMENTATION.md#oauth-google--microsoft).
