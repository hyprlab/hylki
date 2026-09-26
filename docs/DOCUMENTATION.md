# Documentation

How to set up the parts of Hylki that need more than a switch. The
[README](../README.md) covers installing, [FEATURES.md](FEATURES.md) lists
what the app does, and [KEYBOARD_SHORTCUTS.md](KEYBOARD_SHORTCUTS.md) the
keyboard.

**Contents**

- [Configuration](#configuration)
- [GNOME Online Accounts](#gnome-online-accounts)
- [OAuth (Google / Microsoft)](#oauth-google--microsoft)
- [Cloud attachments (Nextcloud, OneDrive, Dropbox, Seafile)](#cloud-attachments-nextcloud-onedrive-dropbox-seafile)
- [Writing in Markdown or HTML](#writing-in-markdown-or-html)
- [OpenPGP (encrypted and signed mail)](#openpgp-encrypted-and-signed-mail)
- [Send with Hylki from GNOME Files](#send-with-hylki-from-gnome-files)
- [Privacy](#privacy)

## Configuration

Add accounts from **Settings → Accounts** in the app. For a plain IMAP/SMTP
account you only need the server and an app-specific password. See
[`accounts.toml.example`](../accounts.toml.example) for the on-disk format:
passwords you enter there are migrated into the system keyring on first run and
removed from the file.

### A certificate in another name

Shared hosting often serves mail for many domains under one certificate in
the host's own name, so `mail.example.org` answers with a certificate for
`server12.hostingcompany.net` and the connection test reports that the names
differ. **Settings → Accounts → the account → Accept a certificate for
another name** waives that one check for the account's IMAP, POP3 and SMTP
connections; the certificate must still be valid and signed by a trusted
authority. Leave it off unless the test names this problem, and prefer
entering the host the certificate is actually for when you know it. Stored
on the account as `tls_accept_hostname_mismatch` in `accounts.toml`. The
connection test's text can be selected and copied.

### GNOME Online Accounts

Any mail account set up in *GNOME Settings → Online Accounts* can be
imported: Google, Microsoft 365 and plain **IMAP and SMTP** accounts alike.
They are listed on the first-run wizard's first account page, which can be
skipped, and under **Settings → Accounts → GNOME Online Accounts**, where a
switch brings one into Hylki.

GNOME keeps such an account's address, servers and password, and Hylki
follows it. The password is read from GNOME Online Accounts each time the
account connects, and server changes made in GNOME Settings are picked up
while Hylki runs, so nothing is typed twice. For an IMAP and SMTP account
Hylki also uses what GNOME records about each server: TLS from the start or
a STARTTLS upgrade, whatever the port, and a certificate accepted in GNOME
Settings although it does not verify, as a home server's self-signed one
does not. Stored on the account as the `security` table in `accounts.toml`.

### JMAP (Stalwart, Fastmail)

A JMAP account (RFC 8620 and 8621) reads and sends mail over HTTPS, so it
needs one server address and no SMTP settings. Pick **Stalwart (JMAP)** in
the Provider list, or any provider and **JMAP** as the Incoming Protocol,
and enter the server: a host name (`mail.example.org`, reached over HTTPS
on 443, or the port in the Port row) or a full URL. Hylki reads the session
resource at `/.well-known/jmap` and takes the API, download, upload and
push addresses from it.

A server names itself in that session, with the host it was set up with.
When the address is entered as a URL with its scheme (`http://10.0.0.5:8080`,
say, for a server on a private network or behind a tunnel), Hylki uses that
origin in place of the one the server advertises for itself; a URL on a
different host, such as the separate one Fastmail serves attachments from,
is left as advertised.

Folders come with their roles from the server, so Sent, Drafts, Junk and
Trash need no detection. Read state, stars and tags are the server's own
keywords, a message keeps its id when it moves, and threading uses the
Message-ID, In-Reply-To and References headers the listing carries. Sending
goes through the server's `EmailSubmission`, which files the copy in Sent
(or the folder chosen under Sent copies) before the send is reported done.
New mail arrives over the server's EventSource push channel when push is
on for the account, with a poll as the fallback. Marking a message as spam
or not spam sets the `$junk` and `$notjunk` keywords the server's filter
learns from.

Tested against Stalwart; Fastmail speaks the same standard but has not been
tried by hand. In `accounts.toml` the account has `protocol = "jmap"` and
the server in `imap_host`.

### Folder order

An account's main folders (Inbox, Drafts, Sent, Archive, Junk, Trash) come
first, in that order, then its custom folders, each under its parent. How
the custom folders are sorted is chosen in **Settings → Sidebar → Folder
order**, for every account:

- **Custom Order** (the default): by the name shown, with any folder you
  dragged kept where you put it. A folder that was never dragged, such as
  one created later, goes just after the sibling that comes before it by
  name.
- **Name (A to Z)** and **Name (Z to A)**: by the name shown, whatever was
  dragged. Gmail's `[Gmail]/Important` sorts as "Important", as it does in
  Thunderbird.
- **Full Path**: by the whole path on the server, the way Gmail on the web
  lists labels.

An account can have its own: **Folder Order → Sort by** in the account's
settings (Settings → Mail Accounts → the account), or the right-click menu
of the account's **Folders** heading in the sidebar. **Follow Settings** goes back
to the choice in Settings → Sidebar.

To put folders in an order of your own, drag them up or down the list.
While a folder is dragged, a line in the accent color shows the gap
between folders where it will land, indented to the level it lands at.
Dragging a custom folder puts its account in Custom Order, every other
folder staying where the previous sort showed it.

- In a gap among its own siblings, the folder only changes places.
- In a gap at another level, the folder is moved to that level on the
  server, the same move as dropping it on a folder, and then takes that
  place. Its sub-folders go with it.
- Over the middle third of a folder, that folder is outlined, and the
  dragged folder is moved inside it on the server.

The main folders only change places among themselves, and keep their order
whichever sort is chosen. Gmail lists some folders inside `[Gmail]`, which
is not a folder of its own; they are drawn at the top level, and only
folders already inside `[Gmail]` can be dropped among them. Renaming a
folder keeps its place.

The order is kept on this computer, in `sidebar.toml`, and an account's
own sort in `accounts.toml` as `folder_sort`; only a move to another level
changes anything on the server. IMAP has no folder order, so other mail
apps and Gmail on the web each keep their own. **Reset Folder Order**, in
the right-click menu of the account's header or its Folders heading, forgets
what was dragged. The item is there only once something has been.

Only the chevrons open and close the items in the sidebar. Clicking a
folder opens it and leaves its sub-folders as they were. Clicking the name
of the Folders, Filters or Tags heading does nothing; the chevron beside it
opens or closes the section. Clicking Inboxes, Starred, Sent, Drafts,
Archive, Filters or Tags in the unified section opens the combined list
without showing each account's folder under it. In the icon rail, which
has no chevrons, a click on a section's icon opens or closes it, and a
long press opens or closes the unified rows.

### Hiding folders

Right-click a folder in the sidebar and choose **Hide Folder** to take it out
of the sidebar and out of syncing: the folder is not listed, its unread count
is not fetched, and its mail is not indexed until it is shown again. Only
plain folders can be hidden; the folders holding a role (Sent, Drafts,
Trash, Junk, Archive) stay, since mail is filed into them. Hiding a folder
hides its sub-folders with it.

The hidden folders are listed under **Settings → Accounts → the account →
Hidden Folders**, each with a **Show** button that brings it back on Save.
They are stored on the account as `hidden_folders` in `accounts.toml`.

An Exchange server lists its calendar, contacts, tasks, notes and journal
folders over IMAP as if they were mail folders, along with Outbox, Sync
Issues, Conversation History, Scheduled and Snoozed. Hylki hides these the
first time it lists such an account, and only when the listing looks like
Exchange (at least two of Calendar, Contacts, Tasks and Journal at the top
level), so a "Notes" folder on any other server is left alone. The look is
taken once per account; a folder brought back from the Hidden Folders list
stays back.

### Keeping an account out of All Inboxes

**Settings → Accounts → the account → Show in All Inboxes** decides whether
the account's mail is merged into the unified section at the top of the
sidebar. Switched off, the account's folders are left out of the unified
Inboxes, Starred, Sent, Drafts and Archive rows, their account lists and
unread counts, and the account's filter folders and tagged mail are left
out of the unified Filters and Tags. The account keeps its own section in
the sidebar, and a search still covers its mail.

The tray icon's count and new-mail notifications still include the
account. Clicking a notification for it opens the account's own Inbox.
With fewer than two accounts left in the unified section, it is not shown,
as with a single account, and **Open at startup: All Inboxes** opens the
first account's Inbox instead.
Stored on the account as `in_unified = false` in `accounts.toml`; without
the key an account is included.

### Moving mail to another account

**Move To** lists the folders of the account the mail is in first, then those
of every other account that can take mail, each under its account's name.
Dragging messages onto another account's folder in the sidebar does the same.

The message is copied into the other account with its read and starred state.
Only once that copy is stored is the original moved to its own account's
Trash (or erased, where the account has no Trash), so a move that fails leaves
the message where it was and says why.

**Undo** (Ctrl+Z) takes the original back out of its account's Trash and puts
the copy in the other account's Trash; Redo does the move again. A move from
or to an account without a Trash folder cannot be undone.

IMAP and JMAP accounts can receive mail this way. A Microsoft account and a
POP3 account can be moved from but not into: Microsoft files a message added
to a folder as a draft, and POP3 has only an inbox.

### Deleting an attachment from the server

**Delete from Server…** in the right-click menu of a file in the attachment
drawer, or in the attachment gallery, takes that one file out of the message
on the server, to save space there. The rest of the message stays: its text,
its other files, where it is filed, its read, starred and tag state, and its
date. Hylki asks first, as the change reaches every device that reads the
account and cannot be undone. Save the file first if you want to keep a copy.
While the server works, the file shows pale red with "Deleting…" in place of
its buttons, then fades out; if the server refuses, it comes back as it was.

An IMAP or JMAP server cannot edit a message, so Hylki stores a copy without
the file and then deletes the original; the copy stands where the original
was. In place of the file the copy carries a short note in the format
Thunderbird uses, so Thunderbird shows it as a deleted attachment and Hylki
leaves it out of the list. A Microsoft account deletes the file from the
message itself.

It is refused where it would not save space or would break the message:

- **Gmail**, which keeps every message in All Mail as well, so the original
  would stay there, file and all.
- **POP3**, which has no way to change a message on the server.
- **A signed or encrypted message**, where removing a file breaks the
  signature or cannot be done at all.
- **A message that is nothing but the file**, which is better deleted whole.

### OAuth (Google / Microsoft)

**Microsoft** works out of the box: pick *Microsoft* in the account editor and
sign in.

**Google** signs in through **GNOME Online Accounts**. Add your Google account in
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

**Bundling a Google client at build time** (for maintainers): set the env vars
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

- **OneDrive:** through GNOME Online Accounts: add your Microsoft 365
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
- **Nextcloud, ownCloud, OpenCloud:** the server URL, your user name and an
  app password (made under *Security* in the server's personal settings).
  Uploads go over WebDAV; links come from the files-sharing API.
- **Behind Cloudflare** (Nextcloud-kind and Seafile accounts): a
  self-hosted server reached through a Cloudflare domain or tunnel cannot
  take a request over 100 MB, Cloudflare's proxy limit. Switch on *Server
  is behind Cloudflare* in the account's editor and files bigger than 90 MB
  go up in 90 MB pieces the server stitches back together (Nextcloud's
  chunked-upload endpoint, Seafile's resumable upload); smaller files go
  as one request as before.
- **Seafile:** the server URL, your e-mail and your password. If the
  account uses two-step verification, also enter the current code from your
  authenticator app: Hylki signs in with it once, gets an API token from the
  server and keeps that in the keyring instead of the password (Seafile's
  web interface shows no such token itself; one obtained another way, say
  from the `api2/auth-token/` endpoint, can be pasted in the password
  field). Uploads go into a library (made when missing, "Hylki" by default)
  and a folder inside it.
- **Dropbox:** sign in through your browser. Dropbox only lets a registered
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

### Where the signature goes

In a reply or forward the account's signature is placed above the quoted
message, so it closes what you wrote rather than what the other person did.
**Settings → Composing → Signature in replies** moves it below the quoted
message instead, the placement Hylki had before 1.38. The setting applies
when a composer opens; a draft keeps its signature wherever it was saved.

### Replies and forwards

A reply or a forward opens in the reading pane, beside the message it
answers. **Settings → Composing → Reply and forward in the main window**,
switched off, opens them in a window of their own instead, as **Compose in
the main window** does for a new message.

The quoted message keeps its layout: its colors, fonts, tables and the
pictures it carries inside itself look as they do in the reader, and go to
the recipient that way. Pictures on the sender's server show only when the
reader shows them for that message, so answering a message does not load
anything reading it did not. The recipient still gets them. In dark mode a
quoted message that sets its own colors keeps the light ground it was
designed for.

### Return and Shift+Return

In the rich text editor <kbd>Return</kbd> starts a new line in the same
paragraph, and <kbd>Shift+Return</kbd> ends the paragraph with a hard
return, which leaves a space before the next one. **Settings → Composing →
Return starts a new paragraph** swaps the two. In a list <kbd>Return</kbd>
still starts the next item, and pressed twice in a quote it still leaves the
quote. The setting applies when a composer opens. Markdown, HTML and plain
text are written as source, where <kbd>Return</kbd> is always a new line.

### Writing in Markdown or HTML

A message can be written in any of four formats, chosen in **Settings →
Composing → Write messages in** for new messages and switched for any single
message with the format button at the right-hand end of the composer's
formatting row, which wears the icon of the format it is set to and lists
the others, the current one in your accent colour:

- **Rich text:** the WYSIWYG editor with its formatting toolbar. The default.
- **Markdown:** you write Markdown, the recipient gets formatted mail.
- **HTML:** you write the message's HTML by hand.
- **Plain text:** no formatting at all, sent as `text/plain` only.

Markdown and HTML are written as *source*: a monospace field, the formatting
buttons gone from the row above it (they would go nowhere), and a **Preview**
toggle beside the format chooser that swaps the source for the message as it
will be sent. Nothing
is sent as source. Markdown is rendered to HTML when the message goes out,
and the Markdown you wrote travels as the plain-text alternative, so a
recipient whose client shows plain text gets something that still reads as
itself. Hand-written HTML gets a readable plain-text alternative made for it
the same way.

Switching format converts what is already in the message, so you can start a
reply in rich text and finish it in Markdown; the quoted original comes
across as `>` lines.

**The Markdown Hylki understands** is the dialect documented at
[markdownguide.org](https://www.markdownguide.org/): all of the basic
syntax, and all of the extended syntax:

| | |
| --- | --- |
| Basic | headings (both styles), bold, italic, blockquotes, ordered and unordered lists, code, horizontal rules, links (inline, reference and autolinks), images, hard line breaks, backslash escapes |
| Extended | tables with alignment, fenced code blocks with a language, footnotes, heading IDs (`{#id}`), definition lists, `~~strikethrough~~`, task lists, `:emoji:` shortcodes, `==highlighting==`, `~subscript~` and `^superscript^`, and bare URLs and email addresses turned into links |

Two notes on what that means in mail. The HTML Hylki writes carries its
styling as `style` attributes on the tags themselves, because a `<style>`
block is the first thing most webmail clients throw away, so tables really
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

To sign everything an account sends, turn on **Sign messages by default**
under the same *OpenPGP* group. New messages, replies and forwards from that
account then open with Sign on, and a reopened draft does too. Changing From
in the composer moves Sign to the new account's setting until you press Sign
yourself; after that the composer leaves it as you set it. Stored on the
account as `sign_by_default = true` in `accounts.toml`.

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

### Dragging files into a message

Files dragged from a file manager over a composer bring up a card for each
place they can go, and the files go where they are let go:

- **Attach** sends them with the message.
- **Insert in Text** places the pictures in the message where the cursor is
  (at the top when the cursor is not in the text), and attaches any other
  file. It is offered when the files include a PNG, JPEG, GIF, WebP, BMP,
  SVG or AVIF picture of 32 MB or less, and only while the message is being
  written as rich text: plain text has nowhere to put a picture, and in
  Markdown or HTML the reference is yours to write.
- **Upload to Cloud** opens the upload dialog for the files, as the
  composer's cloud button does. It is offered when a cloud storage account
  is set up (see [Cloud attachments](#cloud-attachments-nextcloud-onedrive-dropbox-seafile)).

Letting go between the cards attaches the files. A composer in a window of
its own shows the cards too.

Dragged over the main window with no message being written in it, files
bring up the same cards side by side, each starting a new message: **Attach
to New Message**, **Insert in New Message** and **Share Link in New
Message**, offered on the same terms (Insert in New Message when new
messages start as rich text, in **Settings → Composing → Write messages
in**). Attach to New
Message asks about files over the size limit the way *Send with Hylki* does
(below). While a message is being written in the main window, files dropped
anywhere else in it go into that message. Folders are skipped.

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

### Text size

**Settings → Appearance → Text size** makes Hylki's text smaller or larger
than the desktop's, from 90% to 150%: the sidebar, the message list,
Settings and the text of messages. Icons keep their size. Default follows
the desktop's own text scaling. Stored as `text_scale` in `privacy.toml`.

### App icon

**Settings → Appearance → App icon** puts one of five icons on the app's
launcher: the default, the same envelope on a blue, navy or yellow square,
or the classic icon. An icon set on the launcher some other way, with a menu
editor or by editing its `.desktop` file, is left alone when Hylki starts;
Settings says so above the gallery, and picking an icon there replaces it.

### Unread count on the app icon

Hylki puts the number of unread inbox messages on its icon in the dock or
task manager, the same count as the tray icon's dot. KDE Plasma's task
manager shows it, and so do the Dash to Dock and Dash to Panel extensions
for GNOME; GNOME's own dash has no badges. **Settings → System → Unread count
on the app icon** turns it off. Stored as `launcher_count` in `privacy.toml`.

### Notifications

A new-mail notification opens the message when clicked. When it is about a
single message it also carries up to three buttons. **Mark as Read**,
**Archive**, **Delete** (to Trash, with the usual undo in the window) and
**Mark as Spam** act on the message without raising the window; **Reply**
and **Forward** open the message with the composer started. Settings →
General → Notification Buttons picks any three of the six (Mark as Read,
Archive and Delete to begin with); a notification that sums up several new
messages carries none. Stored as `notification_buttons` in `privacy.toml`.

**Settings → General → Sound for new mail** plays a sound with each new-mail
notification. It is off until switched on. **Sound** picks one of GNOME's four
alert sounds (Click, Hum, String and Swing, built into Hylki) or **Custom
File**, which adds a **Choose…** button for a file of your own (any format
GStreamer can play, such as WAV, MP3, OGG or FLAC). Picking a sound plays it,
and so does the play button. A custom file is copied to
`~/.local/share/hylki/notification-sound/`, so the original can be moved or
deleted; the choice is stored in `sound.toml`. The desktop's own sound theme
is not offered: the Flatpak cannot read the host's sounds. Several accounts
receiving mail at once play the sound once. It is not played when event
sounds are switched off in GNOME, or, in a native install, while Do Not
Disturb is on. The Flatpak cannot see Do Not Disturb: the desktop does not
share that setting with sandboxed apps. The desktop may play a sound of its
own for the notification as well; GNOME Settings → Notifications → Hylki →
Sound Alerts turns that one off.

## Privacy

Hylki collects no telemetry and sends no analytics. Remote content in messages is
blocked by default to defeat tracking pixels. Passwords and OAuth refresh tokens
live in the system keyring (secret-service), never in plain files.

**Where things live.** Settings are TOML files in `~/.config/hylki/`
(`accounts.toml`, `privacy.toml`, `filters.toml`, `tags.toml` and friends), the
mail index and cached messages in `~/.cache/hylki/`, and account avatars in
`~/.local/share/hylki/`. A Flatpak install keeps the same layout under
`~/.var/app/co.hyprlab.Hylki/`. Nothing else on the computer is written to, and
`./uninstall.sh --purge` (or removing those directories) takes it all away.

Passwords and tokens are never in those files: they go to the system keyring
through the Secret Service. Decrypted OpenPGP messages are never cached at all.
