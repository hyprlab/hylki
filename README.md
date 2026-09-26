<p align="center">
  <img src="data/repo/logo.png" width="120" alt="Hylki logo">
</p>

<h1 align="center">Hylki</h1>

<p align="center">
  A fast, GNOME-native email client, built with Rust and libadwaita.
</p>

<p align="center">
  <a href="https://hylki.hyprlab.co">Website</a> ·
  <a href="docs/FEATURES.md">Features</a> ·
  <a href="docs/DOCUMENTATION.md">Documentation</a> ·
  <a href="https://github.com/hyprlab/hylki/releases">Releases</a> ·
  <a href="https://discord.gg/YfEJ4b6PFW">Discord</a>
  <br>
  <img alt="License: AGPL-3.0" src="https://img.shields.io/badge/license-AGPL--3.0-blue">
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="data/repo/screenshot-dark.png">
    <img src="data/repo/screenshot.png" width="900" alt="Hylki showing a unified inbox">
  </picture>
</p>

---

Hylki is an email client for the Linux desktop, made to fit in with GNOME. It
connects straight to your mail servers, keeps your mail and passwords on your
computer, and blocks trackers in messages. It sends no telemetry or analytics.
It is free software, built for GNOME, to be feature rich and beautiful
([the manifesto](docs/MANIFESTO.md)).

> [!NOTE]
> **Vireo is now Hylki.** Since v1.35.0 the app once called Vireo (and Veem
> before v1.6.0) has been renamed Hylki, with a new icon. Nothing else changed.
> The first time Hylki starts, it copies your accounts, settings and cached
> mail over from Vireo, and leaves Vireo alone until you remove it.

## Features

- Several accounts at once over IMAP, POP3 or JMAP, each synced in the
  background, with a combined *Inboxes* view. Google and Microsoft accounts
  sign in with OAuth.
- The whole mailbox is synced and searchable, however large it is. The first
  page shows up straight away and the rest is indexed in the background.
- Messages you delete, move or flag on your phone or in another client are
  updated here too.
- Write in rich text, Markdown, HTML or plain text, chosen per message.
- A message that fails to send waits in the Outbox until the connection comes
  back, and any message can be scheduled to go out later.
- Large files can be uploaded to your own Nextcloud, ownCloud, OpenCloud or
  Seafile, or to OneDrive or Dropbox, and sent as a link.
- Remote content is blocked until you allow it for a message or a sender.
  Reader View cuts a cluttered message down to its text.
- OpenPGP: read, sign and encrypt mail and manage keys with the GnuPG already
  on your computer.
- Filters with several conditions tag mail and move it to folders. Tags are
  stored as server keywords where the server supports them.
- Optional Gmail-style single-key shortcuts
  ([the full list](docs/KEYBOARD_SHORTCUTS.md)).
- An adaptive three-pane layout, five colour themes, Focus Mode, light and dark
  styles that follow the system, and an option to keep running in the
  background.

**[All features →](docs/FEATURES.md)**

## Installing

The Flatpak is the recommended way to install Hylki. It works on any
distribution, on x86_64 and aarch64, and updates come from a signed repository:

```sh
flatpak install --user --from https://hylki.hyprlab.co/flatpak/co.hyprlab.Hylki.flatpakref
```

On Fedora you can install the `.rpm` from the
[latest release](https://github.com/hyprlab/hylki/releases/latest) instead:

```sh
sudo dnf install ./hylki-*.x86_64.rpm
```

Community packages for Gentoo and Nix, direct `.flatpak` downloads and the beta
channel are covered in [docs/INSTALLING.md](docs/INSTALLING.md). To build Hylki
yourself, see [docs/BUILDING.md](docs/BUILDING.md).

## Documentation

| | |
| --- | --- |
| [Features](docs/FEATURES.md) | The full feature list |
| [Documentation](docs/DOCUMENTATION.md) | Accounts, OAuth, cloud attachments, Markdown, OpenPGP, GNOME Files, privacy |
| [Keyboard shortcuts](docs/KEYBOARD_SHORTCUTS.md) | The single-key shortcuts |
| [Installing](docs/INSTALLING.md) · [Building](docs/BUILDING.md) | Packages, the beta channel and building from source |
| [Contributing](docs/CONTRIBUTING.md) · [Credits](docs/CREDITS.md) | How to contribute, and the people who have |
| [Release notes](docs/RELEASE_NOTES.md) · [Changelog](CHANGELOG.md) | What changed in each release |

## Privacy

Remote content in messages is blocked by default, so tracking pixels cannot
report that you opened a message. Passwords and OAuth tokens are kept in the
system keyring rather than in files, and decrypted mail is never written to the
cache. There is more in [Documentation](docs/DOCUMENTATION.md#privacy).

## AI notice

Hylki is built by a human maintainer who uses generative AI as a development
tool. The maintainer decides what gets built, reviews the results, tests every
release and signs off on everything that ships. The app itself contains no AI
and makes no requests to AI services.

The app icon, its symbolic version and the wordmark were drawn by
[Yiannis Ioannides](https://github.com/yioannides). The other visual assets
were also made by the maintainer without generative AI.

Bug reports and pull requests are welcome whether or not AI tools were
involved, and everything merged is reviewed by a person.

## Contributing

Hylki is maintained by Hyprlab, and much of it started as a request or a patch
from someone who uses it. Bug reports, feature requests, design feedback,
translations, packaging and code are all welcome. Everyone whose work is in the
app is named in the About window and in [CREDITS.md](docs/CREDITS.md).

Translating needs no Rust ([po/README.md](po/README.md)), and a clear bug report
is often as useful as a patch. **[How to contribute →](docs/CONTRIBUTING.md)**

## Contact & support

- Website: [hylki.hyprlab.co](https://hylki.hyprlab.co)
- Discord: [discord.gg/YfEJ4b6PFW](https://discord.gg/YfEJ4b6PFW)
- Email: [hyprlab@proton.me](mailto:hyprlab@proton.me)
- Security issues: see [SECURITY.md](docs/SECURITY.md)
- [Buy me a coffee](https://buymeacoffee.com/hyprlab) ☕

## License

Hylki is free software, licensed under the **GNU Affero General Public License
v3.0 or later** ([AGPL-3.0-or-later](LICENSE)). The third-party marks, logos
and themes it bundles are listed in [docs/LICENSE.md](docs/LICENSE.md).

© 2026 Hyprlab
