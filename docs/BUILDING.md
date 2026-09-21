# Building from source

Hylki needs the Rust toolchain and the GTK 4 / libadwaita / WebKitGTK 6
development libraries, plus a Secret Service provider (e.g. gnome-keyring) at
runtime.

## Dependencies

**Fedora**

```sh
sudo dnf install gtk4-devel libadwaita-devel webkitgtk6.0-devel poppler-glib-devel
```

**Debian / Ubuntu**

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libpoppler-glib-dev
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
the Flatpak, the RPM and `install.sh` all do it as part of their own build.
See [po/README.md](../po/README.md).

## Flatpak

The manifest is `co.hyprlab.Hylki.yml`, built with `flatpak-builder` (or
`org.flatpak.Builder`). Its Rust dependencies come from `cargo-sources.json`,
which has to be regenerated whenever `Cargo.lock` changes:

```sh
python3 flatpak-cargo-generator.py Cargo.lock -o cargo-sources.json
```

## AppImage

Hylki does not ship an AppImage: the packages it distributes are the Flatpak
and the RPM. The build exists all the same, so the option stays open and
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
