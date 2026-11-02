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

**Bazaar** does not install from a `.flatpakref` whose repository it does not
already know: it reads only the app's ID from the file and reports "ID
'co.hyprlab.Hylki' was not found". Add the repository once, and Hylki then
shows up in Bazaar's search:

```sh
flatpak remote-add --user --if-not-exists hylki https://hylki.hyprlab.co/flatpak/co.hyprlab.Hylki.flatpakrepo
```

## Fedora

Add the signed dnf repository on hylki.hyprlab.co and install from it. Hylki
then updates with the rest of the system, through GNOME Software or
`dnf upgrade`:

```sh
sudo curl -fsSLo /etc/yum.repos.d/hylki.repo https://hylki.hyprlab.co/rpm/hylki.repo
sudo dnf install hylki
```

dnf asks once to trust the repository's signing key, fingerprint
`91A0 AC23 CFD8 C720 4417 B899 8E9F 3DC1 7CFF B221` (the Flatpak repository's
key).

The `.rpm` is also attached to each
[release](https://github.com/hyprlab/hylki/releases/latest). From 1.43.0 the
package adds the repository itself, so an RPM installed from a download
keeps updating too; it is the same `/etc/yum.repos.d/hylki.repo`, kept if you
edit it (set `enabled=0` to stop updates from it).

The RPM targets current Fedora releases (44+) on x86_64 and aarch64. On anything older, use the Flatpak or
[build from source](BUILDING.md).

## Debian and Ubuntu

Add the signed apt repository on hylki.hyprlab.co and install from it. Hylki
then updates with the rest of the system, through GNOME Software or
`apt upgrade`:

```sh
sudo curl -fsSLo /etc/apt/sources.list.d/hylki.sources https://hylki.hyprlab.co/deb/hylki.sources
sudo apt update
sudo apt install hylki
```

The `.sources` file carries the repository's signing key, the same key as the
dnf repository's, so there is no key to import separately.

The `.deb` is also attached to each
[release](https://github.com/hyprlab/hylki/releases/latest); install it with
`sudo apt install ./hylki_*_amd64.deb` (`_arm64.deb` on ARM), which fetches
its dependencies. The
package adds the repository itself, as the RPM does: the same
`/etc/apt/sources.list.d/hylki.sources`, kept if you edit it (add a line
`Enabled: no` to stop updates from it).

The package needs libadwaita 1.7: Debian 13 (trixie) and later, or Ubuntu
25.10 and later, on x86_64 and arm64. Ubuntu 24.04 LTS and the distributions built
on it, such as Linux Mint 22, have an older libadwaita; use the Flatpak there.

## AppImage

One file that runs on any distribution, for **x86_64 and aarch64 (ARM64)**.
Download the one matching
`uname -m`, make it executable and run it:

```sh
curl -fLO https://hylki.hyprlab.co/download/Hylki-x86_64.AppImage
chmod +x Hylki-x86_64.AppImage
./Hylki-x86_64.AppImage
```

On ARM, the file is `Hylki-aarch64.AppImage`. Each release carries both as
well.

Each AppImage knows where its updates come from: add it to
[Gear Lever](https://flathub.org/apps/it.mijorus.gearlever) or
AppImageLauncher, or run `appimageupdatetool` on it, and it updates from
hylki.hyprlab.co, fetching only the parts that changed.

The AppImage runs without a sandbox, like the RPM and the Debian package, and
shares their settings (`~/.config/hylki`) and cached mail. OpenPGP uses the system's `gpg`, which the AppImage
does not carry. When the system's fonts hang the message view (#296), it falls
back to fonts it carries.

## Gentoo

A community-maintained ebuild lives in
[bennypowers' overlay](https://github.com/bennypowers/gentoo-overlay)
(thanks [@bennypowers](https://github.com/bennypowers)):

```sh
eselect repository enable bennypowers
emaint sync -r bennypowers
emerge -av mail-client/hylki
```

## NixOS

A community-maintained flake lives in
[clementpoiret's hylki-flake](https://github.com/clementpoiret/hylki-flake)
(thanks [@clementpoiret](https://github.com/clementpoiret)). It packages the
latest stable release for x86_64 and aarch64. To run it without installing:

```sh
nix run github:clementpoiret/hylki-flake
```

Adding it to a NixOS configuration is covered in its README.

## Beta channel

Betas come in every format the stable release does, on **x86_64 and aarch64
(ARM64)**: Flatpak, RPM and Debian packages, and an AppImage.

The Flatpak beta installs alongside the stable app as a separate application
(`co.hyprlab.Hylki.Beta`) and shares its accounts, settings and cached mail:

```sh
flatpak install --user --from https://hylki.hyprlab.co/flatpak/co.hyprlab.Hylki.Beta.flatpakref
```

Each beta release also carries `Hylki-Beta-x86_64.flatpak` and
`Hylki-Beta-aarch64.flatpak` for a direct download. To remove the beta, run
`flatpak uninstall co.hyprlab.Hylki.Beta`; the stable app is untouched.

On Fedora, betas also come as RPMs from a beta dnf repository. They replace the
installed `hylki` package rather than sitting beside it, and share its settings.
Every RPM from 1.43.0 carries the beta repository, switched off; turn it on
once:

```sh
sudo dnf config-manager setopt hylki-beta.enabled=1
sudo dnf upgrade --refresh hylki
```

A beta version such as `1.44.0~beta.1` sorts before `1.44.0`, so the stable
release replaces the last beta when it comes out, and the next beta follows it.
To leave the betas, set `hylki-beta.enabled=0`; dnf won't move back to an older
version by itself, so run `sudo dnf distro-sync hylki` to return to the
newest stable. With an RPM older than 1.43.0, add the beta repository with
`sudo curl -fsSLo /etc/yum.repos.d/hylki-beta.repo https://hylki.hyprlab.co/rpm/hylki-beta.repo`.

On Debian and Ubuntu, betas come from the apt repository's beta suite in the
same way. Every Debian package carries it, switched off; turn it on once:

```sh
sudo sed -i 's/^Enabled: no/Enabled: yes/' /etc/apt/sources.list.d/hylki-beta.sources
sudo apt update && sudo apt install hylki
```

To leave the betas, set `Enabled: no` again, then run
`sudo apt install hylki/stable` to return to the newest stable; apt asks
before it moves to an older version.

The beta AppImage is the beta app, "Hylki (beta)" with its own launcher, and
updates from the beta channel. Like the packages, it shares the settings and
mail of a stable AppImage, RPM or Debian package:

```sh
curl -fLO https://hylki.hyprlab.co/download/beta/Hylki-Beta-x86_64.AppImage
chmod +x Hylki-Beta-x86_64.AppImage
```

On ARM, the file is `Hylki-Beta-aarch64.AppImage`. To leave the betas, delete
it and use the stable AppImage.

## Other distributions

Arch and Snap packages were discontinued after 1.7.0. Use the Flatpak (it
works on every distribution) or [build from source](BUILDING.md).

## Runtime requirements

A Secret Service provider (e.g. gnome-keyring, preinstalled on GNOME) is needed
for password storage. Address books other than Hylki's own (GNOME Contacts,
[CardDAV accounts](DOCUMENTATION.md#carddav-accounts) and LDAP directories)
are kept by Evolution Data Server, which GNOME installs; on other desktops
install the `evolution-data-server` package. The Flatpak uses the system's,
so it is needed there too. Everything else the Flatpak carries; a source, RPM
or Debian install also wants `gnupg2` (`gnupg` on Debian) for
[OpenPGP](DOCUMENTATION.md#openpgp-encrypted-and-signed-mail) and
`nautilus-python` (`python3-nautilus`) for the
[Files entry](DOCUMENTATION.md#send-with-hylki-from-gnome-files).
