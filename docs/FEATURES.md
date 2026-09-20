# Features

The full list. The [README](../README.md) carries a shorter one.

## Accounts

- **Multiple accounts** — IMAP and POP3, each on its own background worker,
  with a unified *Inboxes* view across all of them.
- **OAuth 2.0 sign-in** — Google, Microsoft and custom providers over
  XOAUTH2. See [Configuration](DOCUMENTATION.md#oauth-google--microsoft).
- **GNOME Online Accounts** — import an account already set up in GNOME
  Settings, including Microsoft 365 over Graph. Pausing an account's Mail
  service in GNOME Settings pauses it here.
- **Per-alias SMTP** — each address on an account can send through its own
  server and credentials.
- **Proton Bridge, Zimbra, iCloud, Mailfence and other awkward servers** —
  STARTTLS with locally signed certificates, servers without MOVE, and
  non-compliant fetch replies are all handled.

## Mail

- **Whole-mailbox sync and search** — no message-count cap. A fast first page
  loads instantly; the rest indexes in the background with infinite scroll.
- **Two-way sync** — deletions, moves and flag changes from your phone or
  another client sync back automatically (IMAP IDLE plus reconciliation).
- **Conversation threading**, with the count covering the whole conversation
  across folders, not just the folder you are looking at.
- **Full folder management** — create, rename, move and delete folders,
  assign the special roles (Drafts, Sent, Junk, Trash, Archive).
- **Filters** — multi-condition rules on sender, recipient, subject, body and
  Reply-To, with comma-separated alternatives; they tag mail, file it away, or
  both, and can be run over mail that is already in a folder.
- **Tags** — IMAP keywords, Graph categories, or a local fallback where the
  server has neither; custom colours, drag to reorder, and number-key
  shortcuts.
- **Unified and filtered folders** — Inboxes, Starred, Sent and Drafts across
  accounts, plus a folder per filter.
- **Empty Trash and Junk**, by hand or automatically after a number of days.
- **Attachment gallery** — every attachment in an account or folder in one
  place, with PDF first-page thumbnails, scoped by account and folder, and a
  deep archive scan that finds old attachments without downloading them.
- **Printing** — a message with its sender, recipients and date, with an
  in-app preview that also saves straight to PDF.
- **Outbox** — a send that fails is kept and retried when the connection
  returns, not lost. Queued messages can be edited, sent by hand or discarded.
- **Send later** — schedule a message for tomorrow morning, Monday, or any
  date and time; it waits in the Outbox, editable, until then.

## Writing

- **Four formats** — rich text, Markdown, hand-written HTML, or plain text,
  switched per message from the composer's format button. See
  [Writing in Markdown or HTML](DOCUMENTATION.md#writing-in-markdown-or-html).
- **HTML signatures**, per account, with a default sender for new mail.
- **Drafts** that can be saved without a recipient, reopened inline, and
  deleted from the composer.
- **Inline images** — dropped, pasted or picked from a file manager, resized
  by handle or menu, with an optional recompress on send.
- **Spell check** through the system dictionaries.
- **Undo and redo** across the whole message.
- **Quote folding** — a long quoted original folds away, interleaved replies
  intact.
- **Split reply** — answer one message of a conversation with the rest still
  on screen.
- **Cloud attachments** — upload a large file to your own Nextcloud,
  ownCloud, OpenCloud or Seafile server, or to OneDrive or Dropbox, and put a
  share link in the message, with an optional expiry and download password.
  See [Cloud attachments](DOCUMENTATION.md#cloud-attachments-nextcloud-onedrive-dropbox-seafile).
- **Send from Files** — a *Send with Hylki* entry in the GNOME Files
  right-click menu sends the selected files into a new message, a draft or a
  reply of your choosing, with an offer to upload big ones to cloud storage
  instead. See [Send with Hylki from GNOME Files](DOCUMENTATION.md#send-with-hylki-from-gnome-files).

## Reading

- **Privacy-first reading** — remote content blocked by default, per-sender
  allow and block lists, and a per-message light/dark content theme.
- **Reader View** — one toggle shows every message in the conversation as its
  content alone: the sender's layout tables, colours, fonts, hidden preview
  text and tracking pixels are stripped, and what is left is set in one
  uniform sheet that follows the app's theme.
- **Your own font and colours** for message bodies, overriding the sender's.
- **Meeting invitations** — what the meeting is, when it runs in your own
  clock and time zone, where it is and who organised it, with Accept, Maybe,
  Decline and Add to Calendar.
- **One-click unsubscribe** — a banner on list mail that leaves the list for
  you, by request or by email, without a browser where the list allows it.
- **OpenPGP** — read encrypted and signed mail, sign and encrypt what you
  send, and manage keys from Settings, through the GnuPG already on your
  computer. See [OpenPGP](DOCUMENTATION.md#openpgp-encrypted-and-signed-mail).
- **Sender identity** — DKIM, SPF and DMARC checked on arrival, with BIMI
  logos where a domain publishes one.
- **Message previews** — the first one to three lines of each message under
  its subject in the list, or off.
- **mid: links** — a link to another message opens it (RFC 2392).

## The app

- **GNOME-native** — adaptive three-pane layout, per-account colours and
  emoji, picture or Gravatar avatars, light and dark following the system.
- **Appearance themes** — five palettes (Rose, Forest, Tidal, Earth and
  Midnight), each with its own light and dark version, or the stock GNOME
  colours.
- **Focus Mode** — Ctrl+Shift+F folds the toolbars, sidebar and list down to
  what you are reading, and puts it back afterwards.
- **A customizable reader toolbar** — reorder its buttons, or drag them in and
  out, in Settings → Appearance.
- **Swipe actions** on message rows, with an adjustable sensitivity.
- **Single-key shortcuts** — Gmail-style `j`/`k`, `r`, `a`, `d` and friends,
  without a modifier. See [Keyboard shortcuts](KEYBOARD_SHORTCUTS.md).
- **Runs in the background** (optional) — closing the window keeps mail
  arriving; Hylki appears under *Background Apps* in the GNOME system menu,
  and can start at login without opening a window.
- **Tray icon** (optional) — a StatusNotifierItem with a count of the folders
  you choose.
- **Notifications** that open the message they are about.
- **GNOME Contacts** — names and photos from your address book, optional.
- **Your language** — the desktop's, or one you pick; a 12- or 24-hour clock
  following the desktop setting.
