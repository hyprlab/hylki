# Installing

The short version is in the [README](../README.md#installing); this page has
the rest.

## Flatpak (recommended)

Works on any distribution, on **x86_64 and aarch64 (ARM64)**. Installing from
the signed repo picks the right architecture on its own and keeps the app
updated:

```sh
flatpak install --user --from https://hylki.hyprlab.co/flatpak/co.hyprlab.Hylki.flatpakref
```

**Prefer a direct download?** Each release carries `Hylki-x86_64.flatpak` and
`Hylki-aarch64.flatpak`; grab the one matching `uname -m` from the
[latest release](https://github.com/hyprlab/hylki/releases/latest) and run
`flatpak install --user ./Hylki-*.flatpak`. The bundle carries the repo address
and signing key, so it still receives updates from the official repo. (A bundle
holds a single architecture; the repo above holds both.)

## Fedora

Download the `.rpm` from the
[latest release](https://github.com/hyprlab/hylki/releases/latest):

```sh
sudo dnf install ./hylki-*.x86_64.rpm
```

The RPM targets current Fedora releases (44+) on x86_64 only. On ARM, or on
anything older, use the Flatpak or [build from source](BUILDING.md).

## Gentoo

A community-maintained ebuild lives in
[bennypowers' overlay](https://github.com/bennypowers/gentoo-overlay)
(thanks [@bennypowers](https://github.com/bennypowers)):

```sh
eselect repository enable bennypowers
emaint sync -r bennypowers
emerge -av mail-client/hylki
```

## Nix

A community-maintained flake lives in
[tbaumann's fork](https://github.com/tbaumann/hylki) (thanks
[@tbaumann](https://github.com/tbaumann)):

```sh
nix run github:tbaumann/hylki
```

## Beta channel

Betas install alongside the stable app as a separate application
(`co.hyprlab.Hylki.Beta`), with their own settings and cache. See
[hylki.hyprlab.co](https://hylki.hyprlab.co) for the repo address.

## Other distributions

Arch, Debian/Ubuntu and Snap packages were discontinued after 1.7.0. Use the
Flatpak (it works on every distribution) or [build from source](BUILDING.md).

## Runtime requirements

A Secret Service provider (e.g. gnome-keyring, preinstalled on GNOME) is needed
for password storage. Everything else the Flatpak carries; a source or RPM
install also wants `gnupg2` for [OpenPGP](DOCUMENTATION.md#openpgp-encrypted-and-signed-mail)
and `nautilus-python` for the
[Files entry](DOCUMENTATION.md#send-with-hylki-from-gnome-files).
