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
- [LDAP directories](#ldap-directories)
- [Translating messages](#translating-messages)
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
needs one server address and no SMTP settings. The Provider list has three
JMAP entries:

- **Fastmail (JMAP)** fills in Fastmail's session URL and signs in with an
  API token rather than the account password. Make the token in Fastmail's
  settings, under Privacy & Security, with access to mail and to sending
  it, and enter it in the API Token row.
- **Stalwart (JMAP)** and **JMAP Server** take the server as a host name
  (`mail.example.org`, reached over HTTPS on 443, or the port in the Port
  row) or a full URL, and sign in with the username and password. Turn on
  **Sign in with an API token** for a server that takes a token instead.
  They differ only in the account's mark: Stalwart's logo, or a red JMAP
  tile.

For a host name, Hylki reads the session resource at `/.well-known/jmap`.
A URL with a path is tried as the session resource itself
(`https://api.fastmail.com/jmap/session`), then as a prefix with
`/.well-known/jmap` under it. The API, download, upload and push addresses
come from the session.

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
learns from. The server's sending identities are offered in the composer's
From row (see [The From address](#the-from-address)), and an alias has no
SMTP settings of its own.

Tested against Stalwart, with a password and with a bearer token; Fastmail
speaks the same standard but has not been tried by hand. In `accounts.toml`
the account has `protocol = "jmap"`, the server in `imap_host`, and
`jmap_token = true` when it signs in with a token, which the keyring keeps
where a password would be. An account made from the JMAP Server entry has
`jmap_generic = true`.

### Folder order

An account's main folders (Inbox, Drafts, Sent, Archive, Junk, Trash) come
first, in that order, then its custom folders, each under its parent.
Folders kept inside the Inbox on the server (`INBOX.Ablage`, `INBOX/Lists`)
are listed under the Inbox row, with an arrow on it to fold them away, when
the account has folders outside the Inbox too, as Roundcube and Apple Mail
show them. A server that keeps every folder inside the Inbox (an `INBOX.`
prefix, as on Courier and some Dovecot setups) has them under **Folders**
instead, without the prefix. How
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

### The People view

The **People** button in the sidebar header swaps the folder list for the
people you exchange mail with, newest exchange first, each with a count of
their unread mail. Picking a person shows the mail between the two of you
in one list: what they sent you and what you sent them, from the Inbox,
Sent, Archive and every other folder except Trash, Junk and Drafts, across
the accounts in All Inboxes. **All People** at the top shows all of that
mail together.

Mail you receive belongs to its sender, and mail you send belongs to each
of its To and Cc recipients. Your account addresses and their aliases are
never listed. The field above the list filters it by name or address, and
right-clicking a person offers **Write To** and **Copy Address**.

While the list is shown, clicking a new-mail notification opens the
sender's mail in it. The button brings the folders back, and the choice is
kept for the next start. The list is off until switched on.

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

**Microsoft** signs in with Hylki's own app, with or without GNOME Online
Accounts: pick *Microsoft 365 / Outlook* in the first-run wizard or in
Settings → Mail Accounts, and sign in in the browser. The account's name and
address are filled in from the sign-in, and its mail is read and sent through
Microsoft Graph. This also works where GNOME Online Accounts cannot add the
account, as with personal accounts on GNOME 46 (Ubuntu 24.04, Linux Mint 22).

Personal accounts (outlook.com, hotmail.com, live.com) approve Hylki
themselves the first time. A work or school account may need its
organization's administrator to approve Hylki once for everyone, at
`https://login.microsoftonline.com/organizations/adminconsent?client_id=01cdc012-c8d8-4d03-822c-76696a01c14e`;
Hylki shows that link when Microsoft refuses a sign-in for want of approval.

An organization that has approved another mail app's registration, and not
Hylki's, can sign in with that one instead. The **Advanced** row on the
account's page takes its **Client ID**, a **Tenant** (`common`,
`organizations`, `consumers`, or the organization's directory ID or
domain), the **Scopes** it was approved for and its **Redirect URI**. Empty
rows keep Hylki's own. Scopes are Graph's short names, space-separated,
such as `Mail.ReadWrite Mail.Send User.Read`; `offline_access` is added if
it is missing. Hylki reads and sends mail, so only the mail scopes are used,
whatever else the list grants. A redirect URI that is not a `localhost`
address, such as Evolution's
`https://login.microsoftonline.com/common/oauth2/nativeclient`, makes the
sign-in run in a window of Hylki's instead of the browser, so the answer
can be caught; that window keeps nothing once it closes. Evolution's
registration is `20460e5d-ce91-49af-a3a5-70b6be7486d1` with that redirect.

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

### LDAP directories

Settings → LDAP Directories holds the directories the composer looks
recipients up in. Once three characters of a name or address are typed in
To, Cc or Bcc, each directory that is switched on is asked for people whose
name, surname or address starts with them, and the answers join the
suggestions from GNOME Contacts and your mail. Nothing is copied from the
directory to your machine.

A directory is an address book in Evolution Data Server, the service GNOME
Contacts and Evolution keep theirs in. Hylki adds no LDAP client of its own,
so a directory set up in Evolution is listed here as well, and one added
here shows in Evolution. Removing one removes it for both.

- **Server** and **Port**: 389 for StartTLS or no encryption, 636 for TLS
  (LDAPS). The port follows the encryption unless you set another.
- **Search base**: where in the directory people are, such as
  `ou=people,dc=example,dc=com`. **Search** takes the whole tree under it or
  only its first level.
- **Sign in as**: the entry to bind as, such as
  `cn=jane,ou=people,dc=example,dc=com`, and its password. Leave both empty
  for a directory that can be searched anonymously. The password is kept in
  the keyring and handed to Evolution Data Server when the directory asks
  for it.

Saving checks the connection and says whether the directory answered, or
why not (a password it refused, a server it could not reach). The Flatpak
reaches the desktop's Evolution Data Server, which has to be installed on
the system, as it is with GNOME.

### Translating messages

Settings → Translation sets up a translation service you have an account
with, under your own key:

| Service | What it needs | Notes |
| --- | --- | --- |
| DeepL | an API key | A free key (ending in `:fx`) covers 500,000 characters a month. |
| Google Cloud Translation | an API key | The Basic (v2) API; the project needs billing turned on, even for the free allowance. |
| Microsoft Translator | a key, and the resource's region | Leave the region empty for a global resource. |
| LibreTranslate | the server's address, and a key if it asks for one | Can run on your own computer or network, so messages never leave it. |

**Check the settings** translates a greeting to show that the service
answers. **Translate into** is Hylki's own language unless another is chosen.

A message is translated from the A文 button in its card's actions, or from
Translate in its right-click menu. The card then shows the translation, with a banner naming the service and the language
it came from, and **Show Original** in the banner or the menu switches back. The
subject is translated with it, in the heading above the message. A
translation is kept for
the rest of the session, so opening the message again costs nothing more.

What is sent is the message's text, a paragraph or a cell at a time, with
the bold, italics and links inside it but none of the sender's styling,
images or link addresses. The translation is put back where the text was, so
the message keeps its design; a plain-text message is shown as Reader View
shows it. Long messages go in pieces, and one with over about 120,000
characters to send is not sent.
Messages that arrived encrypted are never sent.

With **Offer to translate** on, a message in a language other than the one
translations go into shows a Translate button above it. Which language a
message is in is worked out on this computer; nothing is sent until the
button is pressed.

In the composer, the A文 button beside the format chooser translates what
you are writing: the selection if there is one, otherwise everything you
wrote above the quote and signature, which are left as they are. It offers
the language of the message you are answering first, recognised offline,
then the one you used last, then every other. When everything is
translated, the subject is too, unless the message is a reply or a forward,
whose subject is already the conversation's. The translation replaces your
text as one edit, so Ctrl+Z gives back what you wrote. Until you change the
text again, the same menu offers **Show Original**, which puts back what you
wrote, subject included, and then **Show Translation**. It works in rich
text, plain text, Markdown and HTML. A message set to be encrypted is never
sent for translation.

The settings are in `translation.toml`; the key is in the keyring.

### Where the signature goes

In a reply or forward the account's signature is placed above the quoted
message, so it closes what you wrote rather than what the other person did.
**Settings → Composing → Signature in replies** moves it below the quoted
message instead, the placement Hylki had before 1.38. The setting applies
when a composer opens; a draft keeps its signature wherever it was saved.

The signature follows a blank line and nothing else. **Settings → Composing →
Separator line above the signature** puts the traditional `-- ` line (two
dashes and a space) between them, as Hylki did before 1.42. Thunderbird,
Evolution and Mutt use that line to show the signature dimmed and to leave it
out when they quote your message; Gmail, Apple Mail and Outlook ignore it.

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

### Drafts saved as you write

A message being written is saved to the Drafts folder on its own every 30
seconds while it changes, so a crash or a lost connection costs at most that
much. Nothing is saved before you have changed anything, so an untouched
reply leaves no draft. Each save replaces the one before, and **Save Draft**,
**Send** and **Send Later** replace or remove it as they would a draft you
saved. **Discard** on a new message removes the copy it left; on a draft you
had saved before, the last automatic save stays. A save that fails, offline
say, is tried again at the next one, without a message about it.

### The From address

The From row lists each account's address and the aliases set up for it.
Its pencil turns the row into text, where any name and address can be typed
for this one message, a throwaway address or a `+tag` one, without making
it an alias; the arrow beside it goes back to the list. The message still
goes through the account picked in the list, and with one address that row
appears under **More**. A reply to mail sent to a `+tag` address of one of
your addresses starts from that address, and a draft saved from a typed
address opens with it. Whether the server accepts mail from an address it
does not know is up to the server: many refuse it, or rewrite it.

A JMAP account lists the identities the server keeps for it as well, so an
alias made in the webmail is in the From row without setting it up again.
The message is sent as the identity with its address, or as a catch-all
identity for its domain (`*@example.org`) when there is one. The account's
page in Settings shows them under Send-as aliases, read-only: they are
changed where the server keeps them.

### Return and Shift+Return

In the rich text editor <kbd>Return</kbd> starts a new line in the same
paragraph, and <kbd>Shift+Return</kbd> ends the paragraph with a hard
return, which leaves a space before the next one. **Settings → Composing →
Return starts a new paragraph** swaps the two. In a list <kbd>Return</kbd>
still starts the next item, and pressed twice in a quote it still leaves the
quote. The setting applies when a composer opens. Markdown, HTML and plain
text are written as source, where <kbd>Return</kbd> is always a new line.

### The formatting toolbar

The rich text editor's toolbar has bold, italic, underline, strikethrough,
bulleted and numbered lists, quote, link and Clear formatting, all of which
survive a switch to Markdown. The chevron after them shows or hides the
rest of what a mail can carry:

- **Paragraph style:** Normal, Heading 1 to 3 and Preformatted. The menu's
  label names the style the cursor is in.
- **Font:** Sans Serif, Serif or Monospace. These are generic families, so
  the recipient's client picks its own font of each kind.
- **Text color and Highlight:** a palette each, plus Automatic and No
  highlight to take the color off again. Each button shows the color it last
  applied.
- **Decrease and Increase indent.** In a list, Increase indent nests the item
  under the one above it.
- **Insert emoji** (also <kbd>Ctrl+.</kbd>) and **Insert picture**, which puts
  pictures in the message at the cursor, as dropping or pasting them does.

Colors and fonts go out as inline styles, which every mail client keeps.
Markdown has no words for them, so switching such a message to Markdown or
plain text leaves them out. **Settings → Composing → Formatting toolbar**
says whether a new message starts with these tools shown (Always expanded)
or hidden behind the chevron (Always collapsed, the default); the chevron
changes it for the message at hand. When the composer is narrow, the
toolbar wraps onto a second row.

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

To have the result in words as well, switch on *Settings → Reading → Name the
OpenPGP result*. The icons then sit in a label that reads **Signed**,
**Encrypted**, **Encrypted and signed**, or what is wrong, such as **Bad
signature** or **Signed, unknown key**.

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

### Quoted text

When a reply ends with the message it answers, the reader folds that part
away behind a ••• button. It recognises the quotes Gmail, Outlook (web and
desktop), Apple Mail, Thunderbird, Yahoo and Hylki write, an "On … wrote:"
line before them, an "Original Message" divider, and the `>` lines of
plain-text mail. Once opened, the button sits where the quote begins and
folds it again. Nothing is folded when your reply comes after or between
quoted lines, so an answer written inline always shows in full.

### Folding messages in a conversation

A click on a message's header in a conversation folds it to a short card: a
line with the sender's circle and name, then a star, a paper plane or an
inbox for sent or received, a paperclip when it has files, and the day.
Under it comes the start of the message, in as many lines as **Settings →
Message List → Preview lines** asks for; the subject is the conversation's,
in the heading above. A click on the card opens it again; a double-click on
the header still opens the message in a window of its own.

**Settings → Conversations → Fold earlier messages** decides how a
conversation opens: **Never** shows every message, **Messages already read**
folds what you have read, and **All but the newest** folds every message
except the newest, read or not. The newest always opens, and so does a
message you move to with `w` or `b`. Right-clicking anywhere in a
conversation offers **Expand All Messages** and **Collapse All Messages**.
What you fold or open stays that way while the conversation is on screen.
Printing shows every message in full.

### Message list layout

**Settings → Message List → Layout** sets how the list shows a message.
**Cards** gives the sender, the subject and the start of the text lines of
their own. **Single line** puts them on one line in columns: the sender's
circle and name, the subject with the text dimmed after it, then tags, a
paperclip, the conversation's size and a short date. **Automatic** uses one
line while the list pane is dragged wider than about 600 pixels and cards
when it is narrower. On one line the actions palette is not shown; the
right-click menu, swipes and keyboard shortcuts do the same things.

**Settings → Message List → Columns** chooses what a single line shows and
in what order: drag a column between **Shown** and **Not shown**, or along
the row. The subject always stays, and the conversation's size rides at its
end. The columns are:

| Column | What it shows |
| --- | --- |
| Star | The star, lit when the message is starred |
| Sender | Who it is from; in Sent, who it went to |
| Recipients | Who it went to |
| Correspondents | Everyone who wrote in the conversation, you as "me" |
| Subject | The subject, with the start of the text dimmed after it |
| Tags | The message's tags |
| Attachment | A paperclip when there are files attached |
| Importance | A red mark for high importance, a grey arrow for low |
| Account | The account the message is in |
| Due Date | When a Microsoft 365 follow-up flag falls due, red once it has passed |
| Date | When it arrived |

Importance comes from the `X-Priority`, `Importance` and `Priority`
headers, or from Microsoft 365. Mail already downloaded shows as normal
until it is fetched again. Only Microsoft 365 has a due date, so Due Date
takes room only in a list that holds Microsoft 365 mail. **Restore
Defaults** goes back to star, sender, subject, tags, attachment and date.

**Column headings**, in the same group, names the columns above the list.
Clicking a heading sorts the list by that column, and clicking it again
reverses the order; the sorted heading is in the accent color, with an
arrow. Tags and Correspondents do not sort. The sort menu follows the
headings.

The name and date columns are resized by dragging the faint line at a
heading's edge: the right edge of a column left of the subject, the left
edge of one right of it. A double click on that line puts the column back
to its usual width, and **Restore Defaults** puts every column back. When
the list is too narrow for the widths set, the name columns give way in
proportion and the dates keep theirs.

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

### New and seen unread mail

A folder's unread count is in the accent color. With **Settings → Sidebar →
Highlight only new unread mail** on, it is only while mail has come into the
folder since you last looked at it; once you have opened the folder, the
count turns grey, however much is still unread, until more arrives. A folder
on screen counts as looked at, and so do the inboxes of All Inboxes while it
is open. Reading mail elsewhere lowers the mark with the count. What was
unread when the setting was turned on counts as seen. The marks are kept in
`state.toml`, and the setting is `seen_counts` in `privacy.toml`.

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

**Translation** is off until a service is chosen in Settings → Translation.
After that, a message's text goes to that service only when you press
Translate on it, and encrypted messages never do. See
[Translating messages](#translating-messages).
