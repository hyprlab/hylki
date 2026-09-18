<p align="center">
  <img src="docs/logo.png" width="120" alt="Hylki logo">
</p>

<h1 align="center">Hylki</h1>

<p align="center">
  A fast, <strong>GNOME-native</strong> email client — built with Rust and libadwaita, privacy-first.
</p>

<p align="center">
  <a href="https://hylki.hyprlab.co">Website</a> ·
  <a href="RELEASE_NOTES.md">Release notes</a> ·
  <a href="https://github.com/hyprlab/hylki/releases">Releases</a> ·
  <a href="https://discord.gg/YfEJ4b6PFW">Discord</a>
  <br>
  <img alt="License: AGPL-3.0" src="https://img.shields.io/badge/license-AGPL--3.0-blue">
</p>

<p align="center">
  <img src="docs/screenshot.png" width="900" alt="Hylki showing a unified inbox in light mode">
</p>

---

> [!NOTE]
> **Vireo is now Hylki.** As of v1.35.0 the app formerly known as Vireo (and,
> before v1.6.0, Veem) has a new name and icon. Same app, same code. Install
> Hylki from [hylki.hyprlab.co](https://hylki.hyprlab.co): its first start
> carries your accounts, settings and cached mail across from Vireo, which
> is left untouched until you remove it. vireo.hyprlab.co and getveem.com
> redirect here.

Hylki is a GNOME-native desktop email client for the Linux desktop. It talks
IMAP/SMTP directly, keeps your mail and credentials on your machine, and blocks
trackers by default — no telemetry, no analytics.

## Features

- **Multiple accounts** — IMAP and POP3, each on its own background worker, with a unified *Inboxes* view.
- **OAuth 2.0 sign-in** — Google, Microsoft and custom providers over XOAUTH2, plus import from GNOME Online Accounts.
- **Whole-mailbox sync & search** — no message-count cap; a fast first page loads instantly, the rest indexes in the background with infinite scroll.
- **Two-way sync** — deletions and moves from your phone or another client sync back automatically (IMAP IDLE + reconciliation).
- **Conversation threading**, compose/reply/forward with HTML signatures, editable drafts, and full folder management.
- **Four ways to write a message** — rich text, **Markdown**, hand-written **HTML**, or plain text, switched per message from the composer's format button. See [Writing in Markdown or HTML](#writing-in-markdown-or-html) below.
- **Outbox** — a send that fails is kept and retried when the connection returns, not lost; queued messages can be edited, sent by hand or discarded.
- **Send later** — schedule a message for tomorrow morning, Monday, or any date and time; it waits in the Outbox, editable, until then.
- **Cloud attachments** — upload a large file to your own Nextcloud, ownCloud, OpenCloud or Seafile server, or to OneDrive or Dropbox, and put a share link in the message, with an optional expiry and download password.
- **Message previews** — the first one to three lines of each message under its subject in the list (or off).
- **Single-key shortcuts** — Gmail-style `j`/`k`, `r`, `a`, `d` and friends, without a modifier (see below).
- **Printing** — print a message with its sender, recipients and date, with an in-app preview that also saves straight to PDF.
- **Runs in the background** (optional) — closing the window keeps mail arriving; Hylki appears under *Background Apps* in the GNOME system menu, and can start at login without opening a window.
- **Privacy-first reading** — remote content blocked by default, per-sender allow/block lists, and a per-message light/dark content theme.
- **OpenPGP** — read encrypted and signed mail, sign and encrypt what you send, and manage keys from Settings, through the GnuPG already on your computer. See [OpenPGP](#openpgp-encrypted-and-signed-mail) below.
- **Send from Files** — a *Send with Hylki* entry in the GNOME Files right-click menu sends the selected files into a new message, a draft or a reply of your choosing, with an offer to upload big ones to cloud storage instead (optional; see [below](#send-with-hylki-from-gnome-files)). *Email…* and *Open With Hylki* work too.
- **Appearance themes** — five palettes (Rose, Forest, Tidal, Earth and Midnight) in Settings → Appearance, each carrying its own light and dark version, or the stock GNOME colours Hylki has always worn.
- **GNOME-native** — adaptive three-pane layout, per-account colours and emoji avatars, light/dark following the system, optional GNOME Contacts.

See **[RELEASE_NOTES.md](RELEASE_NOTES.md)** for the full list.

## Keyboard shortcuts

Hylki can be driven from the keyboard without holding a modifier, in the style of
Gmail and Geary. The scheme is **off by default** — a stray keystroke shouldn't
archive mail — so switch it on first in **Preferences → Message List → Single-key
shortcuts**. Press **Ctrl+?** (or F1, or *Main Menu → Keyboard Shortcuts*) at any
time for this list in the app; the same key closes it again.

| Move around | | Act on a message | |
| --- | --- | --- | --- |
| <kbd>j</kbd> <kbd>↓</kbd> | Next message | <kbd>r</kbd> | Reply |
| <kbd>k</kbd> <kbd>↑</kbd> | Previous message | <kbd>R</kbd> | Reply to all |
| <kbd>l</kbd> <kbd>→</kbd> | Open the selected message | <kbd>f</kbd> | Forward |
| <kbd>h</kbd> <kbd>←</kbd> <kbd>u</kbd> | Back to the message list | <kbd>a</kbd> | Archive |
| <kbd>w</kbd> | Next message in the conversation | <kbd>d</kbd> | Delete |
| <kbd>b</kbd> | Previous message in the conversation | <kbd>!</kbd> | Mark as spam |
| <kbd>/</kbd> | Search | <kbd>s</kbd> | Star or unstar |
| <kbd>c</kbd> | Compose | <kbd>m</kbd> | Mark read or unread |
| <kbd>?</kbd> | This list | <kbd>x</kbd> | Select the row (for a bulk action) |
| | | <kbd>1</kbd> … <kbd>9</kbd> | Add or remove a tag (the first nine, in Settings order) |
| | | <kbd>0</kbd> | Remove every tag |

<kbd>Esc</kbd> backs out of a reply, forward or compose and returns you to the
message list. It works whether or not single-key shortcuts are enabled — as does
everything in the menus — and in a search field it still clears the search.

Keys never fire while you are typing: whatever has focus gets first refusal, so
"archive" typed into the search box searches for it.

## Installing

**Flatpak (recommended)** — works on any distribution, on **x86_64 and
aarch64 (ARM64)**; see [hylki.hyprlab.co](https://hylki.hyprlab.co) for the
signed Flatpak repo. Installing from the repo picks the right architecture on
its own:

```sh
flatpak install --user --from https://hylki.hyprlab.co/flatpak/co.hyprlab.Hylki.flatpakref
```

Prefer a direct download? Each release carries `Hylki-x86_64.flatpak` and
`Hylki-aarch64.flatpak`; grab the one matching `uname -m` from the
[latest release](https://github.com/hyprlab/hylki/releases/latest) and run
`flatpak install --user ./Hylki-*.flatpak` — the bundle carries the repo address and
signing key, so it still receives updates from the official repo. (A bundle
holds a single architecture; the repo above holds both.)

**Fedora** — download the `.rpm` from the
[latest release](https://github.com/hyprlab/hylki/releases/latest) and:

```sh
sudo dnf install ./hylki-*.x86_64.rpm
```

**Gentoo** — a community-maintained ebuild lives in
[bennypowers' overlay](https://github.com/bennypowers/gentoo-overlay)
(thanks @bennypowers):

```sh
eselect repository enable bennypowers
emaint sync -r bennypowers
emerge -av mail-client/hylki
```

**Nix** — a community-maintained flake lives in
[tbaumann's fork](https://github.com/tbaumann/hylki) (thanks @tbaumann):

```sh
nix run github:tbaumann/hylki
```

Arch, Debian/Ubuntu and Snap packages were discontinued after 1.7.0 — use the
Flatpak (it works on every distribution) or build from source.

The RPM targets current Fedora releases (44+) on x86_64 only — on ARM, or on
anything older, use the Flatpak or build from source. A Secret Service provider (e.g. gnome-keyring,
preinstalled on GNOME) is needed at runtime for password storage.

## Building from source

Hylki needs the Rust toolchain and the GTK 4 / libadwaita / WebKitGTK 6
development libraries, plus a Secret Service provider (e.g. gnome-keyring) at
runtime.

**Fedora**

```sh
sudo dnf install gtk4-devel libadwaita-devel webkitgtk6.0-devel poppler-glib-devel
```

**Debian / Ubuntu**

```sh
sudo apt install libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev libpoppler-glib-dev
```

**Build & install**

```sh
git clone https://github.com/hyprlab/hylki.git
cd hylki
cargo build --release
./install.sh          # installs the binary, icon and .desktop file into ~/.local
./uninstall.sh        # removes them again (--purge also removes settings and the mail cache)
```

## Configuration

Add accounts from **Settings → Accounts** in the app. For a plain IMAP/SMTP
account you only need the server and an app-specific password. See
[`accounts.toml.example`](accounts.toml.example) for the on-disk format —
passwords you enter there are migrated into the system keyring on first run and
removed from the file.

### OAuth (Google / Microsoft)

**Microsoft** works out of the box — pick *Microsoft* in the account editor and
sign in.

**Google** signs in through **GNOME Online Accounts** — add your Google account in
*GNOME Settings → Online Accounts*, then import it in Hylki. Official builds don't
bundle a Google OAuth client (Google's secret can't live in a public repo), so
GNOME Online Accounts is the standard path. You can also use your own OAuth client
(see below).

To use **your own** OAuth client (a fork, a self-hosted build, or to replace the
bundled ones), put it in `~/.config/hylki/oauth.toml`:

```toml
[google]
client_id = "your-client-id.apps.googleusercontent.com"
client_secret = "your-client-secret"

[microsoft]
client_id = "your-azure-application-client-id"  # public client, no secret

[dropbox]
client_id = "your-dropbox-app-key"  # public client, no secret
```

or via the `HYLKI_GOOGLE_CLIENT_ID` / `HYLKI_GOOGLE_CLIENT_SECRET`,
`HYLKI_MICROSOFT_CLIENT_ID` / `HYLKI_MICROSOFT_CLIENT_SECRET` and
`HYLKI_DROPBOX_CLIENT_ID` environment variables.

**Bundling a Google client at build time** (for maintainers) — set the env vars
during the build and they're compiled in via `option_env!`:

```sh
HYLKI_GOOGLE_CLIENT_ID=... HYLKI_GOOGLE_CLIENT_SECRET=... cargo build --release
```

### Cloud attachments (Nextcloud, OneDrive, Dropbox, Seafile)

Settings → Cloud Storage holds the accounts the composer's upload button can
put files on. A file goes to the account's upload folder and a share link,
with the size and any expiry, is placed in the message above your signature.
Every kind of account can expire links after a number of days and protect
them with a download password, shown to you to pass on separately. The
account's settings are the defaults: the upload dialog in the composer
shows them for each upload, where the expiry can be changed or removed,
the password turned on or off, and a password of your own typed in place
of the generated one.

- **OneDrive** — through GNOME Online Accounts: add your Microsoft 365
  account under Settings → Online Accounts, then pick it in the cloud
  account's editor. GOA holds the sign-in and refreshes the token, so Hylki
  stores no password or key. Uploads go into the upload folder (made when
  missing) and are shared with "anyone with the link". **Link expiry and
  download passwords need a Microsoft 365 subscription or OneDrive for
  Business**: a free personal OneDrive refuses the link when either is
  set, so a new OneDrive account starts with both off, and the editor says
  so. The connection check finds out what the drive's plan allows and
  greys out the rows it rules out, with the reason, in the account's
  settings and in the upload dialog. Uploads and plain links work on any
  OneDrive. Google Drive is not offered: GNOME Online Accounts
  does not ask Google for Drive access on every system, and Hylki carries
  no Google client of its own.
- **Nextcloud, ownCloud, OpenCloud** — the server URL, your user name and an
  app password (made under *Security* in the server's personal settings).
  Uploads go over WebDAV; links come from the files-sharing API.
- **Behind Cloudflare** (Nextcloud-kind and Seafile accounts) — a
  self-hosted server reached through a Cloudflare domain or tunnel cannot
  take a request over 100 MB, Cloudflare's proxy limit. Switch on *Server
  is behind Cloudflare* in the account's editor and files bigger than 90 MB
  go up in 90 MB pieces the server stitches back together (Nextcloud's
  chunked-upload endpoint, Seafile's resumable upload); smaller files go
  as one request as before.
- **Seafile** — the server URL, your e-mail and your password. If the
  account uses two-step verification, also enter the current code from your
  authenticator app: Hylki signs in with it once, gets an API token from the
  server and keeps that in the keyring instead of the password (Seafile's
  web interface shows no such token itself; one obtained another way, say
  from the `api2/auth-token/` endpoint, can be pasted in the password
  field). Uploads go into a library (made when missing, "Hylki" by default)
  and a folder inside it.
- **Dropbox** — sign in through your browser. Dropbox only lets a registered
  app sign in, so make one for yourself; it takes a minute and stays private:
  1. Open [dropbox.com/developers/apps](https://www.dropbox.com/developers/apps)
     signed in to your Dropbox and press **Create app**.
  2. Choose **Scoped access**, then the access type: **App folder** gives
     Hylki its own folder under *Apps* and nothing else, **Full Dropbox** puts
     uploads in the folder named in the account's settings.
  3. Give the app a name no one else has used ("Hylki for Jane", say) and
     press **Create app**.
  4. On the **Permissions** tab tick `account_info.read`,
     `files.content.write` and `sharing.write`, then press **Submit**.
  5. On the **Settings** tab, under *OAuth 2 → Redirect URIs*, enter
     `http://localhost:41597/` and press **Add**. The port is fixed because
     Dropbox matches redirect URIs exactly.
  6. Copy the **App key** from the top of the Settings tab.

  In Hylki, add a Dropbox account under Settings → Cloud Storage, paste the
  app key and press **Connect with Dropbox**; the browser opens, you approve
  the app, and the account's e-mail appears in the dialog. The app can stay
  in *Development* status, which allows your own account (Dropbox asks for a
  production review only past a few hundred users). A build can carry an app
  key of its own (the `[dropbox]` entry in `oauth.toml`, or
  `HYLKI_DROPBOX_CLIENT_ID` at build time), in which case the field can stay
  empty. Link passwords and expiry dates are a paid Dropbox feature; on a
  Basic plan leave both off, or the share step reports it.

### Writing in Markdown or HTML

A message can be written in any of four formats, chosen in **Settings →
Composing → Write messages in** for new messages and switched for any single
message with the format button at the right-hand end of the composer's
formatting row, which wears the icon of the format it is set to and lists
the others, the current one in your accent colour:

- **Rich text** — the WYSIWYG editor with its formatting toolbar. The default.
- **Markdown** — you write Markdown, the recipient gets formatted mail.
- **HTML** — you write the message's HTML by hand.
- **Plain text** — no formatting at all, sent as `text/plain` only.

Markdown and HTML are written as *source*: a monospace field, the formatting
buttons gone from the row above it (they would go nowhere), and a **Preview**
toggle beside the format chooser that swaps the source for the message as it
will be sent. Nothing
is sent as source — Markdown is rendered to HTML when the message goes out,
and the Markdown you wrote travels as the plain-text alternative, so a
recipient whose client shows plain text gets something that still reads as
itself. Hand-written HTML gets a readable plain-text alternative made for it
the same way.

Switching format converts what is already in the message, so you can start a
reply in rich text and finish it in Markdown; the quoted original comes
across as `>` lines.

**The Markdown Hylki understands** is the dialect documented at
[markdownguide.org](https://www.markdownguide.org/) — all of the basic
syntax, and all of the extended syntax:

| | |
| --- | --- |
| Basic | headings (both styles), bold, italic, blockquotes, ordered and unordered lists, code, horizontal rules, links (inline, reference and autolinks), images, hard line breaks, backslash escapes |
| Extended | tables with alignment, fenced code blocks with a language, footnotes, heading IDs (`{#id}`), definition lists, `~~strikethrough~~`, task lists, `:emoji:` shortcodes, `==highlighting==`, `~subscript~` and `^superscript^`, and bare URLs and email addresses turned into links |

Two notes on what that means in mail. The HTML Hylki writes carries its
styling as `style` attributes on the tags themselves, because a `<style>`
block is the first thing most webmail clients throw away — so tables really
do arrive with their borders. And anything you send, in Markdown or in HTML,
passes through a sanitizer on the way out: scripts, event handlers and style
sheets are removed, while tables, inline styles, images and everything
Markdown produces are kept. That is not a defence against you; it is a
defence for you, since a `<script>` in a message is at best stripped by the
recipient's client and at worst the reason the message lands in their spam
folder.

A draft saved from a Markdown or HTML message is stored as ordinary mail (a
formatted part and a plain-text one), so re-opening it puts you in your
default format. Switching that draft back to Markdown converts it, which for
a message that started life as Markdown lands very close to what you wrote.

### OpenPGP (encrypted and signed mail)

Hylki can read OpenPGP-encrypted mail, check signatures, and sign and encrypt
what you send. It does this through **GnuPG** (`gpg`), the same program the
terminal command and other mail clients use, so your keys live in one place
(`~/.gnupg`) and every program on the computer sees the same keyring. Nothing
here needs a terminal.

**What you need**

- The Flatpak build carries GnuPG and is set up already. A source or RPM
  install needs the `gnupg2` package (on Fedora it is installed by default).
- Hylki never stores a decrypted message on disk: an encrypted message is
  decrypted for the reading pane each time you open it, and its body and
  attachments are kept out of the cache.

**1. Make a key for your address**

Open *Settings → OpenPGP*. The top row says whether GnuPG was found. Under
*Your keys*, click **Generate…**, pick the address the key is for, choose how
long it lasts, and enter a passphrase twice. Hylki hands the passphrase to gpg
over a pipe and does not keep it; from then on gpg asks for it when the key is
used, and remembers it for a while (ten minutes by default, the normal
gpg-agent behaviour). The key is a signing key with an encryption subkey, so it
does both jobs.

If you already have a key, click **Import…** instead and pick the key file.

**2. Give people your public key**

Click the export button on your key's row and save the `.asc` file, then send
it to the people who should write to you encrypted, or upload it wherever your
provider publishes keys. The file holds only the public half; the secret half
never leaves your keyring.

**3. Get other people's keys**

Hylki needs a person's public key to encrypt to them and to check their
signature. There are four ways to get one, none of which need a terminal:

- A signed message from someone whose key you don't have shows an amber shield
  beside their name. Click it and choose **Fetch the sender's key**. Hylki
  looks in the message itself first (many clients attach the key in an
  Autocrypt header), then asks the sender's provider (WKD), then the keyservers.
- A message with a key file attached shows an **Import OpenPGP key** button on
  that attachment.
- Under *Settings → OpenPGP → Other people's keys*, **Fetch by address…**
  looks a key up by email address, and **Import…** reads a key file.
- A key someone sends you by any other route can be imported the same way.

**4. Trust a key**

An imported key checks signatures, but until you have vouched for it the
shield stays amber and says the key is not trusted yet. Compare the key's
fingerprint with the one its owner gives you in person, on their website or
over another channel, then click **Trust…** on the key's row (or **Trust this
key…** in the shield's popover). Hylki signs the key locally with your own,
which is what turns the shield green. Trusting a key you have not checked
lets an impostor's signature pass as theirs, so do check.

**5. Send signed or encrypted mail**

The composer has two buttons beside Send: **Sign** and **Encrypt**. Sign adds
a signature others can check with your public key. Encrypt scrambles the
message to every recipient's key and your own, and turns Sign on too. If a
recipient has no key in your keyring, or your address has no key of its own,
Hylki says so before anything is sent. Replying to an encrypted message starts
with Encrypt on.

Each account uses the key whose address matches. To use another key for an
account, open the account in *Settings → Mail Accounts* and choose it under
*OpenPGP*.

**Reading the result**

Beside a sender's name, a lock means the message was encrypted and a shield
means it was signed. Green: everything checks out against a trusted key.
Amber: a doubt, such as an unknown or untrusted key, or an expired one. Red:
a failure, such as a signature that does not match or a message that could not
be decrypted. Click the icon for the details.

**If something goes wrong**

- *GnuPG was not found*: install the `gnupg2` package.
- *No passphrase prompt appears when you expect one*: gpg-agent remembers a
  passphrase for a while after you type it. `gpg-connect-agent reloadagent /bye`
  clears it, or shorten `default-cache-ttl` in `~/.gnupg/gpg-agent.conf`.
- *Could not be decrypted*: the message was encrypted to a key you do not
  have. Ask the sender to use the public key you exported in step 2.
- *Nothing to encrypt with for an address*: that person's key is missing;
  see step 3.

### Send with Hylki from GNOME Files

Select files in GNOME Files (Nautilus), right-click, *Send with Hylki*: a new
message opens with them attached. The entry comes from a small extension that
Files loads, so it has to live outside the app, in your home folder.

**What you need**

- The `nautilus-python` bindings, which let Files load extensions written in
  Python: `sudo dnf install nautilus-python` on Fedora, `sudo apt install
  python3-nautilus` on Debian and Ubuntu, `sudo pacman -S python-nautilus` on
  Arch.
- Hylki 1.29 or newer, Flatpak or native.

**Installing it**

Open **Settings → System → GNOME Files** and click **Install**. Hylki writes
the extension to `~/.local/share/nautilus-python/extensions/hylki-nautilus.py`
and shows whether the installed copy is this version's. Then click
**Restart** (or run `nautilus -q`): Files closes its windows, and the next one
opens with the entry. Once Files has loaded the extension the row says so; if
it still says "not loaded" after a restart, the `nautilus-python` package is
the usual reason (a native install of Hylki checks for it and tells you; the
Flatpak cannot see the host's packages). Until Files has loaded the extension,
the group also shows the install command for your distribution, with a copy
button. **Remove** takes it out again the same way.

Without the app, the same file is in the repository at
`data/nautilus/hylki-nautilus.py`:

```sh
mkdir -p ~/.local/share/nautilus-python/extensions
curl -fsSL -o ~/.local/share/nautilus-python/extensions/hylki-nautilus.py \
  https://raw.githubusercontent.com/hyprlab/hylki/main/data/nautilus/hylki-nautilus.py
nautilus -q
```

The extension launches Hylki by its desktop id (`co.hyprlab.Hylki`, then the
beta's), so it works whichever way Hylki is installed. Folders are skipped;
files on a mounted share reach the app through their mount path.

Files also has its own *Email…* entry, which sends the selection to whatever
app handles `mailto:` links; if that is Hylki, it does the same thing without
the extension.

**What the files go into**

When the files arrive, Hylki asks what they are for: a **new message**, a
**draft** you pick from a list of every account's drafts, or a **reply** to a
message you pick (the one you are reading comes first; a search box narrows
the list by sender, subject or account). Files that together exceed the size
limit (20 MB by default) bring a second question when a cloud storage account
is set up: attach them anyway, or upload them and put download links in the
message instead. Each dialog has an *Always do this* box, and **Settings →
System → GNOME Files** holds the same choices, so the questions can be
skipped: what the files go into, what happens over the limit, and the limit
itself.

## Privacy

Hylki collects no telemetry and sends no analytics. Remote content in messages is
blocked by default to defeat tracking pixels. Passwords and OAuth refresh tokens
live in the system keyring (secret-service), never in plain files.

## The Hylki Manifesto

Hylki exists to fill a need in the Linux desktop community for a modern,
GNOME-native email client that doesn't sacrifice aesthetics for features.

The project has the following foundational values that guide its development:

- **Hylki is committed to free and open source software.** Hylki will never be
  for sale and is committed to remaining that way through our AGPLv3 license
  adherence.
- **This project is community-driven and committed to putting humans at the
  center of everything we do.** Our aim is to improve the lives of our users
  and the Linux desktop as a whole.
- **We are committed to GNOME-first development** ensuring 100% compatibility
  with the latest GNOME release. This app was conceived for GNOME and it will
  remain the desktop environment we target primarily.
- **Aesthetics matter as much as features.** Hylki should make using email on
  the Linux desktop both visually pleasing and enjoyable through consistent and
  familiar UI/UX paradigms. New users to GNOME should be able to intuit how to
  use the app without needing to refer to documentation.
- **Feature-rich and choice-forward philosophy.** Providing means for the user
  to maximally customize the app's feature set is a high priority. We maintain
  that Hylki is both beautiful and highly functional to tackle every email edge
  case.

## AI notice

Hylki is built by a human maintainer working with generative AI as a
development tool:

- **Code** — the large majority of the Rust code in this repository was written
  with Anthropic's Claude (via Claude Code), working from the maintainer's
  direction. The maintainer decides what gets built, reviews the results, tests
  every release, and signs off on everything that ships.
- **Text** — documentation, release notes, and website copy are largely
  AI-drafted and human-edited.
- **Artwork** — the app icon and other visual assets are human-made, without
  generative AI.
- **The app itself contains no AI.** Hylki has no AI features, makes no
  requests to AI services, and never sends your mail or any other data to one —
  AI was used to *build* the app, not to run it. See [Privacy](#privacy).

Bug reports and pull requests are welcome from humans and their AI tools alike;
everything merged gets the same human review.

## Contributors

Hylki is maintained by Hyprlab. Thanks to the people who have sent patches
upstream — their work ships in the app and is credited in the About window:

- [**Alfonso Lizárraga**](https://github.com/alfonsolzrg) ([#14](https://github.com/hyprlab/hylki/pull/14)) — sending
  to recipients with punctuated or accented names, the startup message list,
  message-list rebuild performance, the unread dot, and the Attachments-row
  setting.
- [**Chris Pouliot**](https://github.com/chrispouliot) ([#13](https://github.com/hyprlab/hylki/pull/13)) — Proton
  Bridge connections: IMAP STARTTLS and locally signed certificates.
- [**Isaac**](https://github.com/thecalamityjoe87) ([#31](https://github.com/hyprlab/hylki/pull/31),
  [#43](https://github.com/hyprlab/hylki/pull/43), [#44](https://github.com/hyprlab/hylki/pull/44),
  [#49](https://github.com/hyprlab/hylki/pull/49), [#63](https://github.com/hyprlab/hylki/pull/63),
  [#135](https://github.com/hyprlab/hylki/pull/135), [#142](https://github.com/hyprlab/hylki/pull/142)) — PDF first-page thumbnails
  in the attachment gallery and drawer, the fix for attachments not opening
  (wrong O_NOFOLLOW constant + portal-based launching), the reader header's
  "To:" line, the preference to always load remote content, the shared
  GNOME-styled right-click context menus, swipe-to-archive/delete on
  message rows, and the uninstall script.
- [**Alexander Lubovenko**](https://github.com/typedev) ([#45](https://github.com/hyprlab/hylki/pull/45),
  [#110](https://github.com/hyprlab/hylki/pull/110), [#112](https://github.com/hyprlab/hylki/pull/112),
  [#118](https://github.com/hyprlab/hylki/pull/118),
  [#127](https://github.com/hyprlab/hylki/pull/127),
  [#216](https://github.com/hyprlab/hylki/pull/216)) — Gmail
  conversations: showing a message once rather than once per label, answering it
  from whichever label already holds its body or attachments, and fetching a
  conversation's bodies in one request instead of one apiece; listing small
  attachments sent from web Gmail that the inline-image heuristic hid;
  fetching a labelled message's attachments once instead of once per label;
  rejoining filenames split across RFC 2047 encoded-words; and pictures
  dropped or pasted from a file manager landing in the message, keeping their
  filename, with resizing by handle or menu and an optional recompress on send;
  and the non-ASCII part header that took an account's mail thread down.
- [**frenchy82**](https://github.com/frenchy82) ([#122](https://github.com/hyprlab/hylki/issues/122),
  [#131](https://github.com/hyprlab/hylki/pull/131),
  [#134](https://github.com/hyprlab/hylki/pull/134)) — the French translation,
  Hylki's first, and the report that found the labels the app was showing in
  English despite having the translation.
- [**Laszlo Lang**](https://github.com/7system7) ([#169](https://github.com/hyprlab/hylki/pull/169)) — the
  Hungarian translation.
- [**Ilya Semenkovich**](https://github.com/iliasen) ([#176](https://github.com/hyprlab/hylki/pull/176),
  [#185](https://github.com/hyprlab/hylki/pull/185)) — the Russian translation, two
  reader tooltips that could not be translated, and the About menu entry that
  could not be either.
- [**Paulo Fino**](https://github.com/somepaulo) ([#178](https://github.com/hyprlab/hylki/pull/178),
  [#179](https://github.com/hyprlab/hylki/issues/179),
  [#182](https://github.com/hyprlab/hylki/pull/182),
  [#183](https://github.com/hyprlab/hylki/issues/183),
  [#194](https://github.com/hyprlab/hylki/pull/194)) — the Portuguese (Portugal)
  and Brazilian Portuguese translations, the request for a language chooser, and
  the report that the chosen language never reached the Flatpak.
- [**Anton Palgunov**](https://github.com/Toxblh) ([#7](https://github.com/hyprlab/hylki/pull/7),
  [#8](https://github.com/hyprlab/hylki/pull/8)) — sender avatars from GNOME
  Contacts photos, and GNOME Online Accounts refinements: custom server ports
  (IPv6 included), pausing an account while its Mail service is off in GNOME
  Settings, OAuth-aware connection tests, and a timeout on stalled IMAP
  connections.
- [**Yiannis Ioannides**](https://github.com/yioannides) ([#75](https://github.com/hyprlab/hylki/pull/75)) — the
  `--user` flag in the Flatpak install instructions, so a local install no
  longer asks for root; and a long run of requests and design feedback that
  shaped tags, split replies, the reader's own font and colours, Empty Trash
  and the reply panel's fields.

Not every contribution is code. Thanks to
[**p-mitana**](https://github.com/p-mitana) for a thorough round of design
feedback — reader, composer and GNOME-HIG suggestions, and a string of sharp
bug reports — that shaped the 1.15 releases; to
[**7system7**](https://github.com/7system7) for the HTML-signature and `mid:`
link requests and [**EmmanuelP**](https://github.com/EmmanuelP) for tracking
down the deletes that failed on Zimbra, both in 1.22; to
[**7system7**](https://github.com/7system7) again for Send Later and cloud
attachments, [**yioannides**](https://github.com/yioannides) for Empty Trash,
the reply-panel fields and the single-message card, and
[**EmmanuelP**](https://github.com/EmmanuelP) for the quote-folding and
Quote-button reports, all in 1.25; to
[**yioannides**](https://github.com/yioannides) for the sidebar
consolidation proposal, picture avatars and the reply-target and menu
reports, [**7system7**](https://github.com/7system7) for the preview
charset and tag-refresh reports, [**Peter Weiss**](https://github.com/peterweissdk)
for the Move To button and [**frenchy82**](https://github.com/frenchy82) for
Not Spam, all in 1.27; to [**yioannides**](https://github.com/yioannides)
for the Inboxes and notification report, [**Peter Weiss**](https://github.com/peterweissdk)
for conversation moves, [**taprobane99**](https://github.com/taprobane99)
for the clock-format report and [**somePaulo**](https://github.com/somepaulo)
for the language chooser, plain-text composing and monospace requests,
all in 1.28; to [**EmmanuelP**](https://github.com/EmmanuelP) for the
undo and sent-copy requests, [**yioannides**](https://github.com/yioannides)
for the recipient-rule report, [**7system7**](https://github.com/7system7)
for the links report and [**frenchy82**](https://github.com/frenchy82) for the
French update, all in 1.32; to [**p-mitana**](https://github.com/p-mitana)
for the reply-target, thread-selection, reply-placement and per-message
attachment reports, with [**yioannides**](https://github.com/yioannides)
weighing in, all in 1.33; and to everyone who files issues and ideas.

Pull requests are welcome. There's no CLA — by opening one you agree your
contribution ships under the [AGPL-3.0-or-later](LICENSE), and it may be adapted
before it lands (with the change explained on the pull request).

## Contact & support

- Website — [hylki.hyprlab.co](https://hylki.hyprlab.co)
- Discord — [discord.gg/YfEJ4b6PFW](https://discord.gg/YfEJ4b6PFW)
- Email — [hyprlab@proton.me](mailto:hyprlab@proton.me)
- [Buy me a coffee](https://buymeacoffee.com/hyprlab) ☕

## License

Hylki is free software licensed under the **GNU Affero General Public License
v3.0 or later** ([AGPL-3.0-or-later](LICENSE)).

The Nextcloud, ownCloud, OpenCloud, OneDrive, Dropbox and Seafile marks shown
in Settings are trademarks of their owners, used only to identify those
services, and are not covered by that licence (see
[data/brands/README.md](data/brands/README.md)). Hylki is not affiliated with
or endorsed by any of them. The same goes for the bundled sender logos that
can fill a sender's avatar: they come from
[gilbarbara/logos](https://github.com/gilbarbara/logos) (MIT) and
[Simple Icons](https://simpleicons.org) (CC0), remain their owners' marks, and
are listed with their sources in [data/logos/README.md](data/logos/README.md).

The appearance themes (Rose, Forest, Tidal, Earth and Midnight) are the theme
library from [T3 Code](https://github.com/pingdotgg/t3code) (MIT, © 2026 T3
Tools Inc.), converted to sRGB, renamed, and mapped onto libadwaita's colour
roles by `tools/gen-themes.py`. Hylki is not affiliated with or endorsed by
T3 Tools.

© 2026 Hyprlab
