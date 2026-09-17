# Changelog

## 1.33.2-beta.1 — 2026-09-17

Catch-up release: the beta channel is brought level with stable 1.33.1. No
changes of its own — see the 1.33.1 section below for what is in it.

## 1.33.1 — 2026-09-17

A MIME part header carrying a non-ASCII character in the wrong place
could kill an account's mail thread, after which that account never
synced again.

- **A non-ASCII part header no longer takes an account down** (#215,
  reported and fixed by
  [@typedev](https://github.com/typedev), PR #216). `mime_header` found
  its header by slicing the line at the name's byte length and comparing
  that slice as a `str`. Header blocks reach it through
  `String::from_utf8_lossy`, so a line such as
  `Content-Description: abcóde` can carry a multibyte character straddling
  byte 25, the length of `content-transfer-encoding`, and the slice
  panicked instead of not matching. The panic landed on `vireo-mail-N`,
  the thread that owns all of that account's IMAP work, so the account's
  wheel turned for good while the others carried on, and because the
  garbled- and missing-preview passes offer the same rows on every sync,
  it never recovered. The prefix is now compared as bytes; a byte-wise
  match against an ASCII name proves the offset is a character boundary,
  so the slices that follow are safe. Two regression tests reproduce the
  reported panic with the fix reverted. Every release since 1.27.0 was
  affected; 1.32.0 widened it by reading the transfer encoding from the
  part's own header block.

## 1.33.0 — 2026-09-17

Replies address a conversation's newest message, a conversation's row
stays selected while its messages are read, the reply editor can open
beneath the reader, and each message in a conversation lists its own
attachments. Conversations have a Settings page of their own, and the
attachment drawer's count is a button.

- **A reply answers the newest message** (#210, reported by
  [@p-mitana](https://github.com/p-mitana), with
  [@yioannides](https://github.com/yioannides)). An untargeted reply
  (toolbar, Ctrl+R, a row's hover palette or context menu) addressed the
  message the reading pane showed at the top, so with "Newest message
  first" off it answered the message that started the conversation. Every
  untargeted reply now goes through one step that picks the conversation's
  newest message from someone else, whatever the display order; send-as
  aliases count as your own addresses. The row palette and menu paths did
  not know about the conversation at all: the list now sends it with the
  action when the row stands for a collapsed thread. And the check for
  "the selected row is the conversation" compared against the first entry
  on screen, which after Sent members are merged in and re-sorted can be
  your own earlier reply; it now compares against the message the reader
  was opened on. Reply decisions log under `vireo::reply` at debug level.
- **The conversation's row stays selected** (#211, reported by
  [@p-mitana](https://github.com/p-mitana)). With expandable conversations
  off, clicking a reply's card asked the list to select a row that does not
  exist, so the list selected nothing and the conversation appeared to
  deselect itself. A reader key with no row of its own now falls back to
  the row of the conversation it belongs to, including messages pulled in
  from Sent, which belong to no row in the folder on screen at all.
- **The reply editor can open beneath the reader** (#212, reported by
  [@p-mitana](https://github.com/p-mitana)). Settings → Conversations gains
  **Reply editor**: above the messages (the default, and the placement so
  far), below them, or following the reading order (above with "Newest
  message first", below otherwise). The bottom slot is the end child of a
  second Paned nested under the reader, so the reader is never reparented
  and each slot has a divider of its own; beneath the reader the composer's
  header carries no window controls and the grab pill sits on the panel's
  top edge. The dragged height is remembered as the panel's own height
  whichever side it is on. Placing the editor between two cards is not
  possible: the cards are one HTML document in one web view.
- **Each message lists its own attachments** (#213, reported by
  [@p-mitana](https://github.com/p-mitana), with
  [@yioannides](https://github.com/yioannides)). The drawer beneath the
  reader gathers every file in a conversation, which left no way to tell
  which message a file came with. Each card now lists its attachments
  beneath its body: type icon, name, size and a save button; a click opens
  the file, images and PDFs in the lightbox. The rows carry names and sizes
  only and are patched into the live document when the files arrive, so
  the conversation is not re-rendered for them. A lone message keeps the
  drawer alone unless the drawer is turned off. The drawer's right-click
  menu gains **Show in Message**, which scrolls to the card the file
  belongs to and flashes its row. Settings → Conversations has switches
  for both, **Attachments on each message** and **Attachment drawer**,
  both on by default. The offline demo answers a message flagged as having
  attachments with two small files.
- **Conversations has its own Settings page.** The Conversations group
  had grown to eight settings at the top of Reading; it is now a page
  between Message List and Reading with a chat-bubbles icon, and the
  Settings window is 38px taller so the sidebar lists every page without
  scrolling.
- **The attachment count toggles the drawer.** The drawer opened and
  closed from its seam alone, which is invisible until hovered. The
  header's "N attachments" is now a flat button that expands or collapses
  the drawer, with a chevron pointing the way the next click goes; it
  stays in the header when collapsed, so the same target reopens it. It
  takes the same padding as the Save All… button beside it, and the two
  stand the same distance from the edge each faces; the header sits 2px
  closer to the drawer's top edge. `VIREO_SHOWCASE_DRAWER=N` presses the
  button N times for checking the path without injected input.
- **French updated** (PR #214 by
  [@frenchy82](https://github.com/frenchy82)). The two About strings from
  1.32.2 and the four undo strings from 1.32.1 are translated, so French
  covers everything up to 1.32.3. The template gained 13 strings for this
  release, which are untranslated in every language.

## 1.32.3 — 2026-09-16

The two strings the About page gained in 1.32.2 are translated in
Portuguese.

- **Portuguese updated** (PR #207 by [@somepaulo](https://github.com/somepaulo)).
  1.32.2 reworded the one-line description under the version chip and made
  the issue tracker's row translatable. Both are covered in `po/pt_PT.po`
  and `po/pt_BR.po`, so each is complete again at 1220 of 1220 with no
  fuzzy entries.

## 1.32.2 — 2026-09-16

Russian's fuzzy entries are resolved and Portuguese is complete in both
variants. The About page's links are tidied.

- **Russian updated** (PR #205 by [@iliasen](https://github.com/iliasen)).
  Every fuzzy entry in `po/ru.po` is resolved. A fuzzy entry carries a
  translation but `msgfmt` skips it, so roughly sixty-six strings were
  rendering in English despite having been translated; they now show in
  Russian. The 1.32.0 strings are translated too. Russian is 1201 of 1220.
  `"Indexing {n} messages…"` arrived as a three-form plural entry, which
  `msgfmt` rejects because the template declares it singular; the plural
  wording was kept as the single form.
- **Portuguese updated** (PR #206 by [@somepaulo](https://github.com/somepaulo)).
  `po/pt_PT.po` and `po/pt_BR.po` both cover the composing formats, the
  sent-copy rows, undo and redo, the sidebar folder verbs and the Apply Now
  strings, with fifty-nine corrections in each. Both were complete against
  the template as submitted.
- **About page links.** The description is shorter, the issue tracker's row
  says what it is for rather than naming GitHub, Discord is listed by name
  above the contact address, and the issue row is translatable — it was the
  one link label that had never been wrapped.
- Translation template refreshed: 1220 strings.

## 1.32.1 — 2026-09-16

Undo and redo reach the composer: Ctrl+Z now takes back what you were just
writing, a step at a time, along with the attachments beside it. The inline
reply no longer aborts the app as it slides in, and the French translation
is complete.

- **Undo and redo in the composer** (extends #200). Ctrl+Z did nothing while
  a message was being written. WebKitGTK never turns the key into an editing
  command: its key binding translator forwards the keystroke to a hidden
  GtkTextView and watches that widget's signals, and undo is a GtkTextView
  action rather than a signal, so nothing came back; its own table of
  bindings has no undo entry either, and `execCommand('undo')` is refused
  from script. The page's history is reachable only through the widget API,
  so the editor drives it itself. The composer keeps one history over both
  the things that change a message — the body's typing, formatting, pastes
  and dropped pictures, and its attachments — with Ctrl+Z, Ctrl+Shift+Z and
  Ctrl+Y caught above the editor so the order holds. The address and subject
  rows keep their own entry undo, which is the right one for a single line.
  Works inline and in a compose window, and in all four composing formats;
  the body's right-click menu leads with Undo and Redo.
- **A run of typing is cut into steps worth undoing.** WebKit folds a whole
  run of typing into one undo step, and folds the deletes that follow into
  that same step, so Backspace was nothing Ctrl+Z could take back on its own
  and a paragraph written over several minutes came back all at once. Its
  history can be stepped but not shaped from outside, and the one thing that
  closes an open typing command is the selection changing — so the document
  re-sets the selection to exactly where it already is, which moves no caret
  and touches no content. The run is closed when the kind of edit changes,
  typing to deleting or back, and after five seconds without one. An IME
  composition is left alone, since its own events flip between inserting and
  deleting while a character is being built.
- **The main menu's Undo and Redo follow the composer.** They name what they
  would take back, so a reply being written reads "Undo Typing" rather than
  "Undo Archive". They are not decided by keyboard focus: opening a menu is
  itself a focus change, and where focus lands on the way is GTK's business.
  An open inline composer with anything in its history owns them, since what
  it can take back is being written now; while it has nothing they go back
  to the mail history. The keys still follow focus, because Ctrl+Z in the
  body has to undo the body.
- **The inline reply no longer aborts the app as it opens.** Both places that
  settled a running slide held a `RefCell` borrow across the skip that ends
  it — `if let` keeps the temporary guard alive for its whole body, and
  skipping an animation emits `done` there and then, whose handler reaches
  for the same cell. It was a panic in a callback that cannot unwind, so the
  process aborted rather than recovered.
- **French translation complete** (PR #204 by
  [@frenchy82](https://github.com/frenchy82)): the composing formats, the
  sidebar folder verbs, undo and redo, per-message remote content, the sent
  copy rows and the portal link errors. Two newline escapes that had been
  doubled on the way through the editor are repaired, one of them live in
  1.32.0.

## 1.32.0 — 2026-09-16

Messages can be written in Markdown or HTML as well as rich or plain text,
undo and redo now cover most of what the list can do, copies of sent mail
can be filed in any folder, and a reply you send joins its conversation at
once. Links open again inside the Flatpak, and filter rules on recipients
match the way they read.

- **Composing formats.** A message can be written as rich text, Markdown,
  HTML source or plain text. The composer's plain-text button (#180) is now
  a format button at the right-hand end of the formatting row, showing the
  current format with the other three behind it; a narrow pane keeps the
  same chooser in the overflow menu. The two source formats get a preview
  button. Settings → Composing → **Write messages in** picks the default.
  Nothing is sent as source: Markdown is rendered on the way out and the
  source travels as the plain-text alternative, and hand-written HTML has
  a text part derived from it the same way. Both are sanitized before
  sending (scripts, event handlers and style sheets go; tables, inline
  styles and images stay), and styling rides on the tags because webmail
  strips `<style>` blocks. Source mode is a text area inside the same
  editor, so the signature swap, dirty flag, paste path and drop target
  keep working; the preview is a second page built the first time it is
  asked for. The README lists exactly which Markdown is understood. A
  draft reopens in the default format rather than the one it was written
  in.
- **Undo and redo** (#200, requested by
  [@EmmanuelP](https://github.com/EmmanuelP)). Ctrl+Z only ever knew about
  moves, and there was no redo. The history is now a stack of steps, so
  Ctrl+Z and Ctrl+Shift+Z cover moves (archive, delete, drag, Move To,
  whole conversations, Spam and Not Spam), read and unread, starring, tags
  including the 0 key that clears them, and making, renaming or dragging a
  folder. Automatic read marks are left out on purpose. Both keys are
  shown beside Undo and Redo in the main menu. Within ten seconds of a
  move the rows come back on the spot and the server catches up behind;
  after that the careful server-side path takes over. The status bar no
  longer narrates any of it, the reader does not blink over an undo, and a
  restored message stays selected once the server renumbers it.
- **Save a copy of sent mail in any folder** (#199, requested by
  [@EmmanuelP](https://github.com/EmmanuelP)). The account editor's Special
  Folders group gained **Save a copy of sent mail in**, listing every folder
  of the account, the Inbox included. It is a destination only: the folder
  keeps whatever role it had. Left at Disabled, the copy goes to the Sent
  folder as before. A switch beside it says the server keeps its own copy
  (Gmail does), in which case Vireo appends none, so nothing lands twice.
  A message waiting in the Outbox or scheduled with Send Later uses the
  setting current when it goes out, not when it was written. Filter rules
  leave your own mail alone in a folder that receives copies, so a rule on
  a subject or recipient does not file your reply away from the thread it
  answers.
- **A sent reply joins its conversation at once** (#199). Conversations are
  assembled from the cache across folders, and the copy of a message just
  sent was only indexed the next time the Sent folder was listed, so the
  reply stayed out of the thread until you visited Sent and came back. The
  copy's folder is now listed and cached right after the send, and the
  conversation on screen asks for the rest of the thread again, so the
  reply is drawn in on the spot. Both the direct send and the Outbox flush
  do this; Microsoft 365 accounts list Sent Items after a send.
- **Links open again inside the Flatpak** (#202, reported by
  [@7system7](https://github.com/7system7)). A link was handed to GIO's
  launcher, which inside the sandbox fires a request at the desktop portal
  and returns without waiting for the answer; on a host whose portal cannot
  launch the default handler directly (Fedora 44, xdg-desktop-portal 1.22)
  every click looked dead, with nothing logged. Links now go through a
  launcher of their own: outside Flatpak GIO leads and the portal is the
  fallback; inside, the portal request is made directly with its response
  watched, a failed quiet launch is retried with the portal's app chooser,
  and a failure on that too gets a dialog saying what happened. Every
  attempt is logged by scheme and host, never path or query. Fixed on the
  way: the attachment opener waited for the portal's answer under a
  translated signal name, so in French, Russian, Hungarian or Portuguese
  its chooser retry never fired.
- **Recipient filter rules match each recipient** (#201, reported by
  [@yioannides](https://github.com/yioannides)). A rule on To/Cc was held
  against both lists joined into one string, so "is x@y" never matched
  and "ends with @y" only saw the last recipient. Every matcher but
  "contains" now looks at each address, and each display name, on its own.
  An Apply Now run also counted only what it did, so mail already tagged
  on an earlier sync came back as "none of them matched". The run now
  counts matches as well, the dialog shows them as they come in, the
  report says when the matches were already tagged or filed, and the log
  names how many messages each rule matched in each folder.
- **Remote content per message.** A card's actions palette gained a button
  for it, and the card menu an entry, both going both ways and both
  appearing only on a message that has remote content. The choice is
  remembered per message for the session, ahead of the standing policy,
  and the banner's own Load now sticks across a repaint too. The reader
  header keeps its full spacing under a warning or find bar, and the
  reading pane now starts on the same line as the message list.
- **List previews.** A quoted-printable preview no longer reads as base64:
  the part's declared transfer encoding is read and believed, and an `=`
  anywhere but the end rules base64 out. A preview whose first kilobytes
  are preheader padding is read deeper to find what the message says, and
  a half escape cut off by the preview's byte limit is dropped rather than
  shown.
- **French translation** (PR #203 by
  [@frenchy82](https://github.com/frenchy82)): the filter-run strings and
  thirteen corrections.

## 1.31.1 — 2026-09-15

Filter rules can be given a name, and the rules can be run over mail that
is already in a folder instead of only meeting mail as it arrives.

- **A filter rule can carry a name** (#197, requested by
  [@yioannides](https://github.com/yioannides)). The filters list spelled
  every rule out by its conditions, which since #192 can run to several
  clauses on one line. The rule editor's first row is now **Name
  (optional)**, and a named rule is listed by that name with its
  conditions moved into the subtitle above the account and destination;
  an unnamed rule reads exactly as it did. The name is written to
  `filters.toml` only when it is set, so a version that predates it reads
  the file back unchanged, and it is what the log calls the rule when one
  tags or files a message.
- **Filters can be run over mail that is already there** (#198, reported
  by [@yioannides](https://github.com/yioannides)). Rules only ever met
  mail as it landed in the Inbox, so a rule written today never touched
  anything already sitting in a folder, and nothing said what a rule had
  done, which left a rule that matched nothing indistinguishable from one
  that never ran. Settings → Filters gained **Apply Now** beside Add
  Filter, which runs every rule over the mail already in each account's
  Inbox; a folder's right-click menu in the sidebar gained **Apply
  Filters** for the same over that one folder, which is the only way
  rules reach mail outside the Inbox. It is left off Drafts, Junk, Trash
  and Starred, where filing mail back out is not what a rule about
  arriving mail meant, and off accounts with no rules at all. The report
  always names how many messages the rules were held up against:
  "Filters tagged 9 of 23 messages", or that none of them matched.
- **A run can be watched, or sent to the background.** Apply Now spins
  with an "Applying…" label for as long as the run lasts, and the run
  puts up a dialog with a spinner and a live count: messages looked at,
  tagged and filed, and which folder of how many it is on. Its one button
  is **Run in Background** while the work is going and **Close** once it
  holds the report. Waiting on the dialog puts the report there and
  nowhere else; sending the run to the background puts it in the status
  bar when it finishes, as does a dialog that goes down with the Settings
  window.
- A folder counts as done in such a run once its load has been to the
  server and back, not when the cache answers — which it does instantly
  and synchronously, so the run would otherwise have ended before the
  dialog could draw, and mail never synced into the cache would have been
  missed. The worker emits a new `FolderSynced` event at the end of every
  folder load, after success and after an error alike. Both answers
  describe the same mail, so a folder's tally takes the larger "looked
  at" of the two rather than adding them, while tags and moves add up:
  neither happens twice for one message, since the second pass sees the
  tag, or the move already requested.
- **French is complete again** (PR #195 by
  [@frenchy82](https://github.com/frenchy82)): 1171 of 1171 strings, with
  the 1.31.0 themes, the multi-condition filter editor, the Mail Accounts
  columns and the GNOME Files rows all translated and 25 fuzzy entries
  resolved. **Russian** has every 1.31.0 string (PR #196 by
  [@iliasen](https://github.com/iliasen)), along with a batch of wrong
  fuzzy matches corrected; three Send Later labels that had lost their
  `{time}` placeholder to a fixed clock time were restored.

## 1.31.0 — 2026-09-15

Appearance themes, a Drafts count fix for manually assigned folders, a
pass over the Settings wording, and updated Portuguese translations.

- **Appearance themes.** Settings → Appearance gained a **Theme**
  gallery: **System** (the stock GNOME look, still the default) plus
  **Midnight**, **Tidal**, **Rose**, **Earth** and **Forest**. Each
  theme carries a light and a dark palette, so the Style setting
  (follow system, light, dark) keeps doing what it did and picks which
  of the two is on screen; a card shows both halves with a ring round
  the one currently showing. The palette is applied through
  libadwaita's named colours (`@define-color` in an app CSS provider
  loaded above the static stylesheet), so it reaches past the chrome:
  the reader and the composer ground their documents from the live
  theme and are told about a theme change the way they are told about
  a light/dark flip. The divider between the message list and the
  reading pane is painted from the palette too (it is a `GtkPaned`
  separator, which answers to no named colour). The choice is saved as
  `theme` in `privacy.toml`; an id that no longer resolves falls back
  to the stock look. The palettes are the theme library of
  [T3 Code](https://github.com/pingdotgg/t3code) (MIT), converted from
  OKLCH by `tools/gen-themes.py` into `src/theme_palettes.rs`, with the
  dark accents taken down from upstream's very light values and their
  foregrounds checked for contrast, and each dark sidebar divider on
  its palette's own hairline rather than upstream's brighter grey.
- **A Drafts folder assigned under Special Folders now shows its
  count.** The assignment was applied only in the app, after the
  worker had classified and counted the folder list, so the worker
  kept its detected kind for the folder (Custom, when the server
  neither flags nor names it as Drafts) and every count it produced
  for it was the unseen count: the listing's STATUS, the periodic
  sweep and the on-open search all asked for UNSEEN instead of ALL.
  Drafts are never unseen, so the chip always read zero and never
  showed, while a detected Drafts folder on another account counted
  its drafts. Seen on laposte.net, which has no IDLE either, so nothing
  ever corrected it. The assignment now lives in one helper the worker
  applies to every listing, IMAP and Microsoft Graph, before anything
  is counted and before the list is cached, so later re-counts see it
  too; folder ids and order are untouched.
- **Settings wording and layout.** Rows reworded panel by panel:
  General's "Show sender and subject" loses its subheading; Sidebar's
  Filters and Tags placement both read "Choose between appearing in a
  unified section, or a list above or below the accounts", and the icon
  rail's Inboxes row matches the Starred and Sent rows under it;
  Message List's "Sent mail uses your account circle" replaces "Your
  own mail shows your account circle", Swipe actions names the default
  sides, and Reverse swipe directions says only what reversing does;
  Date and Time offers to override the system format; System's Files
  group is "GNOME Files Integration" with "Default Send with Vireo
  behavior" and "Default large attachments behavior". "Actions Palette"
  is lower case unless it opens a sentence. The subheadings under Theme
  and App icon are gone (the pictures say what the setting does).
- **The ⋯-as-a-menu switch moved to Reading.** It sat under Message
  List, acting on the list rows, but was meant for the message cards:
  it is now "Message card actions palette as a menu" under "Message
  card actions palette", greyed out unless the palette is in its
  hidden-behind-a-toggle mode (the only one with a ⋯ to press). The
  card's script hands the click back to the app, which opens the same
  menu a right-click on the card shows. The cards keep their own
  "Message card actions palette timeout" instead of sharing the list's.
- **Mail Accounts list columns.** The provider mark and the source
  badge (GOA / Vireo) each keep one width down the list, with the
  widget centred in its slot, so the marks no longer start at a
  different x on every row. The account editor's accent row says what
  the colour fills. OpenPGP's Import, Generate and Fetch by address
  buttons are a stacked column at the end of their group's header, in
  the shape the Tags and Filters panels use.
- **Portuguese translations updated** (pt_PT and pt_BR, PR #194 by
  [@somepaulo](https://github.com/somepaulo)): complete again against
  the 1.30.1 template.
- The reader toolbar placement test predated the six-button cap per
  side from 1.28.1 and has failed since; it now expects the cap.

## 1.30.1 — 2026-09-14

Swipe actions can be tuned to the trackpad they are used on, and a
committed swipe now leaves the list instead of blinking out of it.

- **Trackpad swipe sensitivity** (Settings → Message List). libadwaita
  scales a touchpad's two-finger scroll against a fixed 400px of its own
  and never asks the widget how far a full swipe should be, so a message
  row had to be scrolled 240px sideways before the action armed — further
  than most trackpads travel in one go, and on some laptops it could not
  be reached at all. The new setting runs from 1 to 10 in half steps and
  defaults to 3.5, roughly "the row moves with your fingers". It is stored
  as `swipe_sensitivity` in `privacy.toml`, greys out when swipe actions
  are off, and applies to open message lists at once. The factor is
  multiplied into the row's `AdwSwipeable::distance` and divided back out
  of the tracker's progress, so a mouse or touchscreen drag still moves
  the row exactly as far as the pointer went whatever the setting says;
  only the trackpad path, which ignores `distance`, feels it.
- **A committed swipe flies out and the list closes over it.** The row
  carries on off the side it was dragged to over 200ms with the action
  strip filling behind it, while its Revealer closes the row's height over
  the same span, so the rows below slide up into the gap instead of
  jumping once it is gone. The action fires as the exit lands, hung off
  the animation rather than a timer, so a row removed mid-flight takes its
  pending action with it; a row that survives its action (no Archive
  folder configured, say) slides back in rather than sitting there
  collapsed and off-screen.
- Sends from glib timers and animation callbacks now go through the
  fallible input sender. relm4's `input` aborts the process when the
  component's runtime is gone — a committed swipe hits that every time,
  since the row is removed just as the exit lands, and the Actions
  Palette's auto-collapse timer could already hit it on any list rebuild
  while a palette was open.

## 1.30.0 — 2026-09-14

Filters can look at the message body and the Reply-To address, hold
several conditions, and take several alternatives per condition.

- **Filters can match the message body** (#191, requested by
  [@yioannides](https://github.com/yioannides)). *Where* gained
  **Message body**: a rule that files anything with "unsubscribe" in it
  into Newsletters is now one condition. The text is searched on the
  server as the Inbox syncs (IMAP `SEARCH BODY`, Microsoft Graph
  `$search`), one search per alternative over the mail just listed, so
  nothing is downloaded for it; the list preview counts too, which is
  all a POP3 account or a server that refuses the search has to go on.
  Body conditions always mean "contains", the only search a server
  offers, and the editor pins the matcher and says so.
- **Filters can match the Reply-To address** (#191). *Where* also gained
  **Reply-To address**: the Reply-To header, or the From address when
  the sender set none, so a rule on where replies go works for mail
  from a person and mail from a system alike.
- **A filter can hold several conditions** (#192, requested by
  [@yioannides](https://github.com/yioannides)). The filter editor is
  now a page of groups: the account; one titled group per condition
  (*Condition 1*, *Condition 2*…), each its own Where/Match/Text set
  with a remove button in its header; an **Add Condition** row; and
  *Then*, what a match does (Move to, Tag with, Count unread mail). A
  **Condition matching** group below Add Condition, shown once there are two,
  says whether all must match (the default) or any one may; the
  later conditions' Where rows read *And where* / *Or where* to match.
  The Filters list prints every condition of a rule.
- **A condition can name several alternatives** (#192). Commas separate
  them in the text to match: `invoice, receipt` matches either, with
  the whole matcher applied to each (`@a.org, @b.org` with *ends with*
  is two endings). A value with no comma is one alternative as before;
  a value that held a comma on purpose now reads as two.
- The filters file keeps the first condition where earlier versions
  read it, and only writes the new keys when a rule needs them, so a
  rule with one condition still loads in those versions unchanged.
- **The filter editor's "Move to" list is no longer empty after Settings
  reopens.** Since 1.27.0, reopening Settings after an account, filter,
  tag or sender changed built a fresh Accounts panel that was never told
  the accounts' folders, so "Move to" offered only *Leave in Inbox* and
  an existing rule's folder showed as that. The panel now gets the
  folders as the first one does.

## 1.29.3 — 2026-09-14

Files sent from GNOME Files ask where they should go, and big ones offer
to go to cloud storage instead.

- **Files from GNOME Files go where you say** (#188 follow-up). *Send
  with Vireo*, *Open With Vireo* and Files' own *Email…* entry used to
  open a new message with the files attached and nothing else on offer.
  Now a dialog asks what the files are for: a **new message**, a
  **draft** picked from a list of every account's drafts, or a **reply**
  to a message picked from a list (the one on screen first, then every
  Inbox and the other folders listed this run, with a search box that
  narrows by sender, subject or account). A draft or reply picked from
  the list has its body fetched first when the cache lacks it; a reply
  to the message being read splits the reading pane the way Reply does,
  any other opens in a window. Drafts folders never listed this run are
  synced before the draft list shows, so it does not miss a draft the
  cache has not seen. A `mailto:` that carries `attach=` (Files'
  *Email…*) gets the same dialog and keeps its recipient and subject
  for the new-message case.
- **Big files can go to cloud storage instead.** When the files together
  exceed the size limit (20 MB by default) and a cloud storage account
  is set up, a second dialog offers to attach them anyway or upload them
  and put download links in the message (the composer's upload dialog
  opens on them, terms and all). Without a cloud account they are
  attached as before.
- **Settings → System → GNOME Files** gained the defaults: what the
  files go into (ask, a new message, a draft, a reply), what happens
  over the limit (ask, attach, upload) and the limit in MB. Both dialogs
  carry an *Always do this* box that writes the same preference.
- Under the hood: the hand-off's "did the window come to the front"
  check now counts any Vireo window, so the dialog asking about the
  files does not trigger the "message ready" desktop alert.
- **Cloud storage behind Cloudflare.** A self-hosted Nextcloud, ownCloud,
  OpenCloud or Seafile reached through a Cloudflare domain or tunnel
  rejected any upload over 100 MB (the proxy's request limit). The
  account editor has a "Server is behind Cloudflare" switch: with it on,
  files over 90 MB go up in 90 MB pieces the server puts back together
  (the WebDAV `uploads` chunking endpoint with a final MOVE; Seafile's
  resumable upload with Content-Range on the same upload link). Smaller
  files and the other services are unchanged. Not tried against a live
  server behind Cloudflare. With the switch off, an upload over 90 MB
  that Cloudflare's proxy refuses (its own 413 page, or a 5xx carrying
  its headers) or that is cut off mid-way no longer reads as a bare
  "HTTP 502": the message says the file's size, that Cloudflare's 100 MB
  limit is the likely cause, and names the switch to turn on.
- **French** (PR #193 by [@frenchy82](https://github.com/frenchy82)): the
  1.29.2 strings (mailbox face, Gravatar, settings wording) translated;
  1128 of 1163 strings, the 35 left being this release's own (the Files
  hand-off dialogs, the GNOME Files settings rows, the Cloudflare switch).

## 1.29.2 — 2026-09-14

A mailbox's own face reaches the mail it sent, with a Settings switch to
have it the old way and a per-account Gravatar option, and the settings
window leaves an open account editor properly.

- **Your own messages wear your mailbox's face** (#189, reported by
  [@yioannides](https://github.com/yioannides)). Choosing a picture for
  a mailbox in Accounts changed the sidebar circle and nothing else:
  the conversation cards you wrote yourself, and your own rows in the
  message list, still carried the plain initials circle every other
  sender gets. Every address a user sends from — each account's own and
  its send-as aliases (#34) — now carries the picture or emoji its
  account was given (`avatar::OwnFace`, refreshed with the sidebar),
  and both views draw it. The reader embeds the picture in the card as
  a PNG scaled to twice its 26px circle (the stored copy is 256px
  square and a conversation would otherwise carry one full-size copy
  per card of yours); the list hands its avatar the texture the sidebar
  already holds. An account showing its initials is left out of the
  map, so nobody else's circle changes.
- **"Your own mail shows your account circle"** (Settings → Message
  List, on by default) chooses between that and the old way: off,
  a message you sent gets whatever circle anyone else's mail would —
  their contact photo, their Gravatar when that is on, else initials.
  The switch is about messages only; the sidebar circle and the
  account editor's preview always show the account's face.
- **"Use my Gravatar"** in the account editor's Appearance group (off
  by default). On, the address is looked up at gravatar.com and that
  picture leads everywhere the account's face is drawn — the sidebar,
  the list, the cards, the editor's own preview; an address with no
  Gravatar falls through to the picture, emoji or initials below it.
  One request per address per session, off the main thread, made when
  the accounts are read and again when an account is saved, the switch
  moves or the machine wakes; a lookup that failed is forgotten rather
  than cached, so it is retried rather than repeated. The lookup sends
  gravatar.com a hash of the address, which is why it is the account
  owner's choice; the Privacy switch still governs other people's mail.
  The editor looks the address up the moment the switch is flipped, so
  its preview answers before the account is saved.
- **Leaving a settings editor goes where you asked.** With an account
  editor open, choosing another category asked about saving, but Save
  and Discard then put the sidebar selection back on the row that was
  clicked — which the click had already selected, so the list emitted
  nothing and the window stayed in Mail Accounts with another category
  highlighted. Both answers now show the page outright.
- **An untouched editor no longer asks.** The account editor records
  what it opened with and compares before answering: an unchanged form
  closes itself and the window moves on. The keyring's passwords arrive
  after the editor opens, so the record is re-taken when they land on a
  form nobody has touched; the signature's own dirty flag is consulted
  before the verdict. Filter and tag editors keep no such record and
  still ask.
- The reader's document builder no longer asks GTK for its toplevels
  before GTK is initialised (it panicked rather than answered), so its
  fifteen conversation tests run without a display.
- Showcase hooks for captures: `VIREO_SHOWCASE_ACCOUNT=N` opens an
  account's editor, `VIREO_SHOWCASE_EDITOR_DIRTY=1` types into its
  Label field, `VIREO_SHOWCASE_SETTINGS_GO=<category>` picks a sidebar
  category the way a click does, `VIREO_SHOWCASE_DIALOG=save|discard|cancel`
  answers the prompt on screen.

## 1.29.1 — 2026-09-14

"Send with Vireo" from GNOME Files now attaches every selected file to
one message.

- **Multiple files from Files land in one message.** The desktop
  entry's `Exec` line used `%u`, the single-URL placeholder, so a
  launch with several files (the Files extension, "Open With Vireo"
  on a multiple selection) started one Vireo process per file. Each
  handed its file to the running instance on its own and the composer
  only ever saw one at a time. The entry now uses `%U`, so GIO passes
  the whole selection to one launch and they all land in the same
  message. Flatpak rewrites it the same way (`@@u %U @@`), and the
  launcher copy the app icon chooser writes mirrors that line; an
  existing copy is rewritten at the next start.

## 1.29.0 — 2026-09-14

The attachments gallery now reaches the whole archive and can be scoped
to accounts and folders, "Send with Vireo" from the GNOME Files
right-click menu, "Send by email" from a file manager works inside the
Flatpak, opening the app while it runs in the background no longer
leaves the pointer busy, and people on Gmail and other mailbox hosts no
longer wear their provider's logo.

- **Attachments gallery reaches the whole archive.** The gallery could
  only show attachments whose bytes had already been downloaded (the
  newest 25 attachment-carrying messages per folder sync plus whatever
  had been opened by hand): on a real mailbox, 45 files out of 17,028
  messages known to carry one. A new tier sits between the index and
  the bytes: a scan works back through each folder's undescribed
  attachment-carrying messages with IMAP `BODYSTRUCTURE`, 200 per
  FETCH, and records filename, type, size and section per part in
  `attachment_meta` (a few hundred bytes per message; two decades of
  archive cost single-digit megabytes). Bytes are still downloaded only
  when a file is opened, and opening one the gallery knows of but has
  never held fetches it and fills the thumbnail in place.
  `BODYSTRUCTURE` is asked for alone rather than beside `ENVELOPE`, so
  iCloud, whose non-compliance is in the envelope half, parses it. A
  batch that fails to parse is halved and retried down to the one
  message responsible, bounded by a per-request budget, and a dropped
  connection leaves the messages unscanned rather than recorded as
  empty (cache schema v15 re-queues messages written off that way
  before). Base64 sizes are decoded back so files do not read a third
  larger than they are. Scope, search, type filter and sort are now one
  SQL query against the cache (`cache::GalleryQuery`), served from the
  app's own cache handle rather than through the account workers; the
  UI shows 120 rows and loads the next page when the scroll comes
  within two rows of the end, with a spinner in the footer for that
  and for the scan. Every `ORDER BY` carries the same tie-break so a
  row cannot drift between pages; search words are LIKE-escaped.
  Schema v14 is additive, so `RENDER_VERSION` now gates the drop of
  `bodies`/`sender_checks` rather than `SCHEMA_VERSION`. Existing
  downloaded attachments are seeded into `attachment_meta` on upgrade.
  Microsoft 365 accounts (Graph) still show only what has been
  downloaded, since their attachments come out of the raw MIME. Demo
  mode opens an in-memory cache seeded with sample mail;
  `VIREO_DEMO_ATTACHMENTS=N` pads it, `VIREO_SHOWCASE_GALLERY_MORE=N`
  pages down. Nine unit tests cover `structure_attachments`.
- **Gallery search keeps the focus.** Typing in the gallery's search
  box lost the focus after the first letter: the toolbar was shown only
  while the loaded list was non-empty, each keystroke cleared the list
  to ask for a fresh page, and GTK moved the focus out of the widget
  that was disappearing. A reload now keeps what is on screen until the
  replacement page arrives, the toolbar and footer stay up whenever the
  view has been narrowed, and typing is debounced by 250ms.
  `VIREO_SHOWCASE_GALLERY_SEARCH=<text>` types into the box and reports
  whether the focus stayed.
- **Gallery scope: accounts and folders.** The footer carries an
  account dropdown ("All accounts", then one row per account; transient,
  hidden with a single account) and a folders button with two master
  switches, "Include Archive" and "Include other folders", above a
  per-account checklist of every folder the gallery can draw on.
  Inboxes answer to neither switch. Sent is listed and starts unticked;
  Drafts, Junk and Trash are not offered. A switch that is off outranks
  a tick without clearing it (the rows go insensitive), so turning it
  back on restores each folder to what was chosen. Only ticks that
  differ from their kind's default are written to `state.toml`. The
  scope is a view filter over the cache, not a change to what the
  background prefetch downloads. Also fixed on the way: `load_state()`
  fell back to `StateFile::default()` with no state file, handing every
  field its type's zero instead of its serde default (pane widths and
  the thumbnail size were landing on 0 and being clamped); it now
  deserializes an empty document. The demo backend returns sample
  gallery attachments; `VIREO_SHOWCASE_GALLERY` (+`_FOLDERS`,
  `_ACCOUNT`) captures it.
- **Send with Vireo from GNOME Files** (#188, requested by
  [@7system7](https://github.com/7system7)). A Nautilus extension,
  `data/nautilus/vireo-nautilus.py`, adds "Send with Vireo" to the
  right-click menu on selected files: they open in a new message,
  attached. Folders are skipped; files on a mounted share go by their
  mount path. It launches Vireo by desktop id (stable, then beta) with
  Files' own launch context so the window can come to the front, and
  falls back to the `vireo` command or `flatpak run`; its label and tip
  carry their own fr/hu/pt/ru strings. Files loads extensions on the
  host, so the copy bundled in the binary is installed from Settings →
  System → GNOME Files into `~/.local/share/nautilus-python/extensions/`
  (`src/nautilus_ext.rs`), with Install, Update and Remove, and a
  Restart button that asks Files to quit over D-Bus. The extension
  leaves a marker (the SHA-256 of the file Files loaded) next to itself
  when loaded, so the row reads "Installed and loaded by Files" once
  that marker carries this build's hash; until then it says so and
  names the `nautilus-python` package, a native install also checks for
  the loader's library, and a read-only field shows the install command
  for the host's distribution (from os-release through `/run/host`:
  dnf, apt, pacman, zypper, emerge, apk, xbps) with a copy button. The
  Fedora RPM recommends `nautilus-python`. The README has a guide,
  including a curl one-liner. New sandbox permissions:
  `--filesystem=xdg-data/nautilus-python/extensions:create` and
  `--talk-name=org.gnome.Nautilus`.
- **Send by email from a file manager attaches the files and brings the
  window up.** Nautilus's "Send by email" hands the chosen files over as
  `attach=` paths inside a `mailto:` URI. Inside the Flatpak the paths
  were not readable (file forwarding only exports arguments that are
  themselves files, and the sandbox could not see the home directory),
  so the composer opened empty. The manifest now grants read-only
  access to the home directory and removable drives
  (`--filesystem=home:ro`, `/run/media:ro`, `/media:ro`; the narrower
  read-write entries still win), and a file that still cannot be read
  is reported in the app's notification bar instead of disappearing.
  When the window does not get the focus after a hand-off (a stale or
  missing token), a desktop notification says the message is ready;
  clicking it raises the main window and the newest composer. The
  alert withdraws itself once any window of ours is active. The
  `mailto:` and file hand-offs log what they received and what they
  could read.
- **Opening the app while it runs in the background no longer leaves the
  pointer busy for 15 s** (#187, reported by
  [@yioannides](https://github.com/yioannides)). With "Run in
  Background" on, Vireo is already running when its icon is clicked,
  and the launch is handed to that instance over D-Bus. GNOME had given
  the new process an activation token for the window that would
  appear, but the hand-off never carried it: GTK 4 removes the token
  from the environment in a library constructor, before `main` runs,
  and keeps it for GApplication's own use. Without the token on the
  window, mutter's startup sequence ran to its 15 s timeout, the busy
  pointer stayed, and GNOME Shell, holding the app in its "starting"
  state, ignored every further click on the icon until then. The
  hand-off now goes through a throwaway `GtkApplication` registered as
  a remote instance, whose `Activate`/`Open` calls carry the stashed
  token. The `open` handler emits `activate` directly instead of
  calling `app.activate()`, which GApplication brackets with a
  `before_emit` carrying no token that wiped the one just installed.
  The window that comes up now completes the launch and can take the
  focus from whoever launched it.
- **Sender logos are for brands, not for people on Gmail.** An address
  at a mailbox host names a person, so mail from a friend on Gmail was
  wearing the Gmail mark (from the bundled map) and anyone at
  hotmail.com got the site's favicon. `logo.rs` keeps a `MAILBOX_HOSTS`
  list (the freemail and privacy providers, national portals and ISP
  domains) and refuses an address at one of them in every entry point,
  so the row keeps its coloured initials and no lookup is scheduled.
  The provider marks identifying an account in Settings are untouched.

## 1.28.2 — 2026-09-13

Sender logos from BIMI and a bundled set, wide mail that scrolls again
in a narrow pane, drafts that are neither read nor unread, an evenly
spaced Special Folders description, more room above the first account in
the sidebar, and French.

- **Sender logos, before the site's favicon.** Two sources now come
  ahead of the 32px favicon a page declares (`src/logo.rs`). BIMI: the
  SVG a sender publishes for mail clients, named by the DNS TXT record
  at `default._bimi.<domain>` (the sending host first, then the
  registrable domain), resolved through GLib and fetched from the
  sender's own site (https only, 64KB at most, SVG only); a confirmed
  no-record is remembered for a week, a resolver or network failure is
  not. A bundled set: `data/logos/`, about 220 sender domains mapped to
  marks from gilbarbara/logos, Simple Icons and the app's own service
  marks, built by `tools/fetch-logos.py` into an embedded `logos.toml`
  and a compiled `logos.gresource` (~316KB), shown with no request at
  all. Every SVG is re-framed to a 160px square (`square_svg`) and
  rasterised with `GdkTexture`, which also lets site discovery take the
  SVG icons a page or manifest declares. Precedence: a stored BIMI logo,
  a fresh BIMI lookup, a bundled mark, the stored or fetched favicon.
  Still behind Privacy → "Show sender logos", whose text now says what
  is and isn't fetched. `VIREO_LOGO_PROBE=<address>` logs which source
  answers for a sender.
- **Wide mail scrolls again in a narrow pane.** A message whose layout
  grows with the frame width (640px tables inside 100% cells) was
  widened once and then measured wider still, so the frame's own
  document stayed horizontally scrollable and WebKit latched the wheel
  to it — vertical scrolling stopped whenever the pane was narrower than
  the mail. The sizing script now widens repeatedly until the content
  stops growing (up to eight passes) and every frame document's root is
  `overflow:hidden`, so whatever is left over is clipped rather than
  scrollable and a frame can never capture the wheel. Printing keeps
  overflow visible.
- **Drafts are neither read nor unread.** A draft is a message being
  written, so the read/unread toggle is withheld wherever it appeared on
  a draft — the row's right-click menu and its "Mark All" form, the
  action palette, the multi-select menu and the bulk bar — when the list
  shows Drafts, and `set_read` refuses a read change on a draft so
  nothing reaches the server and the chip is never adjusted. The empty
  reading pane in Drafts now reads "No draft selected. Choose a draft
  from the list to edit it here." under a pencil.
- **Special Folders description.** Its subheading no longer renders with
  stretched word spacing. libadwaita 1.9 paints a preferences-group
  description fill-justified when the text is shorter than the label,
  though `GtkLabel::justify()` reports left; the account editor now
  left-justifies its wrapping labels. The wording reads "Automatically
  follows…".
- **More room in the sidebar.** The first account's header sits 10px
  below whatever ends the unified block above it (a unified row, Filters
  or Tags), so the two read as separate groups. Nothing changes when no
  unified block is shown.
- **French** is complete again for the 1.28.1 strings by
  [@frenchy82](https://github.com/frenchy82) (#186): 1096 of 1096,
  nothing fuzzy.

## 1.28.1 — 2026-09-13

A white flash between messages fixed, account circles and provider marks
in the accounts list, a wider toolbar editor with six buttons a side, a
tray count that names what it counts, the chosen language reaching the
Flatpak and the RPM, and Brazilian Portuguese.

- **No white strip between messages.** Switching messages in dark mode
  showed a white bar with rounded corners under the card header for a
  frame or two. Each body is a sandboxed `srcdoc` iframe; before its
  document arrives the frame holds the initial `about:blank` page, whose
  colour scheme is light inside a dark frame element, and WebKit paints
  a scheme-mismatched frame's canvas opaque white. The wrapper's ready
  loop took that blank page for a complete document, sized the frame to
  it (8px, the empty body's margins) and counted it toward `ready`. The
  sizing script and the ready loop now skip a frame whose document is
  `about:blank` (the real one is `about:srcdoc`), frames are
  `visibility:hidden` until sized (`.vireo-live`), and an unmeasured
  frame opens at `height:0px` rather than the browser's 150px default.
  Present since the card layout; not a 1.28.0 regression.
- **Accounts list rows** (Settings → Mail Accounts). The account's
  30px circle, as the sidebar draws it (picture, emoji or initials on
  its colour or the palette accent it would get; `worker::accent_for`
  is now crate-visible), sits left of the name; the provider mark moves
  to the right at 24px with a tooltip naming the provider
  (`accounts::provider_name`; a plain account names its server). The
  source badge reads "GOA" with the tooltip "Imported GNOME Online
  Account". Colours come from a per-list `CssProvider`
  (`.acct-list-color-N`).
- **Toolbar editor.** The Appearance page's content column is 640px
  (`preferences::widen_page` finds the page's `AdwClamp`), so a row of
  six chips fits. Each side holds at most six buttons
  (`config::TOOLBAR_SIDE_MAX`): `ReaderToolbar::place` refuses a
  seventh, the drop zones open no gap and take no drop when full
  (`zone_has_room`), a saved layout is trimmed on load, and the zone
  headings say "up to 6".
- **Tray count is the inboxes'.** The tray icon's dot, tooltip and menu
  counted and listed every counted folder (inbox plus counting filter
  destinations); they now use `inboxes_unread` and the inboxes' unread
  mail only, and the menu says so: a disabled heading "N unread in
  Inboxes" (or "No unread mail in Inboxes"), "View all N unread in
  Inboxes…". The sidebar badges and the Background Apps status keep the
  counted total.
- **Chosen language reaches the Flatpak** (#183). flatpak-builder moved
  `/app/share/locale` into a `.Locale` extension with `locale-subset`,
  of which Flatpak installs only the system's languages, so a language
  picked in Settings had no catalogue unless it was also the system's
  and fell back to English. `separate-locales: false` keeps the
  catalogues in the app. Reported by Paulo Fino.
- **RPM ships the translations.** The package script already compiled
  every catalogue into the payload; the spec now installs them under
  `/usr/share/locale` and lists them with `%find_lang`.
- **Translations.** Russian updated to 1.28.0 (PR #185, Ilya
  Semenkovich), with "About {app}" made translatable: it was a bare
  literal in the help menu, the About window's title and its main page.
  French complete against 1.28.0 (PR #184, frenchy82). Portuguese
  (Portugal) updated, and Brazilian Portuguese added and listed in
  `po/LINGUAS` (PR #182, Paulo Fino). The template was refreshed
  (1097 strings); this release's new strings (the GOA badge and provider
  tooltips, the "up to 6" hints, the tray headings, "About") are
  untranslated everywhere.
- **Harness.** `VIREO_SHOWCASE_SCROLL_WIDTH` overrides the settings
  capture's 720px width.

## 1.28.0 — 2026-09-12

An arrangeable reader toolbar, a slide-over sidebar that stays open,
notification clicks that land in Inboxes and open at once,
conversations that move as one and take in new replies while open, a
right-click menu on reader cards, plain-text composing and monospace
reading, the clock following the desktop, a language chooser, and
Hungarian, Russian and Portuguese.

- **Reader toolbar layout** (Settings → Appearance → Toolbar). The
  reading pane's header buttons are arrangeable in two groups, plus a
  "Not shown" zone. Default: left = Reply, Reply All, Forward, Star,
  Archive, Delete; right = Tags, Read/Unread, Spam, Move To, Find in
  Message, Print. Only the right group folds into the ⋯ overflow menu on
  a narrow pane (in its own order); the left group stays at every width,
  and the fold threshold is recomputed from the button count. Stored in
  `~/.config/vireo/toolbar.toml` (`left` / `right` key lists;
  `config::ReaderToolbar`, `App::relayout_reader_toolbar`). The editor
  (`ToolbarEditor` in preferences.rs, three `ChipFlow` drop zones: a
  widget that lays chips out wrapping and eases each to its slot) opens
  a gap the size of the dragged chip under the pointer and slides the
  others aside; every drop autosaves. Right-clicking the header's empty
  space offers "Customize Toolbar…", which opens that page. The section
  is a raised card on the Appearance page.
- **Sidebar peek stays open.** The narrow-window slide-over panel folded
  back one second after the pointer left the rail or the panel, and the
  panel's menu popover is its own surface, so opening it counted as
  leaving: the panel slid away under the menu. The leave timer is gone;
  a click outside the panel (the scrim), a swipe or a navigation closes
  it. The hover-expand preference's text says so.
- **Three more languages.** Hungarian (PR #169, Laszlo Lang, 953
  strings), Russian (PR #176, Ilya Semenkovich, 1040 strings — with the
  reader card header's "Double-click to open in a new window" and the ⋯
  toggle's "Actions" made translatable, which they were not) and
  Portuguese (Portugal) (PR #178, Paulo Fino, 1048 strings), each merged
  against the current template and listed in `po/LINGUAS`. French is
  complete again (PRs #172 and #175, frenchy82). The strings added late
  in this cycle (Send Later's clock labels, the Language row, the
  wizard's caption, the plain-text settings) are untranslated
  everywhere.
- **Language chooser** (#179). Settings → System has a Language row:
  System (the desktop's language, English where no translation exists),
  English, and every catalogue in `po/LINGUAS` by its own name
  (`preferences::language_choices`, `native_language_name`). The choice
  is kept in `~/.config/vireo/language` — its own small file, read
  before any TOML — and applied at startup through LANGUAGE, which
  gettext consults on every lookup ahead of the locale; under a bare C
  locale, where gettext ignores LANGUAGE, messages are set to C.UTF-8
  (`i18n::apply_language`). What LANGUAGE was before the app touched it
  is kept in `VIREO_LANGUAGE_ORIG` and restored first, so a restarted
  instance does not inherit a choice since undone. The welcome wizard's
  first page has the same drop-down under the tagline, with a caption
  saying a pick restarts Vireo: the choice is saved, the instance exits
  in place once the restart helper is up, and a one-shot
  `VIREO_WIZARD_AGAIN` flag has the returning instance open the wizard
  again whatever mode it runs in (init clears it). The wizard in dark
  mode: text on the yellow is always dark, and its cards and the
  drop-down are the theme's opaque popover surface rather than the
  translucent card colour that only tinted the yellow; the hero wordmark
  is 240px wide with 64px to the language row.
- **Clock follows the desktop** (#173). "Follow system" probed what the
  locale writes for one in the afternoon, so GNOME's own Time Format,
  which the locale knows nothing about, was ignored. The desktop's
  `org.gnome.desktop.interface clock-format` is asked first — through
  the settings portal, which works in the Flatpak sandbox and on the
  host (`desktop::setting`), or GSettings outside the sandbox — with the
  locale probe as the fallback, re-read every 30 s. Send Later's presets
  name their hour on the clock in use (`datefmt::clock_label`).
- **Plain-text messages in monospace** (#181). Settings → Reading:
  "Plain-text messages in monospace" and a font row, the desktop's
  monospace font (`monospace-font-name`, same portal) unless another is
  chosen. `ReaderStyle.plain_font` lands on the `.vireo-plain` wrapper
  every plain-text part is rendered in — and on a body that arrives with
  no markup at all — after the message font and one class more
  specific. Formatted messages are untouched.
- **Plain-text composing** (#180). The composer's header has a Plain
  text toggle (between Attach files and the OpenPGP buttons; in the ⋯
  menu when folded), and Settings → Composing a "Compose in plain text"
  switch that starts every message that way. Plain text hides the
  formatting toolbar (`RichEditor::set_formatting_visible`) and sends
  the message as text/plain only (the HTML part is dropped at send and
  at draft save). The text/plain alternative of every message is now a
  real rendering of the body (`window.__vireoBodyText`) rather than
  innerText: quoted blocks carry "> " on each line, nested quotes stack
  them, list items their dashes, links their address, preformatted text
  its spacing, block boundaries become line breaks.
- **Compose toolbar.** The fold threshold was the header bar's own
  natural width, which doubles the wider side to keep the (empty) title
  centred, so every button on the end row counted twice and the toolbar
  folded far too soon (880px asked for a row needing 641); it is now the
  sum of the bar's rows (`header_rows_width`). Folded, Save Draft stays,
  label and all, beside Cancel.
- **Notification clicks** (#170). A click on a new-mail notification
  opened the message's folder and, when that account's section was
  folded in the sidebar, highlighted nothing. Now: mail that landed in
  an inbox opens in the unified Inboxes whenever the sidebar has that
  row (`App::open_unified`, the old `UnifiedSelected` body); otherwise,
  or for mail a filter filed elsewhere, its folder opens and the sidebar
  unfolds the account (`Sidebar::reveal_account`, on every programmatic
  folder highlight, so "Go to Message" from the gallery gets it too).
  Underneath, four things were fixed:
  - The list builds its rows on an idle since the coalesced-rebuild
    speed-up, so a `SelectAndLoad` queued in the same pass as the
    folder's list found no rows and the notified message was never
    selected. It now waits for the queued rebuild (`pending_select`); a
    reply inside a conversation, which has no row of its own, selects
    its thread head (`thread_head_for`).
  - An inbox open in the unified view was skipped when its unread count
    moved, on the assumption that the worker's IDLE would deliver the
    new list; only push accounts IDLE, so a polled account's Inboxes
    slice waited a poll interval for mail its chip already counted
    (`sync_background_folder` no longer skips open folders).
  - The message took seconds to show: the account's worker serves one
    request at a time, and the body request sat behind the inbox list
    fetch opening Inboxes asked for, and behind the unread sweep that
    IDLE waking ran inline (an EXAMINE and a SEARCH per folder, 13
    folders at 165 ms a round trip). The notification handler asks for
    the body first; the IMAP worker moves reader loads (body, bodies,
    source, attachment downloads) ahead of queued list fetches
    (`reorder_reader_loads`, never past a move, a flag change or a
    reconnect); and the sweep keeps its place in `sweep_pending`, stops
    before the next folder whenever a request is waiting, and runs from
    the idle chain after the new mail's body prefetch (`sweep_due`)
    rather than inline.
  - After the message was read, the Inboxes chip came back for a
    moment: lists and counts the worker had fetched ahead of the queued
    STORE still reported it unread. A read/unread change now stays in
    `pending_seen` until the worker reports it stored (new
    `WorkerEvent::SeenSettled` from the IMAP, Graph and POP3 workers;
    entries expire after 20 s); meanwhile that folder's server-reported
    counts are ignored and a fetched list has the pending state overlaid.
- **Conversations move as one** (#171). With a conversation open in the
  reading pane, the Move To… picker starts with a "Whole conversation"
  switch showing the member count, on by default; picking a folder then
  moves every member the way a dragged multi-selection does
  (`drop_move`: grouped by source folder, undoable, members from
  another account reported). Move To… is also in the message list's
  right-click menu (row and multi-selection, between Mark as Spam and
  Archive; the list hands the click's point over in window coordinates
  and the app anchors the picker on the window), on the row's action
  palette, and on the reader card's action row (the page reports the
  button's place in CSS pixels with its width, so a zoomed page still
  lands it). A drag that starts on a conversation row carries every
  member, as its Delete does (`ThreadDragKeys`, published with the row
  keys; a multi-selection of several conversations expands each). The
  picker has 10px above its contents.
- **Reader cards.** A right-click anywhere on a card — its header, or its
  body frame, which never reports events to the page — opens that
  message's full menu, the list row's: reply, star, read, tags, spam,
  Move To…, archive, delete, contacts, source. WebKit's context-menu
  signal says what was hit but not where, so a capture-phase gesture on
  the webview records the pointer and the page is asked which card holds
  it (`elementFromPoint`, in CSS pixels); links, selected text, images
  and editable fields keep WebKit's own menus. Reply, Reply All and
  Forward from that menu open the pane's inline composer, like the
  card's own buttons. The card's action buttons are centred flex boxes:
  the read toggle's nested spans rode the button's text line and sat
  high.
- **A reply arriving for the open conversation** used to mark its row
  and nothing else. After each rebuild the list compares the selected
  head's conversation with the one it last handed the app
  (`emitted_thread`) and reports growth (`MessageListOutput::ThreadGrew`);
  the app merges the new members into the painted conversation,
  chronologically, with bodies from the cache and the rest requested
  (the Body handler repaints as they land). Nothing on screen moves: the
  render keeps the reader's recorded place, and when none is recorded
  yet the card at the top of the pane is pinned there
  (`MessageViewInput::HoldPlace`), so with newest first the new card
  slots in above it out of view and is marked read once scrolled up to,
  by the same visibility rule as any unread card. A card brought into
  view — the unread mark on open, the newest message — lands below the
  page's top gutter rather than flush at the viewport edge; a place the
  user scrolled to is still restored exactly. `VIREO_DEMO_ARRIVAL=<secs>`
  has the demo's mock worker deliver such a reply.

## 1.27.3 — 2026-09-11

The narrow-window sidebar peek is its own panel over an untouched rail.

- **Peek panel as a second sidebar.** The peek used to be the one sidebar
  widget flipped into the account split view's overlay mode: opening it
  collapsed the split, stood a snapshot in for the docked rail and rebuilt
  the rows expanded, so the rail's column was covered and redrawn by the
  panel's content as it slid over, and its menu and refresh appeared to
  jump. The rail is now never touched. A permanently collapsed
  `adw::OverlaySplitView` wraps the account split view; showing its sidebar
  slides a second `Sidebar` instance, in its own `ToolbarView` with the
  expanded layout's header (Refresh top-left, title, menu top-right), in
  from the window's left edge over the rail and the panes with libadwaita's
  scrim, shadow and swipe-to-close (edge-swipe-to-open is off so it cannot
  fight the message list's swipe actions). The rail keeps its rows, header
  and width throughout. The snapshot, ghost, restore-timer and rail-repaint
  machinery is gone, and with it the race that could dock the rail while
  the split view's spring was still settling and leave no sidebar and a
  dead toggle until the next press (seen on 1.17.1).
- **Two instances, one state.** Both sidebars receive the same contents,
  unread counts, busy state and folder rows (`sidebars_emit`); every
  navigation is pushed to both as a silent highlight change
  (`SidebarInput::MirrorSelection`), so the row picked in one is
  highlighted in the other. The second instance is a `mirror` and never
  picks an opening view on its own. List-box selection signals are muted
  while rows are selected programmatically: an input they queue is judged
  later against a selection that may have moved on, which made two
  instances oscillate through the app.
- **Hover peek across both panes.** With hover-expand on, the peek opens
  on the first pointer movement over the rail (not on entering it — GTK
  synthesises an enter when the rail reappears under a resting pointer as
  the panel slides away); leaving either the rail or the panel arms the
  one-second fold-back and entering either cancels it.

## 1.27.2 — 2026-09-11

Inbox and Archive use GNOME's own icons.

- **Inbox and Archive icons** are now GNOME Icon Library's
  `inbox-symbolic` and `shoe-box-symbolic` (icon-development-kit, CC0),
  copied verbatim into `data/icons/hicolor/scalable/actions/` as
  `mail-inbox-symbolic` and `mail-archive-symbolic` and re-bundled with
  `tools/gen-icon-gresource.sh`. They replace the hand-drawn tray and box
  everywhere those names are used: the sidebar, the list toolbar and bulk
  bar, the row action palette, the context menus and the reader's Archive
  button.
- **README screenshot** refreshed to show the new icons.

## 1.27.1 — 2026-09-11

French translation catch-up for the 1.26.0 strings.

- **French translation updated** (PR #160 by @frenchy82). `po/fr.po` now
  has 971 of 1054 strings translated (was 957, with 42 fuzzy and 55
  untranslated). The 14 strings 1.26.0 added are in: the tag number
  keys and "Your tags", the Tags page and Cloud Storage descriptions,
  "Nickname (optional)", "Send new messages from" and its note, the
  Backup page text and "Account of the current folder". Four existing
  strings are reworded ("Generate…" gets its ellipsis, "No keys from
  other people yet."). The 83 strings still in English are the ones
  1.27.0 added.

## 1.27.0 — 2026-09-11

The unified section grows into its own thing and the sidebar remembers
itself; the message list and the Settings window open in tens of
milliseconds; a Move To… button, "Not Spam", a tag finder, a picture as
the account avatar, list previews in the charset they were sent in, and
the Actions Palette as a menu. Everything from the 1.27.0 betas is in.

- **Move To… button** (#164, @peterweissdk). The reader toolbar has a
  folder button between Spam and Find: `ui::folder_picker` is a popover
  listing the account's folders in the sidebar's order and indentation
  (`sidebar::folder_depth`), with a search entry that filters them and
  files into the first match on Enter. It acts on the reader's target
  (`move_to_path`) or, with several rows selected, on the whole
  selection through `drop_move` (grouped by source folder, undoable,
  foreign accounts reported), listing the first selected message's
  account (`AppMsg::MoveToMenu` / `MoveSelectionTo`). The folded ⋯ menu
  has the same entry; `VIREO_SHOWCASE_MOVE=1` opens the picker in the
  demo.
- **Reply addresses the latest message** (#165, @yioannides). With only
  the list row selected over a conversation, the reply target is the
  thread's head (its oldest message). Reply, Reply All and Forward now go
  through `compose_target`: with "Newest first" on, the newest message
  from someone else (never the user's own reply; the newest of all when
  every message is theirs), and the head otherwise, matching what the
  pane shows at the top. A highlighted card is still addressed as
  itself; Archive, star, read and delete keep their thread-head rules.
- **Tag views re-read the server** (#166, @7system7). A tag view lists
  what the on-disk index holds, and the index only learned a folder's
  flags when that folder synced, so a tag set on another device stayed
  out of the view until its folder was opened here. Opening a tag view,
  and Refresh while one is open, send each account in scope
  `MailRequest::RefreshKeywords { keywords }` (`App::sync_tag_keywords`,
  an account scanned in the last 15 s is skipped): IMAP examines every
  folder and sets a `UID SEARCH KEYWORD` per configured tag against
  `Cache::uids_with_keyword` (`refresh_keywords`; `Cache::set_keyword`
  reports whether a row changed); Microsoft 365 re-lists every folder,
  whose listings carry the categories. `WorkerEvent::KeywordsSynced
  { paths }` re-reads the open tag view and re-syncs the open folder if
  it is among them. POP3 and the demo answer with nothing.
- **Send Later rows at regular weight** (#167, @yioannides). The rows
  under the Send button's caret were plain flat buttons, which GTK sets
  in bold; they carry the context-menu classes.
- **Actions Palette as a menu.** Settings → Reading → "Actions Palette as
  a menu" (`list_palette_menu`, off): a row's ⋯ opens the same menu a
  right-click shows, hung under the button
  (`MessageRowInput::ChevronClicked` → `MessageRowOutput::ActionsMenu
  { index, x, y }` in the list's coordinates, since a factory output
  cannot carry a widget → `show_context_menu`), instead of sliding the
  palette out; hover-to-open stands down while it is on.
- **A picture as the account avatar** (#162, @yioannides).
  `AccountConfig.avatar` holds the file name of a scaled copy under the
  data directory's `avatars/` (`config::avatars_dir`, `avatar_path`).
  The account editor imports a chosen image (`import_avatar_file`:
  EXIF orientation, the shorter side scaled to 256px, centre-cropped,
  saved as PNG); `config::save` prunes pictures no account refers to,
  leaving files under ten minutes old for an editor still open. The
  sidebar draws it in both disc sizes through
  `ui::initials::avatar_picture` — a `gtk::Image` at the disc's pixel
  size, since a `gtk::Picture`'s natural size is the texture's and the
  disc box would grow to it — clipped by the disc's rounded corners
  (`set_overflow(Hidden)`); textures are cached by path and modification
  time. Precedence: picture, emoji, initials. GOA reconciliation leaves
  it alone, like the colour and emoji.
- **Account editor: Appearance.** The group opens with a row holding the
  circle as the sidebar will draw it (`preview_disc`, 72px, its colour a
  stylesheet the editor rewrites through `preview_css`), redrawn as the
  colour button, name, label and email change (`refresh_preview`,
  initials from the label, else the name, else the email, as the sidebar
  derives them). "Account accent color" stands on its own; "Circle shows"
  is a linked toggle pair, initials or emoji against a picture
  (`picture_mode`), and only that side's row is shown (Emoji: Choose…
  and Use initials; Picture: Choose… and Remove) and saved
  (`saved_emoji` / `saved_avatar`); the other side's choice stays in the
  editor. Add Alias… is a regular 130px button at its group's end like
  the other panels' add buttons.
- **Reader toolbar.** Find in message moves to the right, left of Print,
  and greys out with no message open rather than hiding, so the toolbar
  never shifts.
- **Tag finder** (Settings → Tags → "Find Tags…"). Every account is
  asked for the keywords in use across all its folders
  (`MailRequest::FindKeywords` → `WorkerEvent::KeywordsFound`, fanned
  out and counted in `App::tag_scan`, with a two-minute safety net for
  an account that never answers). IMAP: EXAMINE per folder reads the
  mailbox's FLAGS line and a `UID SEARCH KEYWORD` per candidate gives
  the count (an unused keyword is dropped; `worker::is_system_keyword`
  leaves out `$Junk`, `$Forwarded`, `$MailFlagBit…` and the like).
  Microsoft 365: the mailbox's master categories with their names and
  preset colours (`graph_preset_color`), counts from the cache
  (`Cache::count_with_keyword`). POP3 has nothing to find; the demo
  answers with a fixed set. Findings merge by keyword across accounts,
  known tags are dropped, and each keyword gets a proposed name and
  colour (`Tag::name_for_keyword` / `color_for_keyword`: Thunderbird's
  `$label1`–`$label5` become Important, Work, Personal, To Do and Later
  in Thunderbird's colours; anything else reads as words and takes the
  next free palette colour). The report (`AccountsInput::TagFindings`,
  an `adw::MessageDialog` checklist) offers Import All or Import
  Selected; the button shows a spinner and "Searching…" meanwhile.
- **Not Spam** (#168). In Junk, the row menu, bulk menu and bar, the row
  palette, the reader's toolbar button, its folded ⋯ menu and the spam
  shortcut read "Not Spam": `MailRequest::MarkHam` / `MarkHamMany` set
  `$NotJunk`, clear `$Junk` (best-effort, as marking spam is) and move
  the messages back to the Inbox in one round (`mark_ham`); Microsoft
  365 moves them. `MessageListInput::SetInJunk` beside `SetRestorable`,
  `RowAction::NotSpam` / `BulkAction::NotSpam`, `RowInit.in_junk` for
  the palette; the unified views clear the state too. Icon
  `mail-mark-notjunk-symbolic` joins the bundled set; the demo's Junk
  folder holds two messages.
- **Sender logos at the size the site publishes** (`logo::discover`).
  Only the two root paths were tried, so most senders got the 16–48px
  favicon.ico. The domain's home page (first 512KB, browser-ish
  User-Agent) and its web manifest are read for `<link rel="icon">`,
  `apple-touch-icon` and manifest icons, ranked by claimed size with the
  root `apple-touch-icon.png` (180) and `favicon.ico` (32); SVG and mask
  icons are skipped. Decoding still downsizes to 160px for the cache.
- **Initials centred by their ink** (`ui::initials::InitialsPaintable`).
  The message list's avatars, the sidebar's account circles and the
  reader cards' circles are drawn from the ink extents of the laid-out
  text rather than a label's logical box: a lone letter and a pair both
  sit exactly in the middle. The list keeps libadwaita's fourteen avatar
  gradients and its name hash, so every sender keeps their colour; the
  sidebar's `glyph_picture` (sized to the circle, expanding nothing)
  replaces the label and its optical-nudge CSS; the reader embeds each
  circle as a PNG rendered at the screen's scale (`png_data_uri`, cached
  per initial and tint) with the per-address hue as before.
- **Compose folds in a narrow pane.** An inline composer (new message,
  reply, forward) in a narrow reader pushed the window's close button
  off the canvas. The composer's root is an `adw::BreakpointBin` (360px
  floor); the full header is measured on first map, and below that
  width everything but Cancel, Send and the fields chevron folds into a
  ⋯ menu (`ComposeInput::SetNarrow` / `OverflowMenu`, the OpenPGP
  toggles ticked when on). The compose header shares the reader
  toolbar's tighter spacing; the compose window opens 720px wide. Fixed
  alongside: `Compose::update_with_view` never called `update_view`, so
  every `#[watch]` in the composer (the Send/Schedule label, Delete
  Draft, the cloud button) held its init value.
- **Tags in a submenu.** The row context menu and the reader's folded
  ⋯ menu put the tag toggles behind a "Tags ›" row
  (`context_menu::MenuEntry::submenu`: the popover is a `gtk::Stack` of
  pages with a back row; a page taller than 420px scrolls). The
  palette's and the reader toolbar's tag menus stay flat.
- **Sidebar rail toggle without the smear.** The freeze-frame snapshot
  over a rebuild was a `ContentFit::Fill` picture, stretched by the
  200ms width animation of a rail toggle. It now keeps the width it was
  taken at (halign Start, clipped by the overlay) and fades out over the
  same 200ms during a toggle (`built_collapsed` tells a toggle from an
  in-place refresh); other rebuilds keep the 80ms lift.
- **Row context menu in the capture phase.** The list's secondary-button
  gesture runs in the capture phase and claims the sequence, so no
  widget inside a row can take the press first; a miss on the row band
  falls back to picking the row under the pointer.
- **Inboxes chip.** The unified Inboxes chip counts the inboxes alone
  (`App::inboxes_unread`); a filter destination has its own row and chip
  under Filters. The rule's "Count unread mail" switch keeps feeding the
  tray icon and the Background Apps status, and says so.
- **Settings.** Add Account… (no longer a pill at the foot of the list),
  Add Filter…, Add Tag… and Find Tags… are regular 130px buttons at
  their group's end, stacked where a group has two. The Tags description
  breaks before "Drag a tag to reorder".
- Showcase hooks: `VIREO_SHOWCASE_FIND_TAGS`, `VIREO_SHOWCASE_ROW_MENU` +
  `VIREO_SHOWCASE_MENU=main|<submenu>`.
- **Filters in the sidebar.** The unified "Filtered Folders" row is
  "Filters" (`row_title`, the heading-style section, and the Settings →
  Sidebar rows that name it). The per-rule "Show under All Inboxes"
  switch is gone: `FilterRule.show_in_unified` is removed (older
  `filters.toml` files still load; the key is ignored) and the unified
  Filters section lists every rule's destination
  (`unified_folder_refs`), switched on or off as a whole. Each account's
  own Filtered Folders section is gone too; instead a destination folder
  is marked in place in the account's hierarchy (`filter_icon` →
  `FolderGlyph`): a custom folder wears the filter-folder glyph in the
  account's colour (`filtered_folder_icon`, `acct-tint-{id}`), a main
  folder (Archive, Junk…) keeps its grey glyph with a 9px
  `filter-symbolic` mark on the icon's corner in the account's colour
  (`FolderGlyph::Marked`, `.filter-mark`; bottom-right in the rail, where
  the unread badge has the top). The unified Filters rows show the
  kind's glyph tinted. `co.hyprlab.Vireo-filter-symbolic` (GNOME's
  three-bar filter) joins the bundled icon set.
- **Tags under each account** sit between the essential folders and the
  "Folders (N)" list.
- **One folder context menu** (`folder_menu_items(id, &Folder,
  filtered)`) wherever a folder is listed — under its account, as a
  unified Filters row, or in a heading-style filtered section: Mark as
  Read, Refresh, "Edit Filter…" for a filter destination
  (`CtxAction::EditFilter { account_id, path }` opens Settings on the
  Filters page with that rule's editor), Rename/Delete for custom
  folders, Empty for Trash and Junk. The unified rows drop their extra
  "Account Settings…" item. Every tag row (unified, heading-style,
  per-account) takes a right-click: "Edit Tag…"
  (`attach_tag_context_menu`, `CtxAction::EditTag(keyword)`).
- **Filter and tag editors are pages.** `open_filter_page` /
  `open_tag_page` push an `adw::NavigationPage` (tags `filter` / `tag`)
  on the accounts panel's navigation view, like the account and cloud
  editors: a header with Save and the window's close button, Enter in a
  field saves (`push_form_page`, `form_save`). `AccountsOutput::EditorOpen`
  carries the settings page that owns the open editor
  (`Option<&'static str>`), so the leave-editor prompt says filter or
  tag and saves through the open page (`AccountsInput::SaveOpenPage`);
  `CloseEditor` pops any of the three. The colour chooser parents to the
  active window. The Filters and Tags list cards lose their pencil (and
  the filter cards their "Count unread" switch, which lives in the
  editor) for a chevron, like the account and cloud cards.
- **Unread chips per unified row** (Settings → Sidebar → Unified →
  "Unread counts", an expander with a switch each for Inboxes, Starred,
  Drafts, Archive and Filters): `UnifiedChips` in `privacy.toml`
  (`[unified_chips]`, all on; the old `unified_chip` still counts for the
  Inboxes row through `load_unified_chips`), read by the sidebar's
  `chip_shown(row)` at every chip site, rail dots included.
- **Chips never overflow.** `style_badge(label, max_chars)` is the one
  way to make an unread chip: ellipsized past five digits (four in the
  rail's corner badges), and the unified header titles ellipsize, so a
  wide chip shortens the title rather than pushing the chevron out.
- **Unified glyphs aligned.** Rows under Filters and Tags no longer take
  the `.unified-subrow` 2px pull-in that centres the 21px account pills
  (`build_unified_sub_row(..., pill)`), so their 16px icons and discs sit
  on the header's icon column; their label gives up the same 2px.
- **Renamed folder follows the selection.** `apply_folder_rename` left
  `selected` on the old path, so every auto-fetch asked the server for a
  mailbox that no longer existed ("Could not load Vreo" every minute
  after Vreo → Vireo). The selection (and any child path) moves with the
  rename, the sidebar row is reselected, and a `LoadMessages` is queued
  behind the `RenameFolder` on the worker so the cleared view refills.
- **"All Inboxes" is "Inboxes"**, in the sidebar, Settings, README and
  the metainfo feature list.
- **Archive row** in the unified section (`UnifiedKinds.archive`,
  `archive_expanded`, rail fold-up and unread-chip switches).
- **Tag views cached.** Opening a tag shows the cached list at once
  (`tag_view_cache`) and reads the index off the main thread
  (`AppMsg::TagViewLoaded`).
- **List previews in the right charset** (#159, @7system7). The preview
  line under a subject read the first part's bytes as UTF-8, so an
  `iso-8859-2` body showed a replacement character per accented letter
  while the reader was fine. The summary fetch now asks for
  `BODY.PEEK[1.MIME]` alongside the slice; `preview_from_part` works on
  bytes, transfer-decodes first and reads the text in the declared
  charset through mail-parser's table (`decode_text`), UTF-8 and ASCII
  directly. Parts inside a nested multipart use their own Content-Type
  (folded headers unfolded, `mime_header`). A slice cut mid-character
  drops the fragment; undeclared 8-bit text falls back to Windows-1252.
  A first part that is a file (photo mail) yields an empty preview and
  hands off to the `BODY[TEXT]` retry (`retry_missing_previews`, now
  shared), which tolerates a MIME preamble; cached rows still holding
  replacement characters are re-read up to 12 per folder load
  (`redecode_garbled_previews`).
- **Unified section.** Starred, Sent and Drafts rows join All Inboxes,
  each built by one builder (`UnifiedRow`, `build_unified_row`): the
  header opens the merged view, the caret (or a long-press) opens the
  accounts' own folders of that kind. The unified view merges a set of
  (account, folder) slices (`UnifiedView::{Kind, Filtered}`,
  `unified_slices`) rather than one folder per account, carried through
  load, refresh, mark-read, index and background-sync paths. Filtered
  Folders and Tags placed "In the unified section" are unified rows too;
  their headers open every rule's folder merged and every tagged message
  (`tag_view` keyword `None`). "Above" and "Below the accounts" keep the
  heading style. Sent wears no unread chip anywhere. The section heads
  the scrolling sidebar rather than the pinned area (it can stand taller
  than a short window), and its rows stack with no gap (`.unified-item`).
- **Per-account Filtered Folders and Tags** under each account's Folders
  heading, whatever the unified section shows: every rule's destination
  (`account_filtered_folders`) and every tag scoped to that account
  (`TagSelected { account }`; the cache's keyword query was already
  per-account). Section widgets are keyed by `Slot::{Unified,
  Account(id)}`.
- **Settings → Sidebar** is three groups. "Sidebar": "Accounts in the
  sidebar" (`show_accounts`, also the main menu's "Show Accounts" check
  item, a stateful `app.show-accounts` action, and Ctrl+Shift+A), chevron
  placement, Attachments/Contacts rows, hover-expand, "Remember the
  sidebar layout" (`remember_sidebar`; off starts every launch with every
  account and section folded, accounts folded as they arrive in
  `SetAccount`) and "Remember icon rail state" (`remember_rail`).
  "Unified": a switch per row (`unified_kinds`, `unified_tags`) plus the
  unread-count switch and the two placement combos. "Icon rail": unread
  dots (`rail_dots`, a `.rail-dots` style on the rail's containers) and
  "Fold up expanded items" (`RailFold { enabled, accounts, all_inboxes,
  starred, sent, drafts, filtered, tags }`), an expander row whose enable
  switch is the master. Everything defaults to on.
- **Sidebar layout persisted.** `sidebar.toml` gains `unified_expanded`,
  `filtered_expanded`, `tags_expanded`, `starred_expanded`,
  `sent_expanded`, `drafts_expanded`, `filtered_expanded_accounts` and
  `tags_expanded_accounts`, reported by `SidebarOutput::SectionsOpen` and
  the per-account toggles.
- **Icon rail.** The Filtered Folders and Tags headings sat 2px left of
  every other rail item (a padding rule meant for the full sidebar);
  fixed with a rail-scoped rule. "Fold up expanded items" is a view of
  the rail, not a change to what is saved: while collapsed, ticked items
  start folded; anything opened or folded in the rail — a long-press on a
  unified row, a click on an account avatar — lands in the rail's own
  `rail_open`/`rail_open_accounts` states, cleared whenever the sidebar
  changes width, so the full sidebar comes back exactly as it was left.
  The rail carries no chevron buttons any more; the tooltip says
  "Long-press to expand or collapse", and the long-press works in the
  full sidebar too, alongside the chevrons.
- **Tag rows** indent like the Filtered Folders rows (the same leaf
  expander slot) and use regular weight.
- **Message list speed.** Measured with a 15,000-message demo mailbox
  (`VIREO_DEMO_BULK`), a switch into a unified view took 300–600ms: 200
  row widgets built at 1.5ms each, the previous 200 destroyed first, two
  or three times per switch. Now: rebuild requests coalesce
  (`queue_rebuild`, one rebuild per main-loop pass ahead of GTK's
  layout); the first 20 rows build synchronously and the rest in idle
  chunks (`fill_rows`, `row_send` guards indices, `flush_rows` before
  structural edits); a page switch hands the pane a fresh list box and
  retires the old rows at idle (`discard_rows`, `wire_list`); the row's
  eleven action-palette buttons build on first open (`build_palette`,
  0.9ms per row from 1.5); "load more" appends when the existing rows are
  unchanged (`row_sigs`). An arriving folder list identical to the held
  one no longer re-threads or re-emits; a unified view seeds missing
  slices from the on-disk index when it opens. Warm switch ≈55ms, cold
  ≈40ms. `VIREO_SHOWCASE_UNIFIED=sent|starred|drafts|filtered|tags`
  drives the rows in the showcase; timings log at debug level.
- **Settings window speed.** Opening took about a second (reportedly
  several with eight or more accounts): every account's passwords were
  read from the keyring first, the icon gallery decoded the catalogue,
  the dictionary list read directories, the OpenPGP page ran gpg, the
  signature editor's WebKit view was created, and the stack measured
  every page. Passwords load when an account's editor opens, off the
  main thread (`AccountSecrets`, with a sync fallback at Save); the
  gallery (`app_icon::texture` now cached), dictionaries and gpg probe
  run after the first paint at low priority; the signature editor and
  the editor page mount on first use; the stacks are non-homogeneous and
  unshown pages join after the first paint; the window is built hidden
  1.5 s after startup and kept (`set_hide_on_close`), the accounts panel
  rebuilt on reopen only when its inputs changed (`accounts_seed`). First
  open ≈170ms, reopen ≈70ms. The settings and account section stacks
  switch without a crossfade.
- **Showcase hooks**: `VIREO_SHOWCASE_SETTINGS_REOPEN`, `VIREO_SHOWCASE_RAIL`,
  `VIREO_SHOWCASE_TOGGLE`; `FolderKind` derives `Hash`, `Message`
  derives `PartialEq`.

## 1.26.0 — 2026-09-10

A default sender for new messages and number keys for tags (#157), the
services' own marks throughout Settings, a Cloud Storage panel that works
like Mail Accounts, and a fuller settings backup.

- **Default sender for new messages** (#157, @7system7). Settings → Composing
  → "Send new messages from": the account of the current folder (as before)
  or any enabled account or alias (`config::compose_default_from`, an
  address). New Message, Compose-to from a contact, `mailto:` links and file
  hand-offs all open from it (`App::new_message_from`,
  `ComposePrefill::from_address`; `build_compose_init` prefers the reply
  address, then this, then the account's own). Replies are untouched. The
  row hides with a single identity; a default whose account is disabled
  falls back silently. The combo's value label gets 50px beyond
  libadwaita's ellipsized width (`widen_combo_value`).
- **Tags by number** (#157 follow-up). With single-key shortcuts on, `1`–`9`
  add or remove the first nine tags in Settings order and `0` takes every
  configured tag off the message (keypad digits too; `Shortcut::Tag`,
  `Shortcut::ClearTags`, which re-reads the message between removals).
  The tag list in Settings drags to reorder (`AccountsInput::MoveTag`), the
  first nine rows show their key, and the Ctrl+? reference adds a
  "Your tags" section.
- **Service marks.** `data/brands/` carries the official marks of the six
  cloud services and eleven mail providers (sources and a trademark notice
  in its README; the project README says they are not under the AGPL, per
  AGPLv3 §7(e)); `src/brand.rs` embeds 128px PNGs rendered from them and
  decodes at the size shown, cached. Vireo's own blue envelope stands for
  IMAP/POP3 and unknown servers, the yellow one for custom OAuth. Shown in
  the cloud Service picker, the Cloud Storage panel (a strip across from the
  heading, and each row), the cloud editor (a header mark following the
  picker), the mail Provider picker in Settings and the welcome wizard,
  the Mail Accounts list, the account editor header, and both GNOME Online
  Accounts import lists (`Provider.brand`, `brand_for_account`,
  `brand_for_goa`, `provider_factory`). The picker factories bind by name,
  since a combo row's selected-value slot has no list position.
- **Cloud Storage panel.** The Service picker lists Nextcloud, ownCloud and
  OpenCloud separately (`CloudAccount::product`; accounts from before ask
  their server's `status.php` once, `cloud::detect_product`). Rows are the
  Mail Accounts card: mark, name over details, an on/off switch
  (`CloudAccount::enabled`; off keeps the account but the composer no longer
  offers it, `cloud::load_enabled_accounts`) and a chevron; the row opens
  the editor. Remove moves into the editor header left of Save, shown only
  for an existing account, and asks first. The name row is "Nickname
  (optional)"; the description is reworded with the providers on their own
  paragraph; Link defaults sit further below Check Connection.
- **Account editor.** Remove moves into the header left of Save (it still
  asks first); the Remove group at the bottom of the form is gone. The
  Provider picker's manual entry is renamed "IMAP/POP3 Account" and sits
  first, in Settings and the wizard, and stays the default.
- **Settings window remembers its category** for the session
  (`App::last_settings_page`, `PrefOutput::PageShown`); the "opens to
  Accounts" preference decides only the first open, an explicit Accounts
  request still goes there, and bringing an open window forward no longer
  switches it to General.
- **Settings backup** carries the cloud storage accounts (sign-ins stay in
  the keyring) and every language's spell-checker word list, both optional
  sections so an older bundle imports without wiping them; words are merged
  on import, never removed. The Backup page and the import dialog say what
  a backup holds. The optional hand-written `oauth.toml` stays out (it
  holds a client secret).
- **Reader.** Find-in-message uses `loupe-with-arrow-symbolic` (from the
  GNOME icon development kit, kept under `data/icons`).
- **French** (PR #158, @frenchy82): the 1.25.2 cloud strings and fourteen
  corrections, merged against the refreshed template; the strings added in
  this release await translation.
- **Credits.** Yiannis Ioannides (@yioannides, PR #75) joins the About
  window's Thanks list, the README and the website; @p-mitana joins the
  About window.
- README: tagline without "clean".

## 1.25.2 — 2026-09-09

Cloud attachments grow to OneDrive, Dropbox and Seafile, with link terms
chosen per upload; recipient suggestions remember everyone you write to.

- **OneDrive, Dropbox and Seafile as cloud storage** (#144 follow-up).
  Settings → Cloud Storage starts with a Service choice: Nextcloud,
  ownCloud or OpenCloud as before, OneDrive, Dropbox, or Seafile.
  - *OneDrive* goes through GNOME Online Accounts: the editor lists the
    Microsoft 365 accounts GOA has (`goa::list_files_accounts`, the Files
    switch shown when off) and the account keeps only the GOA id
    (`goa_id`; no keyring entry, `CloudAccount::has_secret`). Tokens come
    from GOA's `GetAccessToken` at each use. Simple upload to 60 MB, then
    an upload session in 10 MiB chunks, rename on a taken name,
    `createLink` (anonymous view) with the expiry and password. What the
    plan allows on a link is probed at Check Connection or Save
    (`cloud::probe_link_terms`: a personal drive with a free-tier quota
    takes neither expiry nor password, a Microsoft 365 personal one both,
    a business drive an expiry but no password), kept on the account
    (`link_expiry`, `link_password`, `link_note` in `cloud.toml`), and the
    rows it rules out are greyed out with the reason in the editor and
    the upload dialog; the check result says what still works. A new
    OneDrive account starts with both off. Google Drive was built the same
    way and dropped: Fedora builds GOA without Google's Files feature, so
    its token carries no Drive scope.
  - *Dropbox* signs in through the browser (OAuth with PKCE against the
    app key typed in, or one the build carries via `oauth.toml`
    `[dropbox]` / `VIREO_DROPBOX_CLIENT_ID`; the listener sits on the
    fixed port 41597 because Dropbox matches redirect URIs exactly, and a
    new sign-in takes the port over from a stale one), keeps the refresh
    token in the keyring under `cloud:dropbox|<e-mail>`, uploads through
    `files/upload` or an upload session over 150 MB (autorename on a
    taken name, folders made on the way), and shares with
    `create_shared_link_with_settings`; a link password or expiry on a
    Basic plan is reported as such. The editor carries step-by-step app
    setup instructions.
  - *Seafile* signs in with the account password (turned into an API
    token by `api2/auth-token/`); an account with two-step verification
    enters the current code too, and the token the server returns is what
    the keyring keeps (`seafile_login_with_code`, `X-SEAFILE-OTP`).
    Uploads go into a library (found by name, made when missing) and a
    folder inside it through the upload-link endpoint as a streamed
    multipart body; `api/v2.1/share-links/` makes the link with the
    password and `expire_days`.
  - Old `cloud.toml` entries read as Nextcloud. **None of the three was
    tried against a live account.**
- **Per-upload link terms.** The composer's cloud upload dialog (shown
  for every upload now, the account row only with more than one account)
  carries the expiry in days, the password switch and a password field,
  seeded from the chosen account's settings and reset when the account
  changes; what is set applies to that upload alone. A typed password is
  used for every file of the upload, an empty field gets one generated
  per file. `ComposeInput::CloudUpload` carries the adjusted account copy
  and `link_password`; `cloud::upload_and_share` takes the fixed
  password.
- **Cloud account editor as a page.** Adding or editing a cloud account
  slides an editor page in over the Cloud Storage list, the way the mail
  Accounts editor does (an `AdwNavigationView` in `CloudAccounts`, a
  header with Save, the shared settings header hidden meanwhile and the
  leave-editor prompt covering it; `CloudAccountsOutput::EditorOpen`,
  `PrefInput::CloudEditorOpen`). The expiry and password rows form a
  "Link defaults" group that says the upload dialog starts from them and
  can change them per upload; "0 keeps the link indefinitely". The name
  row is titled "Account name, such as … (optional)" per service.
- **Recipient suggestions remember everyone you write to.** The worker
  recorded a sent message's recipients only when SMTP or Graph succeeded
  on the first try; a send through the Outbox, a scheduled one or a
  flush never recorded them. The app now records them at the moment Send
  is pressed, whatever route follows, hands them to every composer
  already open (`ComposeInput::AddSuggestions`), and logs a failed
  write. Your own account addresses are in the list too, flagged and
  sorted after everyone else (`Suggestion::own`), where before they were
  left out altogether.
- **Settings window** opens 32 px taller (772), so its sidebar needs no
  scrollbar.
- **App icon gallery** gains "Vireo envelope, blue subtle": the bird
  envelope with the bird in a darker blue, after the yellow one.
- **Sign-in success page** shows the bare app icon, without the rounded
  tile behind it.

## 1.25.1 — 2026-09-09

French translation catch-up and a message-list drawing fix.

- **French translation updated** (PR #156 by @frenchy82). `po/fr.po` now
  has all 949 strings translated (was 889, with 25 fuzzy and 35
  untranslated), covering Send Later, cloud attachments, Empty Trash and
  Empty Junk, drafts and the reply panel fields. The fuzzy matches left by
  the 1.25.0 template merge are resolved, and a few existing strings lost a
  stray trailing period or gained sentence case to match the source.
- **Thread node dots and the last reply's rail stub are whole again.** The
  swipe surface added in 1.23.0 clips the row to its own box, but a thread
  member's node dot and the last reply's rail stub reach a few pixels left
  of it to sit on the group's rail, so both came out cut in half. The clip
  now starts that reach further left.
- **Demo content.** The demo (`VIREO_DEMO`) ships three tags and one
  filter rule per account, filing into Newsletters, Invoices and Orders
  folders with their own sample mail, and the Q3 roadmap thread runs five
  messages deep, so screenshots show the Tags and Filtered Folders sections
  and an expanded thread without staging. The README screenshot is
  refreshed.

## 1.25.0 — 2026-09-09

Send Later, cloud attachments, and a round of composer and drafts work.

- **Send Later** (#145, requested by @7system7). A dropdown beside Send
  offers Send now, tomorrow morning (8:00), tomorrow afternoon (13:00),
  Monday morning (8:00), or a date and time from a calendar and hour/minute
  picker. A scheduled message is built at once (attachments included) and
  parked in the Outbox with its time, where it reads "Scheduled for …" and
  can be edited (the editor shows the time, with "Send now instead"), sent
  now, or deleted. The app checks every half minute and flushes what is due
  by id, so the ordinary Outbox flush leaves scheduled mail alone; a message
  whose time passed while Vireo was closed goes at the next launch. IMAP and
  Graph accounts alike. `OutgoingMessage.send_at`; the `outbox` table gains
  `send_at` (added in place on existing databases). The Send button and the
  dropdown join as one accent control with an inset divider.
- **Cloud attachments** (#144, requested by @7system7). Settings → Cloud
  Storage holds Nextcloud, ownCloud and OpenCloud accounts (URL, user, app
  password in the keyring, upload folder, link expiry in days, optional
  download password) with a connection check. With an account set up, the
  composer's header has an upload button beside Attach: files go up over
  WebDAV into the account's folder (a taken name gets the time appended),
  each is shared by public link through the OCS files-sharing API with the
  account's expiry and password, and a line with the link, the size and
  those terms lands in the body above the signature and any quoted
  original. Links show as "(link)" chips beside the attachments; removing
  one removes the line. Download passwords stay out of the message and
  show in a bar with Copy. `src/cloud.rs`, `src/ui/cloud_accounts.rs`,
  `cloud.toml`.
- **Empty Trash and Empty Junk** (#152, requested by @yioannides) in the
  sidebar's folder menu, after a confirmation: IMAP searches the folder and
  expunges every uid, Graph lists it and deletes each message; the list and
  chip clear at once. `MailRequest::EmptyFolder`.
- **Reply panel fields** (#154, requested by @yioannides). The inline
  reply's header has a chevron that unfolds its From, To and Subject rows in
  place; the Composing preference "Show From, To and Subject in the reply
  panel" opens every reply with them showing.
- **Lone messages as inset cards** (#153, @yioannides). The card view has
  been the default since 1.18.x, but only for new installs: every settings
  save writes every key, so older installs stayed full-bleed without
  choosing it. The default is applied once (`single_card_default_applied`
  in privacy.toml); the Reading preference still turns it off.
- **Quote folding keeps interleaved replies** (#150, reported by
  @EmmanuelP). The ••• fold hid everything from the first quote to the end
  of the body, which took a reply written below a quote with it. A quote is
  now folded only when what follows the quote run is nothing, a signature
  (`--`/`__` delimiter line or a signature-class element) or a
  mailing-list footer; anything else means an interleaved reply and the
  message shows in full. Outlook's reply header and Vireo's own attribution
  line still fold everything after them. `tools/test-quote-fold.py` runs
  the reader script in a real WebKitGTK view over a dozen body shapes.
- **Quote, bulleted and numbered list buttons are toggles** (#137
  follow-up, @EmmanuelP). Clicking Quote inside a quote steps the paragraph
  out one level, the same move as Enter twice; the three buttons show the
  pressed look while the caret sits in their block, from a `vireoFormat`
  script message on every selection change.
- **Drafts.** A draft with no recipient yet can be saved (it failed with
  lettre's "missing destination address": drafts now build through
  `build_draft`, which gives a recipient-less message an explicit envelope
  that never reaches the bytes). Selecting a draft opens it in the reading
  pane's composer (double-click or Enter still open a window). The composer
  has a Delete Draft button while editing a draft, which moves it to Trash
  with undo. The Drafts chip counts every draft (IMAP `STATUS (UNSEEN
  MESSAGES)`, `SEARCH ALL` on recount, Graph `totalItemCount`).
- **French translation** (#151, @frenchy82): the last strings from 1.24,
  plus accent and wording fixes. New strings from this release are open.
- Showcase hooks: `VIREO_SHOWCASE_FOLDER=drafts|sent|archive|junk|trash`
  switches the demo to that folder; `VIREO_SHOWCASE_SETTINGS=<page id>`
  opens any Settings category.

## 1.24.3 — 2026-09-08

French translation catch-up.

- **French translation updated** (PR #149 by @frenchy82). `po/fr.po` now
  has 884 of 889 strings translated (was 708, with 50 fuzzy and 131
  untranslated), covering the OpenPGP pages and banners, tags, automatic
  Junk/Trash emptying, the reader font and colour overrides, swipe actions
  and the two-pane settings window. Landed with mechanical fixes: 51
  translations had a stray leading space, the `{signer}` placeholder was
  missing from the encrypted-and-signed banner, two wrong fuzzy matches
  (`Senders`, `{n} more address`) were cleared, and two typos corrected.
  Still untranslated: `Senders`, `{n} more address`, `Custom colour…`,
  `Tag Colour` and the export-log description.

## 1.24.2 — 2026-09-08

Any colour for a tag, and a composer that follows the theme.

- **Custom tag colours** (#147, requested by @yioannides). The tag dialog
  has a ninth disc after the eight palette colours: a hue wheel until a
  colour is picked, then that colour. Pressing it opens the GTK colour
  chooser (no alpha); the pick is stored as `#rrggbb` in `tags.toml` like
  a palette colour, so chips, sidebar dots and tints need nothing new. A
  tag whose colour is not in the palette opens on that disc. The discs lay
  out as two rows of five and four.
- **The composer takes its grounds from the theme** (#148, reported by
  @yioannides). The composer pane's background was the stock GNOME page
  shade (`#141414`/`#f1f1f1`) and the editor painted WebKit's `Canvas`,
  whatever the theme said, so under a custom GTK theme the composer stood
  out from the reader. The reader's theme-ground resolution (issue #62) is
  now a shared `theme_grounds_for()`; the scheme CSS takes the page ground
  from it and the editor document the view ground. A live light/dark flip
  re-resolves both: the CSS lookup runs on the next main-loop pass (named
  colours are re-resolved after the dark-notify signal, not before it, so
  an immediate lookup answered for the scheme just left), and the open
  editor document is re-grounded through a style-manager handler the
  editor disconnects when it goes.
- Showcase hooks for captures: `VIREO_SHOWCASE_REPLY` opens the inline
  reply composer, `VIREO_SHOWCASE_FLIP=dark|light` flips the app theme at
  6 s, `VIREO_SHOWCASE_EDIT_TAG=<index>` opens a tag's editor (past the
  end: Add Tag).

## 1.24.1 — 2026-09-08

Three preview lines are three lines again, and a swipe slides flush.

- **Multi-line previews clipped since 1.23.0.** The swipe surface wrapped
  around every row (#135) never declared a size-request mode, so GTK took
  it for constant-size and measured the row's height without a width; the
  wrapping preview label answered for the wrong width, and "3 lines"
  rendered as one full line and a clipped second. The surface now forwards
  its content's height-for-width mode.
- **Swipe to archive or delete slides the bare content over a full-width
  strip.** While a row is being swiped its hover and selection pill is
  suspended (the tint fades out, the rounding drops) so the content meets
  the coloured strip flush, and the strip runs the list edge to edge: the
  6px side inset moved from the list row onto the pill, so nothing in the
  geometry changes and the list never shifts as a drag starts or settles.
  A `swiping` row class is held from the first drag until the snap-back
  animation lands.

## 1.24.0 — 2026-09-08

The reader's own fonts and colours over the senders'.

- **Use my own font, Use my own colours** (#56, requested by @yioannides).
  Two switches in Settings → Preferences → Reading. The first sets every
  message in one font and size, whatever the sender chose: the interface
  font, or any picked with the font button under the switch. Headings keep
  their relative size and `pre`/`code` stay monospaced. The second ignores
  the sender's text and background colours, so each message reads as plain
  text on the reader's own ground, links in the accent colour; pictures
  are kept. Both reach the printed page too. Every message card gains a
  toggle on its action line that shows that one message as its sender
  formatted it, and back, for the session. The reader's stylesheet is laid
  over the message last and at id weight, so it outranks a sender's
  `!important` rules short of inline ones; the message itself is
  untouched, so View source, replies and forwards carry the original.
  Stored in `privacy.toml` as `override_fonts`, `reader_font` (a Pango
  description) and `override_colors`.
- **Empty Junk and Trash automatically** (#140, requested by @typedev).
  Two per-account choices in the account editor's Syncing group: never,
  or after 7, 14 or 30 days. At each sync (a few times a day at most) the
  worker deletes for good whatever in that folder is older than the age,
  counted from the day the message reached the server, the way Thunderbird
  and Apple Mail count. IMAP asks the server with `SEARCH BEFORE` and
  expunges; Microsoft 365 filters the well-known folder on
  `receivedDateTime` and deletes each message. A manual Special Folders
  assignment names the folder to sweep. POP3 accounts have no server
  folders, so the choice does nothing there. Failures are logged and
  retried at the next sync. Stored per account as `empty_junk_days` and
  `empty_trash_days`.
- **Settings in two panes** (#141, requested by @typedev). The settings
  window is a sidebar of categories beside the chosen category's groups,
  in place of the two long scrolling tabs. Under Accounts: Mail Accounts,
  Tags, Filters, Senders (the allowed and blocked lists); under Settings:
  General (with notifications), Appearance, Sidebar, Message List, Reading
  (with conversations), Composing (with spelling), Privacy, Date and Time,
  System, Backup. The Filters and Senders pages carry a search box that
  narrows their lists as you type, so a long allow list or rule set is no
  longer a scroll. The window opens wider to make room; under 640sp the
  panes collapse to one, the sidebar first. The account editor still opens
  over the content pane with its own back and Save header.
- **OpenPGP, first slice: reading** (#133, requested by @greedykangaroo01).
  Vireo now decrypts and verifies incoming OpenPGP mail through the user's
  own GnuPG: `gpg` on the path, the keyring in `~/.gnupg`, the agent and its
  pinentry for passphrases. PGP/MIME (`multipart/encrypted`,
  `multipart/signed`) and the inline forms (an armoured block or a
  clear-signed block in the text) are recognised. A message card shows a
  lock (encrypted) and/or a shield (signed) beside the sender, green when
  the signature checks out against a key the keyring trusts, amber for a
  doubt (unknown or untrusted key, expired), red for a failure (bad
  signature, revoked key, undecryptable); clicking it opens the verdict
  with the details, above the sender check. Nothing decrypted is written
  to disk: an encrypted message is decrypted for the reader at each open
  (the agent remembers the passphrase), its body and attachments are never
  cached, and the background prefetch leaves encrypted mail alone so no
  passphrase prompt appears on its own. Signature verdicts are cached with
  the sender check. The Flatpak gains access to `~/.gnupg` and the agent
  socket.
- **OpenPGP, second slice: keys and sending, without a terminal** (#133).
  An OpenPGP page in the settings sidebar lists your own keys and other
  people's: generate a key for one of your addresses (a signing key with
  an encryption subkey; the passphrase goes to gpg down a pipe and is not
  kept), import a key file, export a public key, fetch a key by address
  (WKD, then the keyservers), trust a key (a local signature with your own,
  the fingerprint shown to check first) and remove one. In the reader, the
  verdict popover offers "Fetch the sender's key" when the signing key is
  missing (the message's own Autocrypt key first) and "Trust this key…"
  when it is unvouched; either re-fetches the message so the chip follows
  the keyring. An attached public key gets an "Import OpenPGP key" action in
  the attachment drawer. Each account can name the key it signs with
  (Automatic picks by address). The composer gains Sign and Encrypt
  toggles: Sign sends PGP/MIME `multipart/signed` (the signed bytes are the
  entity lettre puts on the wire; `micalg` from gpg's own digest), Encrypt
  sends `multipart/encrypted` to every recipient's key and your own, signed
  inside; a missing key of yours or theirs is named before anything leaves.
  Drafts are kept as written; signing and encrypting happen at send. A reply
  to an encrypted message starts with Encrypt on. The README carries a
  setup guide.
- **Settings window details.** The window opens at a fixed 740px and no
  longer remembers a resize; the app menu entry reads "Settings"; the
  account editor keeps the window's close button in its own header, and
  choosing another category while an editor is open asks to save, discard
  or stay. The General category wears the puzzle-piece icon.

## 1.23.1 — 2026-09-07

Every move and delete on iCloud failed since 1.22.0; the fix, and the
server conversation in the log so the next such report explains itself.

- **iCloud: "Could not move … Parse Error" on every delete, archive or
  move** (reported by Jason on his own account). iCloud's capability list
  has no MOVE, so since 1.22.0 the no-MOVE fallback (#128) ran there, and
  the IMAP crate sends `UID COPY`'s mailbox name as given, unlike `UID
  MOVE`'s: iCloud's "Deleted Messages", with its space, went out bare and
  the server rejected the command. The name is now quoted (RFC 3501 quoted
  string), with a test. Every UID set the worker sends is also
  normalised: sorted, deduplicated and collapsed into ranges.
- **The server conversation is in the log.** The console records each
  IMAP command Vireo sends (`> UID COPY 150100:150103 "Deleted
  Messages"`) and the server's verdict (`< OK` or `< BAD Parse Error`),
  plus the sign-in mechanism and IDLE starts, without message bodies or
  secrets; the Microsoft Graph requests and their status; the POP3
  commands (PASS redacted); and each SMTP send. All of it under the
  `vireo::imap`, `vireo::graph`, `vireo::pop3` and `vireo::smtp` targets,
  so "Export log" carries it after the fact; stderr sees the failures
  only. On failure the bulk-move path also names the folders and the set.
- **`uninstall.sh`** (PR #142 by @thecalamityjoe87): undoes what
  `install.sh` placed in the prefix — binary, icons, launcher,
  translations — and the icon override the app writes for a chosen app
  icon; `--purge` also removes the settings, cache and data directories
  (keyring passwords stay). Same `PREFIX` convention as the installer.

## 1.23.0 — 2026-09-07

Tags, swipe actions on message rows, a redrawn icon set with a new
default, a log export for bug reports, four fixes, and the French
translation completed.

- **Tags** (#71, requested by @yioannides, with the folders-versus-tags
  discussion from @p-mitana and @deusnovus). Coloured labels a message can
  carry several of, defined in Settings → Accounts → Tags (name, colour,
  keyword). A tag is stored on the server as an IMAP keyword, the
  standard's own per-message user flag beside `\Seen` and `\Flagged`, so
  the same tag shows in Thunderbird, Apple Mail or a webmail, and theirs
  show in Vireo once a tag names their keyword (Thunderbird's built-in
  five are `$label1` to `$label5`; the tag dialog explains). Microsoft 365
  accounts store them as categories. POP3 accounts, and IMAP servers whose
  PERMANENTFLAGS refuse custom keywords, keep the tag in Vireo's own index
  against the Message-ID instead, with a one-time notice.
- **Where tags show.** A pill at the end of the subject on every list row;
  chips beside the sender on conversation cards and under the subject of a
  full-bleed message; a collapsible Tags section in the sidebar, above the
  accounts, that lists every tag and opens a cross-account view of the
  mail carrying it (Trash and Junk excluded, Gmail's per-label copies
  collapsed to one row).
- **Where tags are set.** The message's right-click menu, the Actions
  Palette's new tag button, the reader toolbar's tag button and the
  reader's overflow menu: one entry per tag, its swatch filled where the
  message already carries it. Untagging a message inside its own tag view
  drops the row.
- **Rules can tag.** The filter dialog gains "Tag with" beside "Move to",
  and "Move to" gains "Leave in Inbox", so a rule can tag, file, or both
  (the tag goes on before the move and travels with it). Tag-only rules
  hide the two folder switches on their row.
- **Swipe actions** (PR #135 by @thecalamityjoe87, for #92 by
  @taprobane99): drag a message row sideways with the mouse or a
  two-finger trackpad swipe. Left deletes, right archives; the row slides
  off a coloured strip naming the action, dimmed until the drag passes the
  commit distance. Settings → Message List gains "Swipe actions" (on by
  default) and "Reverse swipe directions". Built on `AdwSwipeTracker` over
  a small `AdwSwipeable` container (`SwipeSurface`) that keeps the row's
  content on top of a fixed action strip.
- **Dragging a message to a folder** now carries a cursor-sized white
  envelope (the icon gallery's) instead of the raw payload text, and the
  row fades to half strength until it lands (#92 aside). The
  per-account rows under All Inboxes and the Filtered Folders rows take
  drops too: a message dropped on one moves to that folder, provided it
  belongs to the same account.
- **A redrawn icon set, and a new default.** The app's icon is now a
  blue envelope with the bird on it; the 1.21 squircle-with-a-V becomes
  the "Logotype" gallery entry. The gallery is redrawn to GNOME's icon
  guidelines: four more Vireo envelopes (yellow, white, beige, faded
  blue), six plain envelopes (blue, yellow, white, beige, starfield,
  faded blue) and the two birds, ahead of the colours. The
  beta build ships the default's `.Devel` twin with GNOME's hazard stripe
  (the old ribboned beta icon is gone). The new default is asserted once:
  the first start on this release resets the stored choice to it on every
  install (`app_icon::ICON_GENERATION`, recorded in `state.toml` so a
  choice made afterwards stands). Ids from the 1.22 gallery still resolve
  for imported settings: envelope → yellow envelope, cream → beige, the
  blue birds → the new birds. `tools/gen-app-icons.py` renders `<id>.Devel.svg`
  sources to `alt/<id>.Devel.png` and takes the bird envelope, plain and
  `.Devel`, as the hicolor icons of the two builds (the beta now has a
  scalable SVG too).
- **An account whose Sent folder was set to the Inbox vanished from All
  Inboxes** (#136, @EmmanuelP): the role took the Inbox's kind with it, so
  the account had no inbox to list, notify for or filter. A role never
  takes the Inbox now (the assignment is ignored), and the account
  editor's Special Folders combos no longer offer it.
- **Forward in the main window had no To field** (#139, @7system7): the
  inline reply pane hides its address rows to stay compact, and a forward
  got the same treatment although it arrives unaddressed. A pane whose To
  is empty keeps its rows.
- **Leaving a quote in the editor** (#137, @EmmanuelP): Enter twice inside
  a quoted block now steps out below it, the way a list ends, so a reply
  can be written under an excerpt.
- **Getting a deleted message back** (#138, @thecalamityjoe87): Trash and
  Junk rows offer "Move to Inbox" in their right-click menu, the
  multi-selection menu and the reader's overflow menu, alongside the
  existing Undo toast and dragging the row onto a folder.
- **Export log** (for #132 and any report that needs one): Settings →
  System has "Export log", and the status bar's console carries an export
  button while it is open. The file starts with the build, the desktop,
  GTK and libadwaita versions, then everything the console recorded since
  the app started (the console's buffer grows from 2,000 to 20,000 lines
  so a session fits), with email addresses shortened to their domain.
- **French** (PR #134, @frenchy82): the composer's picture-resizing menu
  and the last loose strings; 713 of 717 messages translated.
- Under the hood: a `keywords` column on the message index (added in
  place; rows fill in as folders re-sync), a `local_tags` table for the
  Vireo-only tags, `tags.toml` beside `filters.toml` (both in the settings
  bundle), a `.tag-<keyword>` colour stylesheet the chips and sidebar
  share, and a `tag-symbolic` icon.

## 1.22.0 — 2026-09-06

A translatable interface with a French translation, pictures from the
file manager and picture resizing in the composer, HTML signatures,
`mid:` links, deleting on servers without MOVE, a redrawn icon set, and
Ctrl+C in the reader. Everything previewed in the 1.22.0 betas.

- **Translations through gettext.** Every user-facing string goes
  through the helpers in `src/i18n.rs` (`i18n()`, `ni18n()` for plurals,
  `i18n_f()`/`ni18n_f()` with named `{placeholders}`, `i18n_noop()` for
  tables translated where shown); the text domain is bound at startup to
  the first directory holding a catalogue (`VIREO_LOCALEDIR`, the source
  tree's `po/.build`, the install prefix, the system prefixes). The app
  follows the desktop's language. `po/vireo.pot` is regenerated by
  `tools/update-pot.sh` (`xtr` plus `xgettext` for the launcher and
  metainfo); the Flatpak, the RPM and `install.sh` compile `po/*.po` and
  merge the translated launcher and metainfo fields (`po/LINGUAS`).
- **French translation** (`po/fr.po`, contributed by @frenchy82, #122 and
  PR #131): the interface, the launcher entry and the app description,
  with a second round of corrections landed from PR #131. 705 of 716
  strings; the eleven left are the composer's new picture-resizing menu.
- **Labels that stayed English** (#122, @frenchy82): tables marked with
  `i18n_noop` must be translated where shown, and several consumers passed
  them straight through: the sidebar's folder context menus, the editor
  toolbar's tooltips, the shortcuts help and the provider hint. Also
  translated now: the message list's conversation entries, the
  preview-lines dropdown, the tray's Open Vireo, the filter dialog's
  title, and every dialog button that was a bare literal.
- **Files from a file manager reach the message** (#126, PR #127 by
  @typedev). WebKitGTK hands the editor document a `text/uri-list` it
  then refuses to serve, so a dropped or pasted file arrived as a link or
  a bare path. The widget takes such files itself: a `GtkDropTarget`
  declaring only `GdkFileList`, and a synchronous clipboard-formats check
  before a paste is handed over. Images go inline through the document's
  downscale-and-insert; anything else, a picture over 32 MB, or a type
  outside a fixed list the engine decodes becomes an attachment. A
  picture keeps its filename on the `<img>` as `alt`, which names the
  `cid:` part at send (reduced to a bare filename first, since `alt` is
  reachable through a quoted reply and lands in a header).
- **Pictures can be resized** (PR #127): corner handles on click; Small,
  Medium, Large and Original Size in the context menu as fractions of the
  writing width; the width goes to the inline style and the `width`
  attribute. **Recompress to This Size on Send** arms a picture (red
  frame and dashed outline) to be recut once, as the message is sent;
  drafts never touch the pixels and keep the arming. The frame and
  handles carry `data-vireo-ui` and the body is read through
  `__vireoBodyHtml()`, which drops them.
- **Signatures from an HTML file, or edited as HTML** (#120, requested
  by @7system7). The account editor's Signature group gains "Import
  File…" and "Edit HTML…". Both go through
  `rich_editor::signature_from_source`: plain text is escaped like a
  typed signature; HTML is sanitized with ammonia keeping tables, inline
  `style` and images and dropping scripts, style sheets and handlers; an
  image referenced by a local path is embedded as a `data:` URI (up to
  8 MB) so the send path lifts it into a `cid:` part.
- **Moves work on a server without the MOVE extension** (#128, reported
  by @EmmanuelP on a Zimbra account). Every move, deleting included, went
  through `UID MOVE`; without MOVE the server answers "command not
  permitted with UID". `worker::uid_move` asks `CAPABILITY` and, without
  MOVE, does `UID COPY`, flags the originals `\Deleted` and expunges
  (`UID EXPUNGE` with UIDPLUS, plain `EXPUNGE` otherwise).
- **No stray frame under an empty sender list** (#129, @EmmanuelP): the
  allowed-senders and blacklist lists are hidden until they have a row.
- **`mid:` links open the message with that Message-ID** (#130,
  requested by @7system7 for the Vicinae extension). Vireo registers for
  the `mid` URI scheme (RFC 2392). The id is normalized as the cache
  stores it (percent-decoded, brackets optional, lowercase, an optional
  `/content-id` after the domain dropped, slashes inside the id kept,
  GLib's `mid:///` form accepted). The local index is asked first through
  a new index on `message_id`; on a miss every IMAP account runs `UID
  SEARCH HEADER Message-ID` folder by folder (POP3 and Graph answer from
  the index only), and a miss is reported once in the notification bar.
  A running instance takes the link over D-Bus.
- **Ctrl+C in the reader.** The message list takes GTK focus back after a
  click over a message body, so Ctrl+C landed on the window and copied
  nothing (the context menu's Copy worked). The window hands Ctrl+C to
  the reader whenever the keyboard is not in a text field, the composer
  or the reader's own view; the page copies from whichever document holds
  the selection, hands the text to the host if the engine refuses, and
  shows a "Copied" pill.
- **Conversations** may pull in up to 100 messages from other folders
  (was 50); what is in the open folder was never capped.
- **Icons.** Sources live in `data/icons/src` (SVG, or a 1024² PNG master
  where librsvg cannot render the artwork) with `tools/gen-app-icons.py`
  regenerating every gallery PNG, the hicolor icons and `docs/logo.png`.
  The default icon is also installed as a scalable SVG (install.sh,
  Flatpak, RPM). The envelope is redrawn as "Envelope, yellow" with
  cream, blue and white variants; the gallery runs Default, the four
  envelopes, the birds, the colours, the patterns, Classic.
- **Tests.** The tray's tests build again (#124, reported with a PR by
  @typedev, #125); tests for the filename path, the hostile-`alt`
  rejection, the signature sanitizer and the `mid:` parser.

## 1.21.1 — 2026-09-05

Refreshed icon artwork.

- **Icons.** Every app icon regenerated from the new files: the shipped
  default, the beta channel's ribboned icon, and the sixteen gallery
  alternatives. A second bird variant carrying an @ joins the gallery as
  "Bird, @" (`data/icons/alt/bird-blue-at-symbol.png`).

## 1.21.0 — 2026-09-05

Choose the app icon, a new default icon, and the redrawn wordmark.

- **App icon gallery.** Settings → System & Appearance gains an "App
  icon" row: a sideways-scrolling strip of eighteen icons — the yellow
  default, fifteen colour and pattern variants, the bird, and the
  classic envelope — with the current choice ringed and the edges fading
  where more lie beyond. The welcome wizard's "Make it yours" page
  carries the same strip. The set is embedded in the binary
  (`data/icons/alt`, 512px). A choice is applied by writing the art
  under its own name (`<app id>-<choice>`) into
  `~/.local/share/icons/hicolor` at 512 and 256, and pointing a per-user
  copy of the launcher in `~/.local/share/applications` at that file by
  absolute path — the mechanism menu editors use, and the only one GNOME
  Shell honours at once: it caches app icons by name for the session and
  only re-scans icon directories on its own schedule. The copy carries
  `TryExec` so it hides itself once the app is uninstalled; Default
  removes it. A launcher the user's own install owns (install.sh, a
  source tree) has its `Icon=` line edited in place. The Flatpak
  manifests add `--filesystem=xdg-data/icons/hicolor:create` and
  `--filesystem=xdg-data/applications:create`.
- **New default icon.** The shipped icon is the yellow squircle; the
  round envelope stays available as "Classic". Existing installs keep
  the envelope: the first start that finds no choice records "legacy"
  when settings already exist on disk, and only a fresh install gets the
  new default (the wizard lets it pick). The beta channel ships the
  ribboned yellow icon as its own Default and never offers it as a
  colour; "Classic" is hidden there, since it would hide the ribbon.
- **Tray.** The tray's "Vireo icon" option draws the chosen app icon.
- **Restart offer.** Changing the icon shows a heads-up with Later and
  Restart Now. The app grid follows at once; GNOME Shell keeps a running
  app's windows bound to the app object it created for the old launcher,
  so the dock's running entry and Vireo's own windows switch on restart.
  Restart Now keeps the window up, under a modal with a spinner, until
  nine seconds have passed since the launcher was written — the shell
  rate-limits its watch on the launcher directory and reloads five
  seconds later, and a relaunch inside that window binds to the old
  object — then hands off. Outside Flatpak the app spawns itself as a
  helper (`--restart-helper`) that waits for the D-Bus name to free and
  execs a fresh instance. Inside Flatpak a spawned child dies with the
  sandbox, so the helper is the exported D-Bus service
  `co.hyprlab.Vireo.Restart` (`data/*.Restart.service`), activated by
  the bus as a new sandbox instance; the app quits only once the bus
  confirms the helper owns its name, else a toast says to reopen by hand.
- **Wizard.** Finishing the wizard is recorded in `state.toml`, so an
  install still without an account is not greeted again on the next
  start (a restart right after the wizard looped back into it). The
  restart helper drops the `VIREO_WELCOME` and `VIREO_SHOWCASE*` review
  switches. The window's yellow is now #fec200.
- **Wordmark and artwork.** The redrawn wordmark (viewBox 1329×483) in
  the wizard and About window; the README logo and every icon refreshed
  from the new files.

## 1.20.2 — 2026-09-04

Filter rules can be edited, and their rows get more room.

- **Edit Filter.** Each rule row in Settings → Accounts → Filters is now
  activatable, marked with a pencil beside its trash button as the alias
  rows are. Activating it opens the filter dialog as "Edit Filter" with
  every field prefilled — account, where, match, text, destination
  folder, and the two switches — and Save replaces the rule in place,
  keeping its position. Adding is unchanged.
- **Roomier rule rows.** The rows gain padding around their two-line
  title and the stacked switches, which sat against the row edges; the
  pencil and the switch labels take the full foreground colour, like
  the trash button and the title beside them.
- **Demo.** The demo's Accounts panel now gets folder choices for its
  stand-in accounts, so its filter dialog has destinations. Dev:
  `VIREO_SHOWCASE_EDIT_FILTER=<index>` opens that rule's editor for a
  capture, and Settings captures target the newest window so a dialog
  over Settings is what gets shot.

## 1.20.1 — 2026-09-04

Filter-rule folders reachable from All Inboxes, per rule.

- **Filtered Folders under All Inboxes.** Each filter rule gains a "Show
  under All Inboxes" switch, off by default, beside "Count unread mail"
  in Settings → Accounts → Filters and in the Add Filter dialog; the two
  switches stack in a two-column grid so rule titles keep their width.
  Folders of rules that opt in are listed in a collapsible "Filtered
  Folders" section inside All Inboxes, under the per-account inbox rows.
  The heading leads with its caret like the accounts' "Folders" heading
  and, folded up, wears the section's unread total as a chip that hides
  while the section is open, as the All Inboxes chip does. Rows come from
  the same builder as the folders under an account's "Folders" heading
  (leaf expander indent included), with a new filter-folder glyph tinted
  in the account's colour in place of the folder icon. Selecting a row
  opens the folder, right-click offers Mark as Read and Refresh, and
  unread chips update in place. The section folds with All Inboxes; in
  the icon-only rail it is a glyph toggle over tinted glyphs. A selected
  filtered folder that leaves the section (its rule opted out, or the
  section switched off) keeps its highlight on the account section's own
  row. The rule flag is `show_in_unified` in filters.toml, defaulting to
  off so existing rule files load unchanged.
- **Settings → Sidebar: "Filtered folders under All Inboxes".** A global
  switch, on by default, that hides the section whatever the rules say;
  greyed out while All Inboxes itself is off. Stored as
  `unified_filtered` in privacy.toml.
- **Dev: showcase hooks.** `VIREO_SHOWCASE_SETTINGS=accounts|prefs`
  captures the Settings window instead of the main one,
  `VIREO_SHOWCASE_SCROLL=<0..1>` makes it tall and scrolls its panels that
  far down first, and `VIREO_SHOWCASE_FOLD_FILTERED` folds the Filtered
  Folders section, for checking those states in stills.

## 1.20.0 — 2026-09-04

The 1.20 feature release, previewed through nine betas (1.20.0-beta.1
to beta.9). Spell checking and inline images were proposed in
discussions #114 and #113 (@typedev); the attachment fixes are
@typedev's PRs #110, #112 and #118 for #109, #111 and #117 (#109
confirmed by @mfreeman72); the tray icon and the unread-count work
answer #116 (@mfreeman72, with @p-mitana's off-by-default advice and
@yioannides' icon offer), refined through @mfreeman72's beta testing on
Linux Mint.

- **Spell checking in the composer (#114).** WebKit's checker runs in
  the message body; the subject line asks the same enchant engine
  directly, since GTK entries have none of their own. Both check the
  word being typed: the subject on every keystroke (exempting the word
  under the cursor until a 400ms pause), the body's caret word via a
  600ms round trip drawn with the CSS Custom Highlight API. Settings
  gains a Spelling group: an on-by-default switch, a language dropdown
  offering exactly the installed dictionaries (named in their own
  language), and an "Added words" list managing the personal dictionary
  that Learn Spelling feeds. The Flatpak bundles eleven languages beyond
  English; English variants are trimmed to the five anyone looks for.
- **Inline images in the composer (#113).** Pasting or dropping a
  picture puts it in the text at the caret, downscaled to 1600px and
  obeying the composer's writing width; sending lifts each into an
  inline cid: part inside multipart/related, out of the recipient's
  attachment list. A click selects an image whole (delete/cut/copy work
  on it); right-click offers "Send as Attachment Instead". WebKit's
  native image paste arrives as a blob: URL, invisible to clipboardData
  and dead on the wire, so every blob: image is adopted into a scaled
  data: URI the moment it appears.
- **A tray icon for the desktops that have a tray (#116).** Vireo can
  publish a StatusNotifierItem, which is what AppIndicator means today:
  Cinnamon, KDE, MATE, XFCE, and GNOME with the AppIndicator extension
  draw it. The item is the Vireo icon, or the reader's unread envelope
  in white or black for panels that don't recolour symbolic icons, with
  a red dot on its top-right corner while there is unread mail, a
  tooltip with the count, and a menu: the newest five unread messages
  as card rows (the sender's picture or initials; sender, account when
  there are several, and date; subject; preview line), each opening in
  the reader, then "View all N unread…" (All Inboxes, or the first
  counted folder with unread mail), Open Vireo, Accounts, Settings,
  Quit. A click on the icon brings the window back. Off by default: a
  switch, an icon choice and a mail-list switch under Keep running in
  the background. On a desktop with no tray nothing is drawn and
  Background Apps is untouched; the item keeps waiting, so enabling a
  tray extension later picks it up without a restart. On Cinnamon the
  icon is drawn at five-eighths of the pixmap, since its applet draws
  the pixmap at the colour icon size beside 16px symbolics. Actions on
  a message stay in the reader: a DBusMenu is a vertical list of
  icon-and-text rows and cannot carry buttons. New dependency: `ksni`.
  The Flatpak manifest gains `--talk-name=org.kde.StatusNotifierWatcher`.
- **Filtered folders count toward unread, per rule (#116).** Each
  filter rule carries a "Count unread mail" switch (`count_unread`, on
  by default, in the rule's row and the Add Filter dialog; existing
  rules load with it on). The unread total behind the All Inboxes chip,
  the tray icon's dot and menu, and the Background Apps status is the
  inbox plus the folders of counting rules; Trash and Junk destinations
  never count. The tray menu lists unread mail from those folders too;
  their lists are primed from the disk cache at startup and fetched
  quietly when a rule starts counting a folder. Server-side sorting
  Vireo does not know about is not counted.
- **A folder's list follows its unread count in the background
  (#116).** The worker's IDLE sits on the folder last opened, so with
  another folder in view the inbox only ever got count updates from its
  watcher and the sweep; its message list refreshed when the inbox was
  next opened. The tray menu's cards and the new-mail notification both
  read that list, so the menu said "No unread mail" under a live count
  and mail arriving while a filtered folder was open raised no
  notification. A changed count on a counted folder now asks the
  worker for a quiet resync (`MailRequest::SyncFolder`): the same fetch
  as opening the folder, without the status text and without adopting
  the folder for IDLE or the watch list.
- **Cached mail opens at once, whatever the worker is syncing.** Each
  account's worker takes requests strictly in order, so a click on a
  message whose body was cached long ago sat behind a folder sync, a
  backfill chunk or an attachment prefetch: at startup the reader
  showed its spinner over mail already on disk. A cache lane now sits
  in front of each worker: a thread of its own that answers what the
  disk cache can (a body, a batch of bodies, an attachment list, a
  conversation lookup) and passes only network work on, in order.
- **Reply follows Reply-To.** Summaries carry the Reply-To list from
  every ingestion path (ENVELOPE, iCloud raw-header fallback, POP3,
  Graph); Reply and Reply All answer it instead of From, and Reply All
  keeps the To address out of Cc. Cached mail heals as folders re-sync.
- **Wide mail scrolls.** A message wider than the pane made its
  sandboxed frame horizontally scrollable, and WebKit's wheel-latching
  swallowed vertical scrolling over it. Frames now widen to their
  content inside a panning wrapper, so the wheel always reaches the
  page and wide mail pans sideways in place.
- **Split reply reworked.** The panel holds the height it is given (a
  big paste can no longer push it down), dragged by an iOS-style grab
  pill floating at its bottom edge, and slides in and out on one
  animated divider (the revealer's own transition never ran: it starts
  unmapped, and adw skips animations on unmapped widgets). While a
  reply is open the reader's header bar slides out, since it showed a
  second set of window decorations mid-window; on close it slides back
  in step with the panel, its icons fading in, and the teardown no
  longer re-clamps the divider for a frame. The editor fades in when
  its document has loaded; New Message defers its reveal one frame so
  its slide plays, and closing it slides up before removal. The
  dragged height is remembered.
- **Attachment drawer reworked.** The same grab pill replaces the
  chevron: click toggles collapsed/expanded (animated, both ways), drag
  resizes live, from collapsed too. GtkPaned's own capture-phase drag
  and pan gestures, which hit-tested an enlarged handle area and left
  dead click zones near the seam, are removed; the seam is one
  continuous handle (grab zone, hairline, edge strip) that lights on
  hover, with a cursor matching how it works. Collapsing no longer
  flashes, and a resized drawer reopens at its dragged height.
- **Paste is plain text by default.** Ctrl+V strips formatting; the
  editor's context menu always offers "Paste with Formatting" and
  "Paste as Plain Text"; a Settings switch ("Paste as plain text", on
  by default) flips the default.
- **Attachment fixes (#109, #111, #117; PRs #110, #112, #118).** Small
  attachments sent from web Gmail are no longer dropped by the
  inline-image heuristic; a labelled Gmail message's attachments are
  fetched once, not once per label; filenames split across two RFC
  2047 encoded-words are rejoined, keeping their extension. The
  attachment cache is rebuilt once on upgrade (schema v13) so mail
  already synced by affected builds heals too.
- **Account Settings… from the sidebar opens that account's editor.**
  Right-clicking an account header or one of its folders and choosing
  Account Settings… opened Settings on the Accounts list; it now opens
  the editor for that account, stepping back from another account's
  editor first if one is up.
- **The attachments gallery's table keeps its columns.** The Size
  header's label had hexpand set, which propagates to its button, so
  the header row split spare width between Name and Size while the
  rows gave it all to Name; every header between them sat left of its
  column. A dotless filename (a generated attachment-1) had the whole
  name for an extension, widening its Type cell and sliding that row's
  Sender left. Only the Name header expands, every fixed-width cell is
  clipped to its column, and an extension has to look like one (short,
  alphanumeric, after a dot) or the cell says File.
- **Add Sender to Contacts** joins the message list's right-click menu.
- **Discord** joins the About window's project links, and the README
  gains the Vireo Manifesto.

## 1.19.2 — 2026-09-02

- **Emails with their own dark mode render on the right ground.** A
  message that ships `@media (prefers-color-scheme: dark)` rules (a
  Google Calendar invite, say) had those rules evaluated by WebKit
  against the desktop's light/dark preference, which the `color-scheme`
  the reader injects into each sandboxed message frame does not change.
  So with the desktop in dark mode but a message shown light — a light
  message theme, or the app's and desktop's schemes disagreeing — the
  email painted its light-grey dark-mode text onto the reader's white
  card, rendering as near-invisible grey on white; the mirror case put
  an email's light rules on the reader's dark card. Each
  `prefers-color-scheme` media query is now pinned to the ground the
  reader actually chose, so an email's own light and dark rules follow
  the card, not the desktop.

## 1.19.1 — 2026-09-01

Memory-use fixes for long-running sessions (#106, reported by
@mfreeman72): the process tree could grow past 2 GB over a day of use
and never shrink.

- **WebKit on a document-viewer diet.** Every WebView (reader, pop-out
  windows, print preview, compose editors) now shares one web context
  configured with the DocumentViewer cache model and a 512 MB
  memory-pressure limit. The default browser cache model kept an
  in-memory resource cache and back/forward page cache that only ever
  hoarded dead documents — each render loads a fresh unique URI, so
  nothing cached was ever revisited — and the web process grew by
  hundreds of MB per reading session. It now trims itself back under
  pressure instead of waiting for system-wide memory pressure.
- **Byte-bounded in-RAM caches.** The rendered-body cache (fed 50
  bodies per folder per sync by the background prefetch) and the
  opened-attachment cache were unbounded HashMaps; they are now
  oldest-first evicting caches capped at 64 MiB and 128 MiB
  (`src/ram_cache.rs`). Everything evicted re-reads from the SQLite
  cache in a blink.
- **Sender logos downscaled at decode.** Domain icons (up to 1024²
  apple-touch-icons, a few MB of decoded pixels each) are now
  downscaled to the same 160 px edge avatars already use, with the
  same decompression-bomb guard.
- **Attachments gallery releases its data.** Leaving the gallery now
  drops the eagerly loaded item bytes (up to 300 × 6 MiB per account,
  previously held in two copies until quit); it reloads from the cache
  on the next visit exactly as it already did.

## 1.19.0 — 2026-09-01

The 1.19 feature release, previewed through three betas (1.19.0-beta.1
to beta.3). Features requested in #38 (@isorropisths), #47 and its beta
feedback (@mfreeman72), #50 (@doodoobug-dot), #86 (@yioannides), #97
(@Toxblh), #100–#103 (@p-mitana); fixes for #99 and #105 (@frenchy82).

- **First-run welcome wizard.** A brand-new install is greeted by a
  five-step guided setup — account (one-click GNOME Online Accounts
  imports plus a manual IMAP form with provider presets and a live
  connection test), privacy choices, and popular defaults — with the
  wordmark riding the carousel's spring from hero to header. The main
  window appears when the wizard finishes or is dismissed. Beta builds
  carry a Welcome Wizard burger-menu entry for reviewing it safely;
  stable builds do not.
- **Console mode.** Settings → System & Appearance gains a status-bar
  console: a live verbose log (dedicated vireo=debug tracing layer)
  in a dracula-styled, CRT-grained, selectable view — via a status-bar
  button, the burger menu, or Ctrl+Shift+C; resizable with a 160px
  floor. WebKit's JS console pipes into the same log.
- **Mail filters (#47).** Accounts tab → Filters: file inbox arrivals
  into folders by From address/name, Subject, or To/Cc (contains / is
  exactly / starts with / ends with), per account, first match wins,
  applied on sight so mail that arrived while Vireo was closed is
  filed on the next sync. Filed mail still raises the new-mail
  notification: when the newest arrival was filed, the notification
  opens the destination folder, with Mark as Read/Archive omitted;
  filter moves are remembered per sync so a sync racing the
  server-side move can't re-request it.
- **Settings backup (#50).** Export every configuration file as one
  TOML bundle (passwords stay in the keyring, never exported); import
  replaces the config in place and offers a self-restart.
- **Notification actions (#38).** Single-message new-mail
  notifications carry Mark as Read and Archive buttons that act
  without raising the window.
- **Split replies (#86).** Reply/Reply All/Forward slide a compact
  composer (editor only; pop out for the full fields) down from the
  reader's top, with the conversation visible and interactive below,
  scrolled to the card being answered with the selection outline.
  Cancelling a split reply clears the reply-target outline, and the
  compose body editor wears the fields' card shadow everywhere.
- **Search reworked (#102, #103).** The list's search bar hides
  behind a header button (or Ctrl+F, or /); the reader gains
  find-in-message with rounded pill highlights (current match solid,
  the rest translucent), a live "N of M" counter and arrows, hidden
  text excluded and matches walked in visual order.
- **Quick filters (#97).** Unread-only and starred-only toggles beside
  the sort menu, composable, session-scoped.
- **Read marking rebuilt (#100, #101).** Conversation members mark
  read as they come into view (the old scrolled-through path never
  fired), with a Settings → Reading policy: when displayed, after two
  seconds, or manually. Threads open on the first unread, falling
  back to the newest message.
- **Conversation starring.** A thread row's star (palette, context
  menu, or reader toolbar) stars or unstars the whole conversation —
  any member starred reads as a starred thread — while individual
  messages keep their own stars; the reader no longer collapses to a
  single message when starring an open conversation.
- **Threads surface their newest message** in the list (sender,
  avatar, preview), and a conversation row's context menu can mark
  the whole thread read or unread.
- **Reorganized Settings.** Filters, Allowed Senders and the
  Blacklist live on the Accounts tab; the composer always shows its
  Subject; the About window is rebuilt around the wordmark with
  flowing Release Notes/Changelog text.
- **Single messages render as cards by default on new installs.**
  Existing installs keep their saved choice.
- **Fixes**: cold-start composers from Nautilus's "Send by email"
  keep their From field (#105); avatarless rows align top-left with
  equal padding (#99); emoji avatars centre with their own optical
  parameters; the sidebar's Accounts panel shows the demo accounts in
  demo mode.

## 1.18.4 — 2026-08-30

Composer attachment fixes with Isaac (@thecalamityjoe87, PR #96).

- **Attachment pills hug their content.** The composer's attachment
  chips sat in FlowBox cells that stretch by default, so a pill's
  background ran the full cell width past its remove button and the
  empty remainder highlighted on hover. Chip and cell now shrink to
  the content, and only the remove button takes focus.
- **Files handed to the app open a composer.** A file manager's
  "Open With Vireo" (or `vireo <file>` on the command line) opens a
  fresh composer with the files attached, relayed to the running
  instance over D-Bus like mailto; arguments that name no real file
  are ignored.

## 1.18.3 — 2026-08-30

Fix release for #90 and #91 (both reported by @frenchy82) plus two
sender-seal corrections under GNOME text scaling.

- **A folder click can no longer be swallowed by IDLE (#91).** Ending
  an IMAP IDLE awaited the DONE handshake with no bound, so a server
  that never answers (or a connection a middlebox silently killed)
  wedged the worker with the interrupting request already dequeued:
  clicking a folder did nothing until a restart. DONE now gets 5
  seconds, SELECT/EXAMINE and IDLE-init 10, in the main loop and both
  watcher kinds; a timeout drops the dead connection and the pending
  request reconnects and completes.
- **Push is a per-account choice (#91).** Each account's editor gains
  Syncing → "Instant new mail (IMAP push)": Follow Settings, On, or
  Off. Off also suppresses that account's inbox and per-folder
  watchers, so one server that mishandles IDLE no longer costs the
  others their instant delivery. Existing configs are untouched
  (absent key = follow the global switch).
- **Nautilus attachments (#90).** "Send by email" in GNOME Files
  passes files as attach= parameters on a mailto: URI; the composer
  now attaches them (absolute paths or file:// URIs, several allowed,
  only existing regular files) as normal removable chips. The
  mailto:/// form no longer leaves "///" in To:.
- **The sender seal under text scaling.** The verdict popover is
  anchored by the measured widget/page ratio, so it centres on the
  seal at any GNOME text scaling factor; the seal itself is now
  em-sized and baseline-anchored, so it sits level with the sender's
  name instead of sinking when the type shrinks.

## 1.18.2 — 2026-08-30

Sender authentication in the message header, a chevron-placement
preference, and thread rows that surface the newest message. Sidebar
work with Isaac (@thecalamityjoe87, PRs #89 and #95); the header seal
was requested by @taprobane99 (#88).

- **Sender seal in the header (#88).** The DKIM/SPF/DMARC verdict
  renders as GNOME's verified-checkmark seal beside the sender's
  name: blue for authenticated, amber for suspicious, red for
  failing. Clicking it opens a popover, anchored on the seal, listing
  each check's result. The icon is the icon-development-kit seal as
  bundled by Bazaar.
- **Chevron placement preference.** Settings → Chevron placement
  offers Left or Right (the previous layout; default for new
  installs). The Left layout comes from Isaac's PRs #89 and #95:
  disclosure chevrons are overlaid on the row's left edge and reserve
  no layout space, so icons, labels and unread chips share one column
  down the sidebar. Double-clicking All Inboxes toggles its
  per-account list.
- **Sidebar fixes (PR #95).** Sidebar symbolic icons are pinned to
  exact 16px boxes, removing a subpixel drift against the avatar
  column. Account avatars are now 30px (sub-list pills 21px) to make
  room for the leading chevron. Expanded rows get a small left inset.
- **Thread rows show the newest message.** A collapsed thread row now
  displays the newest member's sender, avatar and preview alongside
  the newest-member date it already used. Row identity (selection,
  reply, expansion) still belongs to the thread head.
- **Thread-wide read toggle.** A conversation row's context menu
  offers Mark All as Read / as Unread for every member, via the bulk
  pipeline, replacing the singular toggle on that row. Expanded
  replies keep their per-message toggle.
- **"Sender circles" renamed to "Sender avatars"** everywhere. With
  avatars off, the unread dot aligns with the sender name and the
  Actions Palette shifts left to match.
- Beta versions use semver prereleases (X.Y.Z-beta.N) natively in
  Cargo.toml; VERSION is the crate version verbatim on both channels.

## 1.18.1 — 2026-08-30

Fast-follow polish release: everything the 1.18.0 feedback surfaced,
plus two long-requested integrations (thanks @thecalamityjoe87,
@yioannides, @frenchy82, @p-mitana, @taprobane99, @tbaumann).

- **Vireo registers as an email client (#87).** The desktop entry
  declares `x-scheme-handler/mailto`, so GNOME's Default Applications
  lists Vireo and mailto: links open a prefilled composer
  (to/cc/bcc/subject/body, RFC 6068 decoding — plus-addressing safe).
  Second launches now hand off over D-Bus and exit instantly (relm4's
  run loop never exits for a remote instance; registration also can't
  happen early, since relm4 builds the UI in its own startup handler).
- **Manual special-folder mapping (#82).** Each account's editor gains
  a Special Folders section: Sent, Drafts, Trash, Junk and Archive,
  each Automatic or pinned to a real folder. Overrides demote the
  auto-detected holder, ride every role-routed action, persist in
  accounts.toml, and fall back to detection if the folder disappears.
- **The list stays put (#84, from Isaac's PR #85).** Deleting a
  message or closing a compose while scrolled elsewhere no longer
  snaps the list back to the selection: `preserving_scroll` pins the
  adjustment through row removals and the focus-restore, and a removed
  focused row hands focus to its neighbour without scrolling.
- **Deletion follows your direction of travel.** Moving down the list,
  delete selects the message below; after moving up, the one above
  (Apple Mail's behaviour). The bulk path no longer clears the
  selection before removal, so the reader advances properly; rows drop
  GTK's focus ring (the selection pill is the indicator).
- **Undo, made trustworthy.** Ctrl+Z selects and reveals the restored
  message, spins the refresh indicator while the server works, and
  survives iCloud: a HEADER search that misses falls back to scanning
  the newest UIDs' Message-ID headers, and summaries lost to iCloud's
  BODY[1]-after-append quirk repair themselves via a deep BODY[TEXT]
  retry (which also heals long-blank previews).
- **Address menus behave.** The reader's address right-click menu
  dismisses on any click (a scrim under the menu — the sandboxed body
  frames can't dispatch events), gains Add to Contacts, and matches
  the app's context-menu styling. The toolbar's Add-sender button
  retires in its favour.
- **The message list, re-balanced (yioannides' #81 pass).** The
  Actions Palette floats over the pill instead of reserving a phantom
  line, so text centres and rows tighten; it slides out of the ⋯ onto
  one shared card (one open at a time), the ⋯ centres under the
  sender circle, thread member cards share the top-level geometry,
  and the thread rail ends at the last member's node dot. Palettes
  and cards mirror the reader toolbar's order, Add to Contacts and
  View Source joining before the close. Read/unread icons show the
  action again, everywhere.
- **Reader toolbar folds when measured, not assumed.** The overflow
  threshold is measured from the real headerbar (and re-derived when
  gtk-decoration-layout changes), so three-button window layouts no
  longer push the close button off the pane.
- **All Inboxes, quieter.** The total-unread chip hides while the
  per-account list is expanded and sits beside the label when folded;
  two new switches control All Inboxes and its chip. Sender logos
  persist on disk with a weekly staleness check. Settings dropdowns
  never truncate (custom non-ellipsizing combo factories). Composer
  recipient rows grow to 42px. Toggling preview lines back on
  refreshes immediately.
- **Peek sidebar, steadied.** The narrow-window overlay no longer
  shifts the panes beneath it (the ghost strip rejects snapshots of
  the expanded panel), and its header matches the expanded sidebar's.
- New installs default to collapsed in-list conversations and
  toggle-gated card actions (existing settings untouched); the README
  points Nix users at @tbaumann's community-maintained flake.

## 1.18.0 — 2026-08-29

The beta-tested 1.18 feature release (previewed as 1.18.0b, hardened by
the community's feedback in discussion #81 — thanks @p-mitana,
@thecalamityjoe87, @frenchy82, @yioannides).

- **Contacts move into the app.** The sidebar's Contacts row opens a
  full view in the content area: a searchable, sortable list
  (first/last name or email, live count, resizable pane, accent
  selection) beside a full contact card — photo (expandable to the
  lightbox; iCloud photos render now), labelled emails (compose or
  copy), phones, postal addresses, websites, birthday, notes, and which
  address book the entry lives in. Contacts can be edited, created and
  deleted right here — writes go through EDS D-Bus so GNOME Contacts
  and CardDAV stay in sync, and edits patch the stored vCard so
  unedited properties survive. Composing from a contact slides the
  composer down over the card; GNOME Contacts stays one click (or
  right-click) away. Address books removed or contacts-disabled in GOA
  disappear (liveness comes from the EDS source registry).
- **One Settings window.** Accounts and Settings share a window behind
  an AdwViewSwitcher; a preference picks the opening view; every option
  regrouped into focused sections; a GOA account's editor hides the
  GNOME-owned connection fields entirely.
- **Bulk actions stop blocking.** Rows leave the list instantly, server
  work runs invisibly in the workers, and nothing waits. The refresh
  button spins while background work runs; the status bar narrates it
  and gains two new routes in (long-press Refresh, Ctrl+Shift+S). Bulk
  removals backfill the rendered window; deletes keep the header count
  honest.
- **All Inboxes is instant at launch** — folders, unread counts and
  inbox slices paint straight from the disk cache before any worker
  starts; each account's catch-up sync lands behind. Empty folders show
  a proper "No Messages" page, and Graph/POP3 folders finally report
  their index complete (no more stuck "Loading more…").
- **Threads slide open and shut** in the message list — per-row
  revealers with surgical row insert/remove and a rotating caret
  (thanks @thecalamityjoe87, PR #79). Conversations land on their first
  unread, support thread-wide delete (optional confirmation), Ctrl+A,
  threaded popouts, and an optional newest-message-first reading order
  (#70). "Single messages as cards" (#57) optionally renders a lone
  message exactly like a one-message conversation.
- **Composer**: a Reply-To field behind the To row's "More" (#58),
  written as a proper wire header for SMTP and Graph alike; forwards
  keep their formatting — bodies pass through an HTML sanitizer
  (ammonia) that strips scripts, handlers, styles and dangerous URLs
  while tables, links, headings and images survive (#52); the editor no
  longer flashes dark on open.
- **Reader**: an "Always show recipients" preference keeps the To/Cc
  line open under each sender (single-recipient chip dropped as
  redundant) (#40); header polish (pinned palette corner, address
  links, honest previews, full-strength icons); Space previews the
  highlighted attachment (#37).
- **Sidebar**: Contacts and Attachments pin to the bottom edge in one
  gapless footer section (from @thecalamityjoe87's PR #80, issue #78);
  the All Inboxes chevron aligns with the account chevrons and gets a
  32px hit target; account headers honour the configured label; the
  menu gains section breaks; the gallery header gets the sidebar
  toggle.
- **Fixes from beta feedback**: encoded-word sender names with illegal
  interior spaces decode ("…DPD ?="); the message count updates on
  delete; GNOME notifications clear when mail is read in the app (#41);
  bold pane titles; the contact editor's title sits between Cancel and
  Save; matching AdwStatusPage placeholders; the conversation count
  chip hides its caret when expansion is off and centres its number.
- **Beta infrastructure** (#83): the beta's shared-data grant now
  creates the stable directory on the host (`:create`), so a beta-first
  install establishes the persistent home a later stable install picks
  up — in either order — with a mounted-check fallback so accounts can
  never land on the sandbox tmpfs again.
- **Under the hood**: per-folder IMAP IDLE watchers keep subfolder
  unread chips near-instant on a one-hour activity lease; the sidebar
  peek self-heals; demo mode gains a sample address book; the README
  documents @bennypowers' community Gentoo overlay (#53) and the
  --user flatpak install flag (thanks @yioannides, PR #75).

## 1.17.1 — 2026-08-28

- **Microsoft 365 via GNOME Online Accounts works (issue #36).** GOA's
  `ms_graph` provider serves no IMAP (its token is Graph-scoped), so the
  old import produced an empty-host account that died on connect. A new
  `graph` protocol speaks Microsoft Graph end to end with the GOA token:
  folders (well-known roles mapped), summaries, raw-MIME bodies through
  the same parsing pipeline as IMAP, flags, moves, undo, folder
  management, drafts, and sendMail (which files the Sent copy itself).
  Threading rides a synthetic conversation token stripped before any
  wire header; broken pre-#36 imports heal to Graph at config load; the
  inbox polls on the auto-fetch cadence (default 2 min) since Graph has
  no push channel.
- **The embedded Microsoft OAuth client is removed.** Google and
  Microsoft sign-in both route through GNOME Online Accounts (the
  provider entries guide there); user-supplied clients via env or
  oauth.toml remain the only native escape hatch.
- **GOA accounts, first-class in the Accounts window.** Toggling one off
  un-imports it back to the GNOME Online Accounts list (GNOME keeps the
  account); the editor gains the standard Remove button (Vireo-only
  removal); the greyed-out server section is gone from GOA editors; and
  saving no longer validates GNOME-owned connection fields — which had
  blocked every label/signature edit on Graph and Gmail accounts.
- **Sidebar rework.** Contacts moves from the reader toolbar to a
  sidebar row below Attachments (with a Preferences toggle); Refresh
  moves beside a new "+ New Message" pill aligned to the row highlights;
  the status-bar button is retired in favour of error auto-reveal plus a
  "Reveal Status Bar" menu entry.
- **The message list header slims down.** The folder-title row is gone;
  the visible count and the sort menu live in the pane's header bar,
  across from the sidebar toggle.
- The demo inbox (VIREO_DEMO) opens on a six-message conversation for
  screenshots.

## 1.17.0 — 2026-08-28

- **Inline compose.** The reader pane is wrapped in an overlay at init; New
  message slides a full-height composer down over it (300ms SlideDown),
  covering the toolbar — the compose header takes over the window
  decorations while inline. A pop-out button moves the draft to the old
  separate window; "Compose in a window" preference restores the previous
  behaviour outright. The compose body loses its frame, takes the
  conversation cards' page ground, and gets a 20px interior document inset.
  The sidebar footer's collapse button is gone; compose is an accent
  "New message" row in the sidebar and the sidebar toggle sits leftmost in
  the message pane header.
- **Unlimited undo (Ctrl+Z).** Every destructive move records an
  `UndoEntry` (account, destination, origin folder, Message-IDs): delete,
  spam, bulk move, and drag-and-drop. Undo re-finds the messages by
  `HEADER Message-ID` search — UIDs change on IMAP MOVE — moves them back,
  drops stale cache rows, and reloads the restored folder with a toast.
  Ctrl+Z is guarded so it never fires while typing in an entry or the
  composer. **Ctrl+W** maps to `window.close` (background sync continues,
  matching the existing close-to-background behaviour); both are listed in
  the shortcuts window. (Issue #64.)
- **Per-alias SMTP (issue #34).** `AliasConfig` grows optional SMTP
  host/port/username with the password under a dedicated keyring key; the
  alias editor gets the fields plus a Test button, and the transport layer
  picks the alias's server by envelope sender — including Outbox retries.
- **Conversation layout for everything.** Single messages render through
  the conversation document as one full-bleed surface: no card padding,
  radius, or borders, header on the same ground as the subject, and a
  painted-background heuristic so plain-text/unstyled mail shows one
  continuous chrome-coloured pane. Threads keep inset cards; expanded
  thread rows share the list's right edge and widen the pane floor.
- **Scroll-to-unread that lands.** Opening a thread with unread mail
  scrolls to the *last* unread card using real rendered frame heights, a
  follow mode that chases the target through image/quote settling until
  user input, and a synchronous position report before card interactions
  so click re-renders restore to the right anchor. Read-marking is
  click-driven (scrolling past no longer marks read); dot updates patch
  the DOM via JS instead of re-rendering, killing the height blip.
- **Card action palettes.** Ten actions per card (reply/reply-all/forward,
  move, spam, delete, flag, read/unread, print, view source) behind a ⋯
  toggle with three modes — always shown, hover-to-reveal (space reserved,
  300ms fade), or click-to-expand — set in Preferences and sharing the
  palette auto-collapse timeout; the message list palette can also open on
  hover. Mark as Read/Unread added to the reader toolbar and card headers.
- **Message list restyle.** Selection is a full-accent pill with white
  text in both schemes (no focus dimming); unread accent pills are gone —
  the 10px dot alone marks unread, in white on selected rows; thread count
  + caret merge into a grey chip that inverts to white-on-accent when
  selected; thread heads date by their newest member; separators removed;
  minimum list width down to 348px.
- **Attachment drawer.** Divider invisible (drawer tint, no
  border/shadow), resizing locked until the drawer is expanded, height
  persisted (debounced) across messages and restarts, 6px bottom padding,
  and Save All… is a standing button at the header's right edge.
- **Remote-content banner** follows the theme (shield #ffca28 dark /
  #ff7800 light), 48px tall with standard grey pill buttons, single-line
  text while there's room, reworded to "Remote content (images, trackers)
  is blocked to protect your privacy."
- **Stability & chrome.** Concurrent poppler PDF thumbnail renders crashed
  (lcms2 heap corruption) — rendering is serialised behind a process-wide
  mutex. Startup highlights All Inboxes; Preferences/Accounts/About open
  100px shorter and remember their heights; message inset normalised to
  20px (cache schema bumped to re-render); View Source left the toolbar
  for the context menu and card palette.

## 1.16.2 — 2026-08-27

- **A cancelled attachment chooser stays cancelled (issue #65).** With no
  default handler registered, the OpenURI backend shows its app chooser even
  on the quiet (ask=false) attempt; cancelling answered response 1, which
  the quiet-attempt arm treated as "failed, retry with the chooser" — the
  dialog popped straight back up. Response 1 is now honoured on either
  attempt; the genuine-failure retry (response 2, the Fedora 44
  broken-direct-launch case) is untouched. Verified via a signed local
  test bundle.
- **Message list highlights are inset pills.** Hover, unread, thread-unread,
  and selection (bright and focus-dimmed) all paint on the inner
  .message-row with the thread cards' rounded inset treatment; padding was
  trimmed by exactly the new margins so content geometry is unchanged.
- **Separators float too**: the row hairlines take the same 6px side inset
  as the pills instead of running wall to wall.

## 1.16.1 — 2026-08-27

- **Rename folders.** "Rename Folder…" in a custom folder's context menu:
  a dialog pre-filled with the current name renames the leaf in place via
  the same optimistic machinery as drag-and-drop moves (shared
  `apply_folder_rename`; display name recomputed from the new leaf,
  sub-folders ride the server RENAME, collisions refused).
- **Single-clicking a parent folder toggles its sub-tree** — wired through
  `row-activated`, which fires on every click including the already-selected
  row; the caret still works and consumes its own clicks. Leaves select as
  before.
- **Unread chips survive folder operations.** A refresh's per-folder STATUS
  can fail silently (Gmail answers zeros right after a RENAME) and
  SetFolders adopted them wholesale, wiping every chip. Counts now merge by
  path and a zero never overwrites a known count; genuine zeros re-assert
  through per-folder sync events. (The intended 1.16.0-era fix had silently
  failed to apply — its patch anchor missed; now verified in place.)
- **New folders appear instantly.** Creation pushes the folder into the
  local list and runs the shared `normalize_folders` step (worker sort,
  index ids, unread/selection re-key) instead of waiting out the server
  round-trip and re-list.

## 1.16.0 — 2026-08-27

### Folder tree (issue #51)

- **Custom folders render as a collapsible hierarchy.** Parents get a caret
  that spins open/closed (CSS `-gtk-icon-transform` transition); descendants
  hide by row visibility, so selection indices, context menus, and drop
  targets never shift. Collapsed nodes persist per account+path in
  `sidebar.toml`. Nested rows carry a full-path tooltip ("Work › 2025 ›
  Archive"). Expanding/collapsing slides rows via per-row revealers.
- **Folders move by drag-and-drop.** One IMAP RENAME moves the subtree
  (RFC 3501 §6.3.5); drop on a folder to nest, on the "Folders" header for
  top level. Guarded: same account only, never into own subtree, never under
  special folders, name collisions refused with a notification. The move is
  **optimistic** — the local list is reshaped exactly as the worker will
  report it (same sort, same index-assigned ids; unread map, selection, and
  collapsed nodes re-keyed) so the sidebar updates instantly and the
  confirming refresh is a recognised no-op.
- **The sidebar can no longer jump or flash on rebuilds.** Four mechanisms,
  each real: scroll offset pinned during the layout pass; a freeze-frame
  snapshot shown over the widget swap; rebuild-created revealers start at
  duration 0 so content reaches full height in one pass; and the viewport's
  scroll-to-focus is off (a destroyed focused row made GTK yank the view).

### Server robustness

- **An empty LIST is never trusted** (INBOX always exists): a wedged iCloud
  session's empty response used to wipe an account's folder list — cached,
  so it survived restarts. Both list paths and the app now keep what they
  have.
- **Interior NUL bytes are scrubbed from message text** at every ingestion
  point (envelope builders, preview extractor, cache loaders, body
  delivery). One wild message with a 0x00 aborted glib mid-`set_label`,
  killing the message list and then the app.
- **`folder_namespace` means INBOX-rooted only**: it previously took the
  first nested folder's parent as "the namespace", sending new folders and
  top-level moves under a random folder on servers like iCloud. Delimiter
  inference likewise stopped trusting the first dot it saw.

### Dark mode (issues #35, #62)

- **Automatic contrast for mail.** Message documents pass through a colour
  adaptation engine at render time (never the disk cache): every declared
  colour — hex/rgb()/named, inline styles, `<style>` blocks with `@media`,
  legacy `font`/`bgcolor`/`text` attributes — is flipped in HSL when its
  lightness is wrong for a dark ground. Dark text lightens, light
  backgrounds become surfaces, dark-designed mail passes untouched,
  url()/data: payloads are never touched, and CSS comments with semicolons
  can't split declarations (the Buy Me a Coffee trap).
- **Reader grounds follow the live theme** (`view_bg_color`, page shaded a
  step deeper) instead of hard-coded pairs; grounds join the render
  fingerprint. Trust-badge colours use libadwaita's
  success/warning/error named colours.
- **App theme preference**: Follow system / Light / Dark for the chrome
  itself (AdwStyleManager), separate from the message-content theme.

### Sidebar

- **Hover-expand peek** (preference): hovering the icon rail floats the full
  sidebar over the panes; it folds 1s after the pointer leaves. Click-open
  uses the same overlay. Fixed the scrim-dismiss watcher killing every peek
  mid-open; open/close animate with a deferred show and staged restore; a
  ghost rail snapshot (cached on pointer-enter) keeps the panes still and
  the strip painted; at wide widths the arrow pins the sidebar open
  (persisted). The hamburger and the footer toggle hold their rail
  positions while floating.
- The expand/collapse toggle is a standard-size icon button
  (sidebar-show-symbolic): right-aligned expanded, centred on the rail.

### Message list & reader

- **Thread members are inset pill cards** on a dotted rail with node dots
  (ringed to mask the rail). Selection dims via `:focus-within` instead of
  vanishing when the reader takes focus; clicking the reader's empty space
  no longer unselects the list (falls back to the viewed message).
- **Context menus share one GNOME-styled builder** (PR #63 by Isaac) with
  per-entry icons matching the toolbar, a restored "N selected" caption,
  and the same treatment in the drawer, gallery, and sidebar.
- **The reader header collapses into an overflow menu** below 560px
  (AdwBreakpointBin) so the close button can't be pushed off; reader floor
  drops 480→400px. List floor is a constant 374px (a member card's palette).
- **The attachments drawer replaces the toolbar attachments menu**, with
  Save All in its header; the quiet blocked-content banner is the only
  style — compact, with small pill buttons — and its brightness preference
  is gone.

## 1.15.7 — 2026-08-26

- **Panning a zoomed lightbox document is smooth.** The drag gesture lived on
  the picture, whose coordinate space moves with every pan — each scroll
  displaced its own measurement and the view jittered. It now lives on the
  scroller, whose space stays put.
- **The toolbar attachment menu's Open works.** It still ran a fossilised
  third copy of the open logic — private-/tmp staging plus a bare AppInfo
  launch, exactly what the sandbox work fixed elsewhere. Both its paths now
  delegate to `open_bytes`, and the fossil is deleted so a third copy can't
  drift again.

## 1.15.6 — 2026-08-26

### The Flatpak really opens attachments now

Three sandboxed-open fixes, found by testing against a real installed
Flatpak (`flatpak-builder --run` turned out to have no session bus at
all — nothing portal-shaped can be tested there):

- **Staging moved to `$XDG_RUNTIME_DIR/app/$FLATPAK_ID`.** The document
  portal validates an exported fd by re-opening its path in the HOST
  namespace; a file in the sandbox's private /tmp fails that silently,
  killing every OpenFile before any UI.
- **The portal protocol is spoken directly** (GIO D-Bus): GTK's
  FileLauncher mis-finishes its own async task in this runtime — its
  callback never fires, so successes and failures alike vanished. We now
  subscribe to the request's Response first (own handle_token, no race),
  pass the fd, and read the verdict: quiet attempt, then one `ask: true`
  chooser retry (cancel respected), dialog only on real failure.
- **Options marshal as `a{sv}`** via `Variant::tuple_from_iter` — a Rust
  tuple's ToVariant boxed the dict as "(shv)" and the portal rejected
  every call.

Verified live: chooser → Papers on a machine whose direct portal
launches are broken.

### The lightbox

- **Fills the Vireo window** instead of opening a second window with its
  own titlebar — same overlay look as the gallery's, driven by the app
  (`DrawerOutput::ShowLightbox`), with Escape/arrow keys handled at the
  window level (capture phase, gated on the lightbox being open).
- **Click to zoom 3x, anchored at the click** — the clicked content
  centres in the viewport (a tick callback waits for the resize to lay
  out before positioning). Click or Escape returns to fitted; Escape
  from fitted closes; **dragging pans** the zoomed document (a shared
  movement threshold keeps a pan from also zooming).

## 1.15.5 — 2026-08-26

### Opening attachments: a chooser fallback, and the truth on failure

- **When the portal's direct launch fails, Vireo retries with the app
  chooser** (`OpenFile` with `ask: true`, one blocking zbus call from a worker
  thread). The chooser is the portal backend's own dialog and launches the
  picked app through different machinery than the failing default-handler
  path — verified working on a machine whose direct launches all fail —
  and "always open with" persists in the permission store, so it's one
  confirmation ever, not one per open.
- **The failure dialog earns its keep**: it shows the portal's own error text
  (reportable verbatim) and the two steps that actually help — Download →
  open from Files, and updating `xdg-desktop-portal` + re-login. No Flatseal
  advice: portal access is not a permission, so no toggle exists for this.
  Also fixed: a codegen step had baked a run of 14 literal spaces into the
  dialog copy, which rendered as a janky gap.
- **Double-clicking a lightbox preview opens the document externally** — in
  the gallery's lightbox and the drawer's alike.

## 1.15.4 — 2026-08-26

### The Flatpak opens attachments again

- **Root cause:** the sandboxed build stages an opened attachment into the
  app's *private* `/tmp`, and then handed the desktop portal a `file://` URI
  *string* — a path that, host-side, does not exist. The launched viewer
  pointed at nothing, on every machine; the click read as dead. (The fix took
  a detour: one test machine's portal is also broken for host callers, which
  masked the real mechanism until a second machine reproduced it.)
- **Fix:** the sandboxed branch now launches through `gtk::FileLauncher`,
  which passes the staged file as a **file descriptor** via the document
  portal — the portal exports it at a host-readable path and hands that to
  the handler. Native builds keep GIO-first launching.
- When a portal genuinely cannot launch anything, both chains now end in a
  dialog saying so — and that Download still works — instead of silence.

## 1.15.3 — 2026-08-26

### Send-as aliases (#34)

- An account can declare extra From identities — a "Send-as aliases" field in
  the account editor (`Ann <ann@work.org>, ann@shop.org`). The composer's
  From menu offers every identity; an alias changes only the From header on
  the wire (and the Message-ID's domain, so replies thread back) — SMTP
  server, credentials and the Sent copy stay the account's. Replying to mail
  addressed to an alias answers from that alias automatically. Aliases
  persist in accounts.toml; two new tests pin the wire format.

### Attachments

- **The drawer activates on double-click only.** Its single-click lightbox
  stole the first click of every double-click (the modal opened, the second
  click landed in it), which made "open externally" unreachable and read as
  the Open path being broken. Single clicks now do nothing; double-click
  previews images/PDFs in the lightbox (whose Open button launches the
  default app) and opens other types externally at once. The toolbar
  attachment menu matches: double-click a row to open, and its Preview
  button drives the lightbox directly — which also fixes a latent index
  mismatch when the drawer's list view was sorted.

### Fit and finish

- The conversation chevron pill's border-radius drops from 9999px to a
  concrete 10px: GTK paints an oversized radius on a 1px-bordered pill with
  a faint dot at the centre of each rounded end.

## 1.15.2 — 2026-08-26

A community-feedback release: most of this answers p-mitana's issue
series (#22, #25, #27, #28 — #23 was already fixed in 1.13.4) and the
inline-forward half of #52.

### The reader (#22)

- **Conversation cards lead with an initials avatar**, tinted per sender
  address (pure escaped markup — no image bytes cross into the sandboxed
  document), so who wrote each card and which are your own Sent replies reads
  at a glance.
- **Card headers stick** to the top while their message scrolls, over an
  opaque ground; hovering anywhere on a card tints its header.
- **Reading width caps at 1000px**, centred, for cards and single messages.
- **View Source** shows angle brackets (new `code-symbolic`) instead of the
  ghost; the **sender-authentication badge** is a checkmark seal (new
  `verified-checkmark-symbolic`) instead of the lightbulb.

### The composer (#25, #52)

- **The inline reply/forward pane shows its address fields.** From (with more
  than one account) and To in every host — an inline forward was literally
  unaddressable before. Subject shows for new messages and drafts, and waits
  behind the To row's "More" button for replies/forwards, with Cc and Bcc
  (Cc surfaces on its own when a reply-all carries it). Focus lands in To
  when it's empty.

### The message list (#27, #28)

- **Sent-folder rows name their recipients** — "To: <names>", with the circle
  showing (and face-looking-up) the first recipient instead of the sender.
- **The pane width survives a restart**: divider drags persist (debounced,
  one write per drag; clamped 350–4000px) and the window opens with it. The
  fixed opening width is gone; `state.toml` gains `list_pane_width`.

### Attachments and chrome

- **The drawer previews in-app**: single click shows images *and PDFs* in its
  lightbox (PDF first pages render on workers into the shared full-size
  cache); other types ignore single clicks, and double click opens anything
  externally — grid cells and list rows alike.
- **Opening externally works outside Flatpak** even where the XDG portal
  accepts an OpenURI and silently launches nothing: native builds lead with
  GIO and fall back to the portal; the Flatpak keeps portal-first.
- **"Go to Message" moves the sidebar highlight** to the message's folder
  (same for notification opens), so Attachments can be clicked to return.
- The status bar leads with the bell (matching its toolbar button), the
  thread chevron is an accent-outlined pill sized to its count chip, and an
  expanded conversation widens the list floor by the indent so child rows
  never clip.

## 1.15.1 — 2026-08-26

### Attachments

- **Attachments download the moment a message or conversation opens** — disk
  cache first, then the server. The reader's "load attachments" button (a
  download icon that turned into the paperclip) is gone: the toolbar shows a
  spinner while fetching and the paperclip when they land. Applies to single
  messages, whole threads, popped-out windows and attachments discovered late
  in a body (inline PDFs, #9); `AttachmentsPending` remains handled as a
  safety net that fetches instead of asking.
- **The gallery lightbox renders PDFs in-app**: a full-size first-page render
  (1600px, off the main thread behind a spinner page, cached per content
  hash — failures too, so a broken PDF isn't re-rendered on every arrow
  press). Stepping through mixed images and PDFs stays in the lightbox.
- **Table view: headers line up with their columns** — the header buttons'
  own padding was offsetting every label — and rows gain the grid's quick
  actions as a trailing column (Download, Open, Go to Message), with a
  matching header spacer.

## 1.15.0 — 2026-08-26

### Attachments

- **Opening an attachment works again** (adapted from PR #44 by Isaac,
  @thecalamityjoe87). The staging code passed `0o200000` as O_NOFOLLOW, but on
  Linux that value is O_DIRECTORY — creating a regular file with it fails, so
  no attachment could be staged, and the symlink refusal the flag was meant to
  provide was silently absent. The constant is now the real O_NOFOLLOW
  (`0o400000`), with a comment naming the old bug. Opening also goes through
  the XDG portal (`gtk::UriLauncher`, properly parented) instead of
  `AppInfo::launch_default_for_uri`, which under Flatpak — or for a type with
  no registered default app — silently does nothing; the portal shows GNOME's
  app chooser instead, and the old call remains as a fallback when the portal
  itself is unreachable.
- **PDF attachments show their first page as the thumbnail** in the gallery
  and the in-message drawer (adapted from PR #49 / issue #48, also Isaac):
  rendered via poppler-glib at 360px on a white ground, falling back to the
  type icon when a PDF won't parse. The Flatpak manifest gains a poppler
  module (config after Evince's, JPEG2000 off); native builds need
  poppler-glib-devel / libpoppler-glib-dev.
- **All thumbnails decode off the GTK thread, behind per-cell spinners.**
  Rendering PDF pages (and decoding images) while cells were being built froze
  the window — long enough for GNOME's Force Quit dialog on a real mailbox.
  Cells now show a spinner immediately, workers produce the texture, and
  finished renders are cached by content hash so rebuilds, searches and
  revisits this session never decode the same attachment twice. The gallery's
  "Loading attachments…" page also actually shows now (its flag was being
  reset by the clear that followed it).
- **The attachment drawer covers whole conversations.** Opening a thread never
  loaded attachments at all — the request lived only in the single-message
  path, so the drawer stayed empty until a member was clicked and re-clicked.
  A conversation now asks the disk cache for every member's attachments as it
  opens and shows the deduplicated union, re-merging as members' attachments
  or late-arriving related messages land. Selecting one message out of the
  thread still shows just that message's attachments.

### The attachments gallery

- **A control footer**: grid ↔ table view toggle, a type filter (images,
  PDFs, documents, archives, audio & video, other), the sort dropdown (moved
  down from the toolbar, which keeps just the search), a thumbnail-size
  slider (140–380px, one debounced rebuild per drag), and a shown-of-total
  count. View choice, thumbnail size and sort order persist across sessions.
- **A table view**: name, sender, type, date and size under clickable column
  headers — click to sort, click again to flip, with the dropdown following.
  Rows keep the grid's behaviours (click to preview, double-click to open,
  right-click for the menu) and reuse already-rendered mini thumbnails
  without ever spawning renders of their own.

### The reader

- **A "To:" line above the Cc line** for single messages (adapted from PR #43,
  Isaac) — at a glance, who a Sent/replied message went to, or which of
  several addresses an auto-forward landed on. Conversation cards already
  carry this in their recipients chip (1.14.2), so nothing changes there.

### Project

- **GitHub release pages carry only their own release's notes** (issue #46,
  suggested by @yioannides). `tools/release-notes.sh` extracts one version's
  section (RELEASE_NOTES.md, falling back to CHANGELOG.md) with a footer
  linking the full history; all 72 existing releases were rewritten through
  it.

## 1.14.4 — 2026-08-26

### Sender avatars

Adapted from PR #8 by Anton Palgunov (@Toxblh), unified with the sender-logo
pipeline that had grown on main in the meantime.

- **GNOME Contacts photos as avatars.** A background thread indexes photo
  locations (not bytes) from Evolution Data Server's local and CardDAV
  address-book caches, with a stat-fingerprint stability check against
  half-written SQLite files. Sender circles resolve personal-first: contact
  photo → Gravatar → domain icon → coloured initials, each tier consulted
  only while its switch is on — so switching a tier off hides its cached
  images immediately (this also fixed cached sender logos surviving the
  "Show sender logos" switch being turned off).
- **vCard PHOTO parsing** handles Nextcloud `data:` URIs, standard folded
  `ENCODING=b`, grouped (`item1.PHOTO`) properties, and EDS-materialized
  `file://` photos. Remote PHOTO URLs are never fetched.
- **Confinement and decode hardening.** `file://` photos are canonicalized
  and confined to EDS's own data/cache directories (`confine_to_roots`, with
  tests for `..` traversal, symlink escape, lookalike sibling directories,
  unresolvable roots and host-bearing URIs). Only known raster formats reach
  the decoder — never SVG — and images are size-capped, pixel-capped and
  downscaled to 160px during decode, off the GTK thread.
- **Gravatar improvements.** Requests are coalesced per sender with a
  concurrency cap, timeouts and a 30-second retry backoff; only a definitive
  404 is cached as a miss. No hash leaves the machine before the local
  contact index has had a chance to answer, and the hash sent is now SHA-256
  rather than dictionary-reversible MD5 (the `md5` dependency is gone).
- **Fresh without flicker.** An EDS/CardDAV sync bumps the index generation
  and refreshes visible circles without moving the list's scroll position;
  generation-stamped caching discards stale results rather than masking a
  newly synchronized photo. The reader correlates avatar results by sender
  address instead of mailbox-scoped IMAP uid.

## 1.14.3 — 2026-08-26

### GNOME Online Accounts

Adapted from PR #7 by Anton Palgunov (@Toxblh).

- **Custom server ports are honored.** GOA stores a non-standard port inside
  the host string itself (`mail.example.com:1143`, `[2001:db8::1]:1993`);
  discovery now splits host and port apart — IPv6 brackets included — instead
  of discarding the port and assuming 993/143 and 465/587.
- **Pausing instead of losing.** Switching an account's Mail service off in
  GNOME Settings pauses the account in Vireo: workers stop, it leaves the
  sidebar, and every local setting (label, colour, emoji, signature, sidebar
  state) survives. Switching Mail back on restores it, including whether it
  was enabled in Vireo before the pause. New `AccountConfig` fields
  `goa_mail_disabled` and `goa_enabled_before_mail_disabled`; the accounts
  window locks the enable toggle and says why while GNOME Settings owns it.
- **GOA changes are handled off the main thread, debounced.** One Settings
  edit emits a burst of D-Bus signals; the watcher now coalesces them for
  300 ms and takes a single `GetManagedObjects` snapshot on its own thread,
  delivered inside the `GoaChanged` message — the GTK main thread no longer
  performs blocking D-Bus I/O. The watcher also subscribes to
  `PropertiesChanged` on account objects, so property edits (the Mail toggle
  among them) are seen at all, not just removals.

### Connections

- **The connection test authenticates SMTP the way sending does.** OAuth
  accounts (Gmail, Microsoft) are tested with XOAUTH2 and a fresh token
  instead of PLAIN/LOGIN with a password they don't have — so their test no
  longer reports a false failure. The server address is passed as a
  `(host, port)` pair, so a bare IPv6 SMTP host no longer mis-parses.
- **IMAP connection attempts time out after 30 seconds.** TCP connect, the
  TLS handshake and authentication together now run under one deadline, so a
  server that accepts the socket and stalls surfaces an error naming the host
  instead of hanging the worker forever.

## 1.14.2 — 2026-08-26

### The window

- **Tiles to half a screen.** With a populated mailbox the window's minimum
  width was 1070px — wider than half of a 1920px display — so GNOME refused
  Super+←/→ and edge-drag tiling and offered only the top-edge maximize. The
  message list and sidebar scrollers no longer impose their content's width on
  the window's minimum: names ellipsize and rows clip gracefully instead of
  vetoing the tile.
- **The sidebar gives way when the window narrows.** Below 1120px — a
  half-screen tile on a 1920px display — the sidebar drops to its icon rail
  automatically, which is what leaves the message list its full Actions Palette
  width in the tile. The user's own collapse preference is restored the moment
  there is room again, and the automatic switch never overwrites it.
- **Expanding while narrow floats.** From the rail in a tiled window, the
  sidebar's expand button opens it as an overlay above the panes rather than
  pushing them aside. Picking a folder closes it, as does clicking the dimmed
  content or pressing Escape.

### The sidebar

- **Every folder shows its unread count**, not only the Inbox. The counts were
  already fetched per folder (IMAP `STATUS (UNSEEN)`); now each folder row
  carries the badge, updating in place — in the icon rail it rides the icon's
  corner.
- **Sub-folders in the server's structure.** Custom folders sort by their full
  hierarchical path, case-insensitively, and nested folders are indented under
  their parents — rather than appearing in whatever order the server's LIST
  returned them. Depth follows listed ancestors, so a Dovecot-style `INBOX.`
  namespace prefix doesn't produce a phantom level.

### The message list

- **The toolbar's Delete deletes the whole selection.** With several messages
  selected, the trash button (and the `d` shortcut) removes them all through
  the same path as the selection bar — including the "delete permanently"
  confirmation when the selection is already in Trash. Its tooltip says how
  many it is about to take.

### Conversations

- **Each card names everyone the message went to.** An "N recipients" chip in
  the card's header expands into the full To/Cc list — selectable for copying —
  and collapses again so headers stay one line tall. Recipient headers are
  attacker-controlled text and land escaped in the trusted wrapper document,
  under regression tests.

### Attachments

- **The drawer grew a list view.** A header toggle switches the thumbnail grid
  to an alphabetical list — type icon, filename, size, Download/Open — with an
  A→Z / Z→A switch beside it. Both choices persist. Hovering anywhere on a
  thumbnail now shows the full filename.

### Status bar

- **The notification dropdown is now called the status bar** — in its tooltip
  and its panel — which is what it is.

## 1.14.1 — 2026-08-25

### Conversations

- **Mail that was already in the mailbox threads.** 1.14.0 stamped a
  `threading_since` instant the first time it ran and grouped only messages at
  or after it. An account added after that — or the same account re-added on
  another machine — therefore had an inbox where nothing threaded at all, however
  complete its `References` headers were, because `compute_thread_keys` filtered
  on the timestamp *before* the union-find ran and those messages never reached
  it. The stamp, the `thread_old_mail` preference that gated it, and the
  `ts >= ?` predicate in `messages_by_thread_ids` are gone: a message threads
  because its headers say what it answers, whenever it was sent. Covered by a
  test built from the headers iCloud actually delivers.
- **The References repair runs for every folder**, rather than only when the
  retired setting was switched on. A message indexed by an older build carries
  only its In-Reply-To, and threading reads References — so this is what makes
  old mail threadable in the first place. Unchanged otherwise: replies only,
  chunked at 500, resumable by watermark, once per folder.
- **Conversations join across folders further back.** The reply-header pool grew
  from 4,000 messages to 20,000. It bounds memory and rebuild cost, never
  correctness — a conversation whose members are all on screen never consults it
  — and what one conversation costs to *open* is still capped at 50 members, the
  bound the date was only ever a proxy for.

## 1.14.0 — 2026-08-25

### Conversations

- **Threading is back, and no longer exhausts memory.** 1.13.3 could consume
  every byte on the machine when a message was opened. The cause was one wrong
  index: `messages_by_thread_ids` binds ids, then padded ids, then the account,
  but read the `References` comparisons from slot `n+2` instead of `n+1`, so
  every one shifted and the last ran against the *account id* — which SQLite
  coerced to text, matching every message whose References merely contained that
  digit. On a real cache one click returned 6,278 "related" messages instead of
  1, each a body to fetch and a re-render of a document holding all the others.
  Two tests cover the query; the first fails on the old indices.
- **A conversation reads oldest first**, with the message that started it as the
  row on screen and its replies descending beneath. Opening the head shows the
  whole thread in that order; opening a reply shows that message alone.
- **Each message is a card**, with its own Reply, Reply all and Forward — the
  toolbar's are disabled while a conversation is shown, because they could not
  say which message they meant. Clicking a card selects that message, Ctrl and
  Shift extend the selection as they do in the list, and the two stay in step in
  both directions. A quoted reply chain is folded behind a ••• that expands and
  collapses, the card growing and shrinking with it.
- **Unread messages are marked** and clear as they are scrolled through, rather
  than the whole thread being marked read on opening it.
- **Threading applies from this release forward by default**, with "Thread older
  messages too" for the whole mailbox. An archive's conversations run years and
  hundreds of messages deep; the two things the date bound was quietly holding
  down — the cross-folder links and the size of one conversation — are bounded
  by count instead, so either setting is safe.
- **A conversation is joined through the folders it spans.** Every reply in an
  Inbox answers something in Sent, so where both sides of an exchange arrived in
  one mailbox it grouped and where they did not it fell into pieces. Reply
  headers from an account's other folders now join what is shown without
  appearing themselves.
- **Replies carry `In-Reply-To` and `References`, and our own mail carries a
  `Message-ID`.** Vireo threaded incoming mail by headers it never wrote: a
  reply began a new conversation for everyone who received it, and with no id of
  our own the SMTP server assigned one on the way out, leaving the copy filed in
  Sent with no identity at all.
- **Gmail's labels no longer show a message three times** ([#45](https://github.com/hyprlab/vireo/pull/45)).
  One message lives in INBOX, All Mail and Important at once; the reader keyed
  on folder and UID, so a six-message thread rendered as eighteen. Bodies,
  sender checks and attachments were keyed the same way, so data cached under
  one label was invisible to the others. Both now match on Message-ID, with
  sender and timestamp required to agree.
- **A conversation's bodies are fetched in one request** rather than one apiece,
  in batches of ten ([#45](https://github.com/hyprlab/vireo/pull/45)).

### The reader

- **Opening a conversation paints once.** It re-rendered per body that arrived,
  and each render is a full page load — so the reader flashed its way through
  loading. It now holds the spinner until the conversation has settled, bounded
  so an unanswered lookup cannot strand it, and returning to a thread already
  assembled paints from it with no spinner at all.
- **A message frame opens at the height it had last time**, so a reopened
  conversation lays out on its first frame instead of every card jumping once
  measured. Frames are sized to their content rather than only ever grown, so a
  short message no longer sits in a tall card.
- **The spinner and the cover behind it follow the message theme**, not the
  app's: a light message in a dark app used to hand a dark spinner to a white
  page.
- **A body can no longer be applied to the wrong message.** A UID is unique only
  within its folder, and the background prefetch pushes bodies from every folder
  it syncs.

### Elsewhere

- **Dragging several messages moves all of them** ([#23](https://github.com/hyprlab/vireo/issues/23)).
  The payload was built from the dragged row alone, and the drop was discarded
  when that row's account differed from the target folder's — which is why
  dragging from the unified inbox often did nothing at all.
- **"All Inboxes" no longer omits an account** that was still syncing, offline,
  or busy backfilling. It cleared every account's slice on entry and rebuilt
  from whatever came back; it now keeps each account's last known inbox and
  tops it up from the caches.
- **Preferences: the sender fields sit with their lists.** "Always allow sender"
  is the first row of Allowed Senders, and it and the Blacklist row add with a
  + button.
- **The blocked-remote-content banner can be quieter, or silent.** A grey style
  with outlined buttons, and a switch to hide it — which gates the notice only;
  remote content is blocked exactly as before.

## 1.13.5 — 2026-08-24

- **Fixed: replies sent from Vireo were not part of any conversation.** Vireo
  threaded incoming mail by its reply headers but never wrote them on the mail
  it sent: `build_email` set From, To, Cc, Bcc and Subject and nothing else. A
  reply composed in Vireo therefore began a new conversation for every client
  that received it — Vireo included — so replying back and forth between two
  accounts produced a pile of unrelated messages. The composer now carries the
  parent's Message-ID and the thread's id chain from the reply prefill through
  to the outgoing message, and `build_email` writes `In-Reply-To` and
  `References`, re-wrapped in the angle brackets the wire format wants (they are
  stored bare). References carries the whole chain rather than just the parent,
  so a client can place the reply even if it never saw the immediate parent.
  Tests cover both directions: a reply carries both headers, and a message that
  starts a conversation carries neither. Mail already sent without them cannot
  be repaired — there is nothing on the wire to reconstruct from. Forwards
  deliberately stay out of the parent's thread.

## 1.13.4 — 2026-08-24

- **Conversation threading is back, and no longer exhausts memory.** 1.13.3
  grouped messages into conversations and pulled in the rest of a thread from
  other folders. Opening one message could then consume every byte of memory on
  the machine. The cause was a single wrong index: `messages_by_thread_ids`
  binds its parameters ids-then-padded-then-account, but read the `References`
  comparisons from slot `n+2` instead of `n+1`. Every comparison shifted by one,
  and the last ran against the *account id* — which SQLite coerced to text, so
  `instr` matched every message whose References merely contained that digit. On
  a real cache one click returned 6,278 "related" messages instead of 1, each
  becoming a body to fetch and a re-render of a document holding all the others.
  Two tests cover the query now; the first fails on the old index.
- **Threading applies to mail from this release forward.** The cutoff is stamped
  once into `state.toml` on first run. Older mail never groups and is never
  pulled into a conversation: an archive's conversations run years and hundreds
  of messages deep, and every member is a body the reader loads and renders.
  Recent mail threads normally — in a real mailbox the largest conversation in
  90 days is five messages. This also retires 1.13.3's References repair pass
  entirely, along with its watermark table: old mail never threads, so it never
  needs its headers backfilled. A conversation is additionally capped at 50
  members.
- **Conversation re-renders are coalesced.** A conversation renders as one
  document holding every member's body, and each arriving body triggered one.
  Bodies arrive in bursts — the background prefetch alone pushes fifty per
  folder synced — so an N-message conversation meant N loads of an N-body
  document, faster than WebKit could retire them. Arrivals now mark the reader
  dirty and one render runs on a short timer.
- **A body can no longer be applied to the wrong message.** `WorkerEvent::Body`
  now carries the folder it was read from. A UID is unique only within its
  folder, so matching on the number alone let a prefetched body from one folder
  overwrite a different message that happened to share it.
- **Fixed: dragging several messages moved only one** ([#23](https://github.com/hyprlab/vireo/issues/23)).
  The drag payload was built from the dragged row alone, and the drop was
  discarded outright when that row's account differed from the target folder's —
  which is why dragging from the unified inbox often did nothing at all. The
  list now publishes its rows' (account, folder, uid, id) keys, a drag maps the
  selected indices through them and carries every selected message, and the app
  groups them by source folder into one server-side `MoveMessages` per group.
  Messages from another account stay put and say so.
- **Fixed: "All Inboxes" could omit an account entirely.** It cleared every
  account's slice on entry and rebuilt purely from what each worker sent back,
  so an account busy backfilling, reconnecting, or offline was simply missing —
  while its own Inbox, seeded from cache, still listed its mail. It now keeps
  each account's last known inbox, tops it up from the folder caches the way
  opening a single folder does, and replaces a slice only when that account's
  load lands.
- **Preferences: the sender fields sit with their lists.** "Always allow sender"
  was in the Privacy group, nowhere near the list it fed; it is now the first
  row of Allowed Senders. It and the Blacklist row both add with a + button
  rather than a checkmark, so the two lists read and behave alike.
- **A quieter blocked-content banner, and a switch to silence it.** The warning
  can now be drawn in grey with outlined amber buttons instead of a full amber
  bar, and can be hidden altogether. Hiding it gates the notice only — remote
  content is still blocked exactly as before.

## 1.13.2 — 2026-08-23

- **Fixed: messages could not be deleted from Trash.** Every delete route ended
  at `move_to(m, FolderKind::Trash)`, and the move is a no-op when the source
  and the destination are the same folder — so in Trash the request was dropped
  before it reached the worker and nothing happened at all, on IMAP and Gmail
  alike ([#20](https://github.com/hyprlab/vireo/issues/20)). Deleting something
  already in Trash now erases it: `UID STORE +FLAGS (\Deleted)` followed by
  `UID EXPUNGE` (RFC 4315), so only the messages you picked go and anything
  another client flagged in the meantime is left alone; servers without UIDPLUS
  answer `BAD` while the response stream is drained, which is caught and
  retried as a plain `EXPUNGE`. Because there is no undo it always asks first.
  A mixed selection splits — whatever is still outside Trash moves there as
  before, and only the messages already in Trash prompt. The purge is grouped
  by folder into one request each, drops the messages from the local cache so
  they don't reappear on the next load, and reuses the bulk spinner for large
  selections. All three entry points go through it: the reader and popped-out
  windows, the row palette, and multi-select.

## 1.13.1 — 2026-08-23

- **New: an option to always load remote content.** Preferences → Privacy →
  *Always load remote content*, off unless you turn it on. Blocking by default
  stays, as do the two existing ways out — load once, or trust this sender —
  but people who would rather not be asked no longer have to be. It hooks
  `remote_allowed`, the single point every render path already funnels through,
  so the reader, conversations, popped-out windows and printing all follow the
  setting without a second gate to keep in sync; since 1.13.0 that same flag
  decides what is stripped and what the content policy permits, so turning it on
  relaxes both together. Toggling it re-renders whatever is open — a
  conversation as a conversation, not collapsed to its first message.
  Contributed by [Isaac](https://github.com/thecalamityjoe87) in
  [#31](https://github.com/hyprlab/vireo/pull/31).

## 1.13.0 — 2026-08-23 — security

Reported privately by [Alexander Lubovenko](https://github.com/typedev), who
reviewed the whole tree and wrote it up properly. Thank you.

- **Fixed: a sender could run JavaScript in the reader.** The `From:` display
  name was escaped for an *attribute* value — `&` and `"` — but it is rendered
  as element text, where `<` and `>` are structural. The message bodies sit in
  sandboxed frames that cannot run scripts, but the headers are drawn in the
  wrapper document around them, which does run one script of its own and can
  read every frame in the thread. So a display name of `<script>…</script>`,
  delivered as an RFC 2047 encoded-word, executed as soon as a conversation was
  opened: no click, nothing visible, and from there every message in that thread
  could be read and sent anywhere. Header text is now escaped as text.
- **The wrapper document also has a Content-Security-Policy now**, so the script
  that sizes the message frames is the only script that can run in it — it
  carries a per-render nonce, and nothing else in that document does. Escaping is
  the fix; this is what stands behind it if the escaping is ever wrong again.
  Confirmed against the engine rather than assumed: with the escaping bug put
  back deliberately, the injected `<script>` is parsed into the page and WebKit
  still refuses to run it.
- **Fixed: remote content could load while the UI said nothing was blocked.**
  Whether a message referenced remote resources was decided by searching for
  fixed strings like `src="http`, so `src="//host/p.gif"` (a protocol-relative
  URL, which resolves perfectly well), `src = "http://…"` with spaces around the
  `=`, `<video poster>`, SVG `<image href>` and `@import "//…"` all went unseen.
  Worse, that same guess also chose the content policy — so a miss switched off
  the stripping, relaxed the policy *and* hid the banner, all at once, and a
  tracking pixel loaded silently.
- **Blocking now follows your setting, not that guess.** The detector decides
  only whether the "Remote content was blocked" banner appears; what is stripped
  and what the policy permits come from your own choice. A detector miss now
  costs a banner rather than the blocking. The detector itself was rewritten to
  walk the markup rather than search it, so the cases above are caught — and so
  detection and stripping can no longer disagree, which they previously did.
- **Links only open if they are `http`, `https` or `mailto`.** An HTML message
  keeps its own `href` values, so it could name `file://`, `smb://`, or any
  scheme some installed application had registered, and one click handed it over.
- **Dropped `--talk-name=org.freedesktop.Flatpak` from the Flatpak manifest.** It
  permits `flatpak-spawn --host` — arbitrary commands outside the sandbox — which
  made the rest of the sandboxing advisory. It was only a fallback for the "Open
  GNOME Contacts" button, which reaches the host app by D-Bus activation anyway.
- **The mail cache is no longer world-readable.** `cache.db` holds message
  bodies, attachment bytes and the harvested address book, and it was `0644`
  inside a `0755` directory while `accounts.toml` — which by design holds no
  passwords at all — was correctly `0600`. The directory is now `0700` and the
  database and its sidecars `0600`, existing caches included. The fallback that
  put the cache in a shared temp directory when there was no data directory is
  gone; there is no acceptable location in that case.
- **Attachments you open are cleaned up, and are no longer readable by other
  users.** They were written `0644` into a predictable `/tmp/vireo-attachments`
  and left there indefinitely. The directory is now created `0700` and checked to
  be ours, each file is created fresh with `O_EXCL`/`O_NOFOLLOW` at `0600` rather
  than written through whatever sits at a guessable name, and the whole directory
  is cleared at startup. Under Flatpak `/tmp` is per-app, so only the
  accumulation applied there; the RPM and Arch packages had all of it.
- **OAuth now uses PKCE `S256` instead of `plain`.** With `plain` the challenge
  *is* the verifier, so anyone able to read the authorization URL — browser
  history, an extension, another local process — could redeem a stolen code,
  which is the one thing PKCE exists to prevent.
- **A failure to read randomness is now an error instead of a silent constant.**
  If `/dev/urandom` could not be read the buffer stayed all zeroes and the
  function returned the same string every time — as both the anti-CSRF `state`
  and the PKCE verifier, with nothing logged. Sign-in now fails and says so.
- **`accounts.toml`, `privacy.toml` and `sidebar.toml` are created `0600`**
  rather than written with the umask's permissions and tightened a moment later.
- **New: notifications can leave out the sender and subject.** GNOME draws
  notifications on the lock screen, and the only control was on/off.
  **Preferences → Mail → Show sender and subject.** On by default.
- Added a `SECURITY.md` with a documented private reporting channel.

## 1.12.7 — 2026-08-23
- **New: sender logos** (#30, asked for by [@doodoobug-dot](https://github.com/doodoobug-dot)). The sender circle can carry the brand's own icon instead of coloured initials, so mail from Capital One, US Bank, GitHub or Amazon is recognisable at a glance. **Preferences → Privacy → Show sender logos.**
- No third-party service and no bundled logo database: the icon comes from the sender's own domain, best first — `apple-touch-icon.png` (180px, what a site publishes when it cares how it looks as an icon), then `favicon.ico`, each tried bare and at `www.`. The domain is the registrable one, so `usbank@notifications.usbank.com` asks `usbank.com`, with three labels kept for country-code pairs like `bbc.co.uk`.
- **Off by default**, because the request tells that domain your IP address — which is what blocking remote content otherwise avoids. The switch says so rather than burying it.
- A Gravatar still wins where one exists: it belongs to the person, not their employer. Each domain is asked at most once per session and misses are remembered, so a sender with no icon costs one request rather than one per row. `.ico` files decode through GdkPixbuf, since `GdkTexture` reads only PNG and JPEG.
- Responses are checked by content type before being read: some sites answer *every* icon path with their home page under a 200 — `pm.me` sends 347KB of HTML for `/favicon.ico` — and downloading that to discover it isn't an icon is a third of a megabyte wasted per attempt. Senders with no icon anywhere keep their initials.

## 1.12.6 — 2026-08-23
- **Fixed: dates and times ignored the system format** (#32, reported by [@edisso999](https://github.com/edisso999)). Every date went through chrono with a hard-coded pattern, and chrono has no notion of a locale, so a machine set to German still showed `Aug 23, 2026 at 5:40 AM`. Formatting now goes through GLib, which reads `LC_TIME`, and learns three things from the locale itself: its **field order** (by asking it to write 25 December 2026 and seeing which number comes first — day-first, month-first, or year-first as Japanese and Swedish are), its **clock** (by asking it to write one in the afternoon: a result containing "13" means 24-hour), and the **separator** a day-first locale uses, since German writes "23. Aug" where British and French write "23 Aug". German now reads `23. Aug 2026 at 05:40`; American English is unchanged.
- Vireo keeps its own arrangement rather than adopting the locale's written date (`%x`, which is all digits): the complaint was that the order and the clock were American, not that the month should stop being spelled, and a spelled month is quicker to place when scanning a list.
- **New: the format can be set independently of the system**, in **Preferences → Date and Time**. *Date format* offers Follow system, `Aug 23, 2026`, `23 Aug 2026` and `2026 Aug 23`; *Clock* offers Follow system, 12-hour and 24-hour. Both follow the system by default. Changing either rebuilds the list and re-renders the open message, so it takes effect at once — and it reaches everywhere a date appears, including the printed page.

## 1.12.5 — 2026-08-23
- **Fixed: some message previews showed MIME machinery instead of the message.** Previews fetch `BODY.PEEK[1]` — MIME section 1 — on the assumption that it holds the text. When the first part is itself a multipart, as in the `mixed(alternative(text, html), attachment)` layout ProtonMail sends, section 1 *is* the nested container, and the preview read `--b2=_cipkIEq1…`: its boundary. The preview builder now descends into a multipart, prefers `text/plain` over `text/html` as the reader does, and decodes each part by the `Content-Transfer-Encoding` it declares rather than guessing base64 from the bytes. A message that merely opens with `--`, as a signature does, is still read as text.
- **Fixed: previews of marketing mail showed a tracking URL instead of the greeting.** A plain-text alternative generated from HTML renders links as `text ( url )`, so a message whose first element is a linked logo begins with a bare URL — which was the whole preview. Rendered links are now dropped, keeping the words around them, along with any URL-only lines left at the top. Brackets that are not links are untouched, and a message that is only a link still shows it rather than nothing.
- **Fixed: the background backfill erased previews.** It re-fetches summaries without asking for a body slice, and the cache wrote rows with `INSERT OR REPLACE`, so every message a backfill pass touched lost its preview. Writes are now an upsert that will not let an empty preview overwrite a stored one.
- Previews already cached from the two bugs above are cleared when the cache opens; those rows show nothing until their folder syncs again, which beats showing a boundary or a tracking link.

## 1.12.4 — 2026-08-23
- **The message list can be dragged much narrower.** With the sender circles off (1.12.3) the list still refused to go below 340px, and the messages were never the reason. Three things above them were holding the pane open: the bulk-action bar, whose `SlideDown` revealer reserves its child's full **width** even while collapsed, so seven buttons set the pane's minimum whether or not anything was selected; the search scope drop-down, sized to its widest entry ("All folders"); and the folder title and "N selected" label, neither of which could ellipsize. The bar now sits in a scroller with no minimum of its own, the drop-down's button label ellipsizes (the list still spells both choices out), and both labels give way. The pane's floor drops from **340px to 171px**.
- With the circles off the rows themselves now reach **90px**: the Actions Palette line stops reserving its 260px — turning the circles off is a request for a narrow list, and that reservation was the only thing in the way — and the date, the one item on the sender line with no give, ellipsizes as well. Below the palette's own width it is clipped by the row rather than pushing the pane wider, and the row's content is clipped so nothing paints across the divider into the reader. Circles on is unchanged: floor still 350px, palette space still reserved.
- The list still **opens** at 350px rather than at its minimum, which is a fine width to be able to drag down to and a poor one to be handed on startup.
- **Fixed: squeezing the window pushed the reader's toolbar icons off the right-hand edge.** The pane was allowed to be allocated below its own minimum, so its header kept being given less room than its buttons needed. The reader now has a floor of 535px. The window's own minimum becomes 848px as a result — still half of a 1920px display, but wider than before.

## 1.12.3 — 2026-08-22
- **New: Sender circles can be turned off** (#29, reported by [@taprobane99](https://github.com/taprobane99)). The coloured circle of initials beside every message costs horizontal room that a small screen would rather give to the sender and subject. **Preferences → Message List → Sender circles** hides it in the message list, above the open message, and in popped-out message windows — the reader too, since a setting that applied to one and not the other would just look like a bug. On by default; the rows are rebuilt as the switch moves, so it takes effect at once without losing your place in the list.
- The circle is hidden rather than faded, so the row gives up its slot and the width is actually reclaimed. With the circle gone the unread dot leads the row, so it gets an equal gap on each side instead of keeping the wider inset that had been holding the circle clear of the list's edge.
- **Gravatar fetching stops while the circles are hidden.** It is wasted work, and it would send a hash of each sender's address to a third party to fill in a circle that is never drawn.

## 1.12.2 — 2026-08-22
- **Fixed: deleting a message could throw the selection to the top of the folder** (#19). Two things decide what is selected after a delete: the list picks the row that slides into the deleted one's place, and then the folder sync that follows checks whether the reader's message is still there. That second check advanced to whatever now occupies the message's old slot, found by looking it up in the cached copy of the folder — but deleting prunes the cache first, so the lookup failed on exactly that path, and the fallback was `messages.first()`: the top of the folder. The selection went there and the list scrolled with it. There is no sensible message to advance to when the old slot is unknown, so the reader now clears instead.
- **Focus follows the row the list advances to.** Removing the focused row left GTK to choose its own replacement, and moving focus drags the viewport with it. Taking focus deliberately also means the single-key shortcuts carry on from the row that is now selected rather than from wherever focus landed.
- The message selected after a delete is still the one **below** — the direction Thunderbird and Apple Mail take, so deleting a run of mail keeps moving one way.

## 1.12.1 — 2026-08-22
- **Fixed: printed pages and saved PDFs carried scrollbars, and long messages were cut off** (#16). The reader wraps each message in a sandboxed iframe — right on screen, since an email's CSS cannot escape it — but a print engine does not paginate what is inside a frame: it draws the frame as a box at its on-screen size, scrollbars and all, and clips the rest. So a message came out with a grey bar down the right edge, another under the text, and everything past the first screenful missing.
- **Printing now builds a document of its own, with no frames**: the header, then every message inlined into the page, so it flows across as many pages as it needs. Long URLs wrap instead of running off the sheet, and wide content — images, tables, `pre` blocks — is scaled to the page rather than clipped. **Ctrl+P** renders that same document offscreen and prints it, so the print dialog and the preview can no longer disagree.
- A printed **conversation** now names each message: inlining removed the per-message frame headers, so every message in a thread prints its sender and date above it.
- Inlining also gives up the iframe's CSS isolation, so each message's own `html`/`body` rules are redirected to the block that message prints in — otherwise one sender's `body{font-family:monospace}` would restyle the header and the rest of the thread. Bare type selectors (`p`, `a`) can still reach across, which is the remaining price of printing a thread as one page.

## 1.12.0 — 2026-08-22
- **New: printing** (#16). There was no way to print a message at all. The reader is a WebKit view, so it prints what it is already showing — the message as rendered, quoting, inline images and current theme — through the system print dialog, which inside Flatpak is the portal's, so printers configured for the desktop work with no extra permissions. **Ctrl+P**, **Main Menu → Print Message…**, or the printer button in the reader toolbar; **Ctrl+P** also works in a popped-out message window.
- **The printed page carries the message's identity, not just its body.** WebKit prints the document it is showing, and everything naming the mail — sender, recipients, date — lives in the GTK pane above it, which cannot be printed. Those facts now go into the document and are hidden with `@media`, so the screen is unchanged and paper gains a header: subject, From, To, Cc and the full date. Empty fields are omitted, and the subject is escaped — it is text, not markup. Printing is forced light in the wrapper and inside each message frame; reading in dark mode would otherwise put white text on a black page.
- **New: a print preview inside Vireo** — the toolbar's printer button, **Ctrl+Shift+P**, or **Main Menu → Print Preview…**. It shows the message with its print styling on a page-shaped sheet, with **Print…** and **Save as PDF…** in its header bar. The preview renders the same `document_html` the reader builds and prints the very view on screen, so it cannot drift from what comes out.
- The preview is Vireo's own window rather than an exported PDF opened elsewhere. That route — a temporary file, a URI, the document portal, and whatever the desktop registers for `application/pdf` — has four links that can each fail without saying anything, and two of them duly did: the file printer's name is translated (so asking for "Print to File" fails), and a URI built with `format!` breaks on the spaces and brackets that mail subjects put in filenames. **Save as PDF…** keeps the two lessons: ask GTK for a virtual printer that accepts PDF, and take the URI from GIO.
- Printing uses **GtkPrintDialog**, not WebKit's `run_dialog`. The latter spins a nested main loop, and polling a glib future inside one aborts the process outright — which it did, the first time this shipped as a test build. This raises the gtk4 requirement to **4.14** for building from source; the GNOME 50 runtime the Flatpak builds against has 4.20.
- The ARM64 build was again compiled with `rust-nightly`: Flathub is still serving a 404 for an object of `org.freedesktop.Sdk.Extension.rust-stable/aarch64/25.08`.

## 1.11.0 — 2026-08-22
- **New: Vireo can keep running when its window is closed** (#3). The request was for a system tray; GNOME has no tray, and its equivalent is the **Background Apps** section of Quick Settings, which xdg-desktop-portal populates from sandboxed apps running without a window. Staying alive after the last window closes is therefore all it takes to appear there — no icon to draw, no shell extension. **Off by default**, since closing a window is expected to quit: turn on *Keep running in the background* in Preferences → Mail, after which closing hides the window and mail keeps arriving and notifying.
- The portal permission is requested at the moment the switch is moved, so GNOME's "Allow Vireo to run in the background?" dialog appears while the user is looking at the setting rather than at some later unexplained point. `SetStatus` puts the unread count beside the entry ("3 unread messages", or "Checking for new mail"), so a process with no window says what it is there for.
- **New: Start at login**, a second switch enabled only when background running is. The portal's autostart entry runs `vireo --hidden`, a new flag that starts the app without presenting its window — it builds as usual, syncs, notifies, and waits in the system menu. The flag is stripped before the arguments reach GTK, which would otherwise reject it as unknown, and the activation handler that re-presents a hidden window skips exactly one activation on such a run: that first activation *is* the launch, and presenting there would undo the point of it.
- **New: a `quit` action on the application** (main menu, and Ctrl+Q). GNOME's Background Apps menu quits an app by activating this over D-Bus and only resorts to `flatpak kill` if nothing answers within five seconds, so without it the ✕ there would be a hard kill. Activating Vireo — its icon, a notification, the autostart entry — brings a hidden window back rather than doing nothing.
- Hiding rather than closing also sidesteps the reason that handler exits outright: nothing is torn down, so there is nothing to abort in GTK, WebKit or the per-account worker threads.
- The ARM64 build was again compiled with `rust-nightly`; Flathub is still 404ing an object of `org.freedesktop.Sdk.Extension.rust-stable/aarch64/25.08`.

## 1.10.3 — 2026-08-22
- **Fixed: an IMAP/SMTP account imported from GNOME Online Accounts could fail to authenticate** (#17), while a Gmail one — which uses a token rather than a password — was fine. The password was read exactly once, during import, and never again, so an import that came back empty left an account permanently unable to log in. Vireo also never asked GOA to `EnsureCredentials` first, which is how GOA is told to unlock the keyring or refresh a credential before handing it over; Geary does, which fits the report that Geary worked on the same account.
- `EnsureCredentials` now runs before the read, and a GOA account with no stored password asks GOA again when its worker connects, storing what comes back so a later run works even if GOA is slow to start. That repairs accounts already imported in the broken state — which matters more since 1.10.2, where the password field is greyed out for GOA accounts and typing it in by hand is no longer possible.
- The credential id is no longer assumed. GOA's mail provider files these under `imap-password` and `smtp-password`, other providers use a plain `password`, and some builds return the account's secret whatever id they are given; all three are tried, and a separate SMTP password falls back to the incoming one rather than sending none. If GOA still has nothing, the error says so and points at Settings → Online Accounts instead of surfacing a bare authentication failure.
- The ARM64 build was again compiled with `rust-nightly` — Flathub is still serving a 404 for an object of `org.freedesktop.Sdk.Extension.rust-stable/aarch64/25.08`.

## 1.10.2 — 2026-08-22
- **Accounts imported from GNOME Online Accounts can no longer be edited in Vireo.** Their address, servers, protocol and credentials come from the system, but the editor let them be typed over — and anything changed that way was either overwritten the next time GOA was read or left quietly disagreeing with what the rest of the desktop uses. Those fields are now insensitive, with an explanation at the top of the editor and a button that opens Settings → Online Accounts, which is where they are actually changed. What Vireo owns stays editable: the sender's display name, signature, colour, emoji and label.
- Saving such an account now restores its connection settings from the stored account rather than reading them back out of the form, so a greyed-out field can't be written back through some other route (an insensitive widget still holds and returns its value). **Test Connection stays available** — it is read-only, and confirming that the imported settings really connect is exactly what someone would want to do on that screen.
- The GOA explanation used to be split in two, with a near-identical paragraph and a second "Open Online Accounts" button in a group at the bottom of the editor. It is said once now, at the top, with the **Enabled in Vireo** switch — which hides an account locally without touching the system account — as the first thing on the page.
- The ARM64 build of this release was again compiled with `rust-nightly`: Flathub is still serving a 404 for an object of `org.freedesktop.Sdk.Extension.rust-stable/aarch64/25.08`. CI picks stable as soon as Flathub can serve it.

## 1.10.1 — 2026-08-22
- **Fixed: a message with an inline attachment showed no paperclip and no attachment** (#9). An Apple Mail PDF marked `Content-Disposition: inline` with a filename and no Content-ID was extracted correctly — that part has worked since 1.7.2 — but nothing ever asked. The paperclip is guessed before any body is fetched, from BODYSTRUCTURE or (on servers whose structure Vireo's IMAP parser rejects, notably iCloud) from the top-level `Content-Type`, and attachments are only downloaded for messages the guess flagged. A message the guess missed could never correct itself: no flag, no fetch, no attachments, indefinitely.
- `load_body` already holds the whole message for the body and the sender check, so it now also reports whether attachments are genuinely present, and the worker emits `HasAttachments` — the mirror of the existing `NoAttachments`, which has been clearing *false* paperclips from the same evidence all along. Nothing extra is fetched. The corrected flag is written to the cache so it survives a restart instead of being re-guessed, and if the message is the one on screen its files are fetched too, since the reader only requests attachments when the flag was already set. Background body prefetch runs the same check, so for recent mail the paperclip is right before the message is ever opened.
- The guess itself is deliberately unchanged. Treating a top-level `multipart/alternative` as attachment-bearing would have caught the nested Apple Mail shape at the cost of a false paperclip on nearly every HTML newsletter — the noise removed in 1.4.1. Evidence from the body is the honest answer, and small inline `cid:` decoration still earns no paperclip.
- The ARM64 build of this release was compiled with `rust-nightly`, for the reason given under 1.10.0: Flathub is still serving a 404 for an object of `org.freedesktop.Sdk.Extension.rust-stable/aarch64/25.08`. The CI job picks stable whenever Flathub can serve it.

## 1.10.0 — 2026-08-21

- **New: an Outbox** (#15). A send that fails no longer disappears. The composer has already closed by the time SMTP reports a failure, so anything not kept at that moment was lost; failures are now stored in the cache as the built MIME plus the SMTP envelope, and retried automatically as soon as a connection is back — plus by hand, per message or all at once. A queued message can be opened, edited and sent again; the edited version replaces the queued original rather than joining it. The Outbox appears as an ordinary folder, using the same message list and reader as any other, with a sidebar row that exists only while something is waiting. The envelope is stored separately and verbatim because `Bcc` exists nowhere else — lettre strips it from the bytes that go on the wire, so rebuilding recipients from the headers would silently drop those people. A message sent by a background retry now posts a notification: the last thing the user was told is that it had *failed*, and it going out in silence is not an improvement.
- The **"Send failed: Invalid input"** in that report was the address-parsing bug already fixed in 1.9.0. Sending with attachments now has regression tests (including a filename with a comma and a display name needing quotes), and a failed attachment read names the file — "No such file or directory" alone doesn't say which one, which matters under Flatpak where the portal's paths expire.
- **New: message previews** (#6). `Message.preview` was set to the empty string in every code path that built a summary — the preview line had never existed. The sync fetch now asks for `BODY.PEEK[1]<0.2048>` alongside the summary: section 1 is the first body part of a multipart message (text/plain in the usual layouts) and the whole body of a single-part one, so a single query covers both with no extra round trip per message. Preferences → Message List chooses **Off, 1, 2 or 3 lines**; Off also stops the fetch, since not downloading a slice of every message is half the point of turning it off.
- Those bytes arrive still transfer-encoded, and the header that declares the encoding isn't part of the response, so it is inferred from the bytes: base64 (which would otherwise show as gibberish), quoted-printable, or plain. A 2KB fetch almost never lands on a base64 group boundary, so the incomplete tail is dropped rather than decoded into noise, and a `=` escape cut in half stays literal. HTML parts become text through the same path replies already use, and `>` quoted lines are skipped so the snippet describes *this* message.
- **New: single-key shortcuts** (#5), Gmail-compatible where Gmail and the request agree, **off by default** as they are in Gmail and Geary — a stray keystroke shouldn't archive mail for someone who never asked for it. Enable them in Preferences → Message List; press **Ctrl+?** (or F1, or Main Menu → Keyboard Shortcuts) for the list, which closes on the same key or Escape.

  | Move | | Act on a message | | Everything else | |
  | --- | --- | --- | --- | --- | --- |
  | `j` `↓` | Next message | `r` | Reply | `c` | Compose |
  | `k` `↑` | Previous message | `R` | Reply to all | `Esc` | Back out of a reply |
  | `l` `→` | Open the selected message | `f` | Forward | `?` | Shortcut reference |
  | `h` `←` `u` | Back to the message list | `a` | Archive | | |
  | `w` | Next message in the conversation | `d` | Delete | | |
  | `b` | Previous message in the conversation | `!` | Mark as spam | | |
  | `/` | Search | `s` | Star or unstar | | |
  | | | `m` | Mark read or unread | | |
  | | | `x` | Select this row | | |

- The shortcut handler sits on the window in the *bubble* phase, so whatever has focus always gets first refusal — typing "archive" into the search field types it, and the composer keeps every letter. A guard covers widgets that handle keys without consuming them, chiefly the reader's web view. **Escape** backs out of a reply, forward or compose and returns to the list whether or not single-key shortcuts are enabled; in a search field it still belongs to the field.
- **Fixed: Gmail's non-ASCII folders showed as `&XfJSoGYfaAc-`** (#1). IMAP names mailboxes in modified UTF-7 (RFC 3501 §5.1.3) and nothing decoded them. New `src/mutf7.rs` implements the codec with no new dependency — a modified BASE64 of UTF-16BE where `,` replaces `/` so the hierarchy delimiter stays usable. Only display is decoded: the encoded string is the mailbox's real name, the one SELECT and APPEND must be given, so paths are still stored and sent exactly as the server states them. Encoding was missing too, so creating a folder named 测试 sent raw UTF-8; that now works, surrogate pairs included. Anything that isn't valid modified UTF-7 — a server ignoring the rule, or one speaking `UTF8=ACCEPT` — passes through untouched.
- The message row was rebuilt around the preview. The Actions Palette moved to a line of its own below the text, with the ⋯ button on the left, so nothing overlaps or reflows; rows size to their content rather than a fixed height; and the list reserves the palette's width up front, so opening it for the first time no longer shoves the whole pane wider under the pointer.
- Cached folder names from before this release correct themselves on the next folder list, a second after connecting. No cache re-sync: the Outbox arrives as a new table, and the preview column is added in place.
- **The ARM64 build of this release was compiled with `rust-nightly` rather than `rust-stable`.** Flathub's copy of `org.freedesktop.Sdk.Extension.rust-stable/aarch64/25.08` was serving 404s for one of its objects for hours — the ref was listed but not downloadable, from CI and from a workstation alike — while the nightly extension was intact. Rather than hold the ARM release behind someone else's outage, the build fell back to nightly; the x86_64 build is stable-compiled as always. The CI job now picks stable when it can and nightly only when it must, so this reverts by itself.

## 1.9.2 — 2026-08-19
- **ARM64 builds.** The Flatpak repo now carries `aarch64` alongside `x86_64`, so Raspberry Pi 4/5, Snapdragon X Elite laptops and ARM virtual machines install the same way everyone else does — `flatpak install --from …co.hyprlab.Vireo.flatpakref` resolves the architecture itself. Until now an ARM machine got the x86_64 build and failed at startup with `bwrap: execvp ldconfig: Exec format error` (#4). Both architectures are signed with the same key, so `flatpak update` verifies identically on either.
- Releases carry a standalone bundle per architecture: `Vireo-x86_64.flatpak` and `Vireo-aarch64.flatpak`. A bundle holds one architecture by design; the repo holds both, which is why the install command is the recommended route. The Fedora RPM stays x86_64-only.
- Vireo is developed and released from an x86_64 machine, so the ARM64 build is made natively on GitHub's `ubuntu-24.04-arm` runners (`.github/workflows/build-arm64.yml`) — emulating it locally through qemu-user takes hours per release rather than minutes. CI signs nothing: it uploads a plain OSTree repo, which `tools/import-arm64.sh` re-commits into the signed distribution repo under the project's key. The signing key never leaves the maintainer's machine.
- New `tools/local-manifest.sh`, which rewrites the Flatpak manifest's pinned GitHub source to build the working tree instead. The ARM job needs it because the manifest's pin necessarily lands in a commit *after* the release tag, so building the manifest as it exists at the tag would ship the previous version. Local test builds use it too.
- The About window's **Changelog** and **Release Notes** pages render their Markdown properly. Both were fed through a converter that knew only `#`, `##` and `- `, so every `**bold**`, backtick and `[link](url)` was displayed verbatim, and a long entry wrapped back underneath its own bullet because the whole document was a single label. Each block is now its own widget — bullets keep the marker in a separate column so wrapped lines align with the text, headings carry their own spacing, and inline emphasis, code spans and links become Pango markup, with links opening through the app's URI handler so they work inside the sandbox.

## 1.9.1 — 2026-08-19
- Records **[Chris Pouliot](https://github.com/chrispouliot)**'s authorship of the Proton Bridge work in a form GitHub can resolve. 1.9.0 credited him with a `Co-Authored-By:` trailer carrying the address from his own commit on #13, `chrispouliot@icloud.com` — which isn't verified on any GitHub account, so neither his commit nor the trailer could be matched to his profile and he never appeared as a contributor. This release's commit repeats that co-authorship using his `users.noreply.github.com` address, which always resolves. The 1.9.0 commit itself is left alone: `v1.9.0` is tagged, built and published, and rewriting it to fix a display detail would invalidate a shipped release.
- The About window's "Thanks" rows now show each contributor's GitHub handle alongside what they contributed, rather than hiding it in the row's link.

## 1.9.0 — 2026-08-19

First release with code from outside Hyprlab. Thanks to **[Alfonso Lizárraga](https://github.com/alfonsolzrg)** (#14) and **[Chris Pouliot](https://github.com/chrispouliot)** (#13), whose pull requests are the basis of most of what follows.

- **Fixed sending to a named recipient** (from #14). Every mailbox is now built from its parts instead of formatting `Name <addr>` and parsing that string back. An RFC 5322 display name only survives that round trip when it is a bare atom, so a name carrying an accent, a comma or a full stop — or one that is simply the address again, which is what an import with no separate display name produces — failed to parse and the send was rejected with "Invalid param". The pull request fixed `From:`; `To:`, `Cc:` and `Bcc:` went through the identical path and are now built the same way, through the existing `parse_recipients`.
- **Proton Bridge and other local mail bridges now connect** (from #13). Bridge broke two assumptions at once: it speaks STARTTLS rather than TLS from the first byte (`wrong version number`), and it presents a certificate generated on the machine at install time, signed by no CA and issued for an address rather than a name (`self-signed certificate`). IMAP now opens in plaintext and upgrades with STARTTLS when the port is 143 or when the host is this machine on any port but 993, so a bridge moved off its default port still works. Certificate and hostname verification is relaxed for loopback addresses only — where anyone able to intercept the connection is already running code as the user — covering `localhost`, `::1` and all of 127/8, and applied to SMTP and POP3 as well so a bridge is configured the same way throughout. TLS is still required; only the checks against a CA are dropped. The submitted patch keyed on literal `127.0.0.1` with Bridge's two default ports and routed *every* non-993 port through STARTTLS, which would have broken implicit TLS on custom remote ports.
- **A synced account no longer opens to an empty message list** (from #14). The worker awaited the background backfill's IMAP handshake before it would look at its request queue, but the first thing the UI asks for at startup is the visible folder — which the cache answers with no network at all. An incoming request now preempts that connect, and if the connection comes back offline the worker waits for a request instead of spinning on reconnects.
- **The message list rebuilds without cloning the folder** (from #14). Filtering and sorting now work on references and only the page actually rendered is cloned. Every rebuild — each keystroke in search, the cache-backed load at startup — previously copied the entire folder index to then discard all but `render_limit` of it.
- **The unread dot keeps its place.** Hiding it surrendered its slot in the row, so a read message's sender and preview shifted 18px left and the column jittered as mail was read. The dot is now always allocated and only its ink fades. Row spacing was rebalanced with it: 16px from the list's edge to the avatar, 8px between the avatar, the dot and the text.
- **Compose moved to the message-list header**, immediately left of the notification bell, so it sits above the list it adds to rather than above the folder tree. With the sidebar collapsed to its icon rail, the main-menu button is centred and back at full size — it was shrunk to 20px only so it could share that header with Compose.
- **New Preferences switch for the sidebar's Attachments row** (from #14), on by default.
- The sender-authentication lightbulb, the About window's new "Thanks" list, and `accounts.toml.example`'s Proton Bridge stanza round out the release. GOA's `GetAccessToken` failures are now logged with their D-Bus reason instead of being discarded (from #14).
- No cache bump: nothing about body rendering or the stored verdicts changed.

## 1.8.1 — 2026-08-13
- The sender-authentication lightbulb no longer appears and disappears with the verdict. It is always in the reader toolbar and, until a verdict for the open message has arrived, sits insensitive and greyed out like Reply, Archive and the rest — so the icon row never shifts position under the pointer. `set_visible` became `set_sensitive` on the badge's `MenuButton` (`src/app.rs`), which also means the details popover can't be opened while there's nothing to show.
- The verdict tint (`trust-pass`/`trust-suspicious`/`trust-fail`/`trust-unverified`) is now applied only once a verdict exists, via a new `App::sender_badge_classes`. `trust-unverified` carries `opacity: 0.55`, which would otherwise have stacked on top of GTK's insensitive dimming and left the lightbulb visibly fainter than its neighbours. With no verdict the tooltip reads "Sender authentication".
- No cache bump: nothing about body rendering or the stored verdicts changed.

## 1.8.0 — 2026-08-12
- **New: sender authentication.** A lightbulb badge in the reader toolbar (right of View Source) reports whether a message's `From:` address was actually forged, in four states — verified, not verified, check this sender, possible forgery. Colour carries the verdict, the tooltip names it, and clicking opens the evidence: DMARC/DKIM/SPF results, the signing domain, which authority reported each verdict, and any reply-to, bounce or display-name domain that doesn't match. A suspicious or failed verdict also raises a banner across the top of the message.
- The check (`src/verify.rs`) reads back the SPF/DKIM/DMARC results the receiving provider recorded in `Authentication-Results`. It costs no extra network traffic: `load_body` already fetches the whole message, so the verdict is computed from bytes we had.
- Providers lay those headers out differently, and getting this wrong made the feature useless before it was right. Gmail packs every method into one header; **iCloud emits one header per method** (`dmarc.icloud.com`, `dkim-verifier.icloud.com`, `spf.icloud.com`) and leads with BIMI, which carries a `header.d` but no authentication result. Reading only the topmost header — the obvious reading of "trace headers are prepended, so the first is ours" — reported every iCloud message as unverified and credited BIMI's domain as the DKIM signer. All `Authentication-Results` headers are now scanned in order, keeping the first verdict per method (so the provider's still beats anything a sender ships further down), and `header.d` counts only from a header that reported DKIM.
- Only a **DMARC** failure claims forgery. DMARC is the one check that verifies alignment with the `From:` domain; DKIM breaking while SPF passes is routine for mail relayed through bulk senders, and an earlier rule that failed on it flagged legitimate billing mail from Toyota and T-Mobile as forgeries. Crying wolf teaches users to ignore the badge, so that shape now reads "not verified" with the failure visible in the details. Both real-world cases are regression tests.
- A verdict is delivered on **every** path that delivers a body — cache hit, network fetch, body prefetch and attachment prefetch — and held in an app-side `sender_cache`. Opening a message usually renders from the in-memory body cache without asking the worker for anything, so a verdict computed minutes earlier had to be remembered or the badge stayed blank. `Show` clears the outgoing message's verdict, so the stored one is re-asserted after it, not before.
- **New: link destinations.** Hovering a link in a message shows its full target on a plaque in the bottom-left of the body, browser-style. The reader has carried a GTK tooltip for this since 1.0.0, but WebKit handles motion events itself, so GTK's hover timer often never starts and the tooltip never appeared. Since WebKit also reports the link's visible text, text claiming a different site than it points at is called out inline: `https://evil.example/login ⚠ looks like "paypal.com" but goes to evil.example`. Subdomains count as the same site, `mailto:` is ignored, and `https://paypal.com@evil.example/` resolves to `evil.example` — the userinfo trick doesn't fool it.
- Cache `SCHEMA_VERSION` → 11, adding a `sender_checks` table. Dropping `bodies` alongside it means every cached message gains a verdict on next open; the message index is kept, so no re-sync.
- New embedded icon `co.hyprlab.Vireo-lightbulb-symbolic` (Adwaita has none), drawn for this and bundled via `tools/gen-icon-gresource.sh`.

## 1.7.2 — 2026-08-10
- Right-clicking an image in a message and choosing **Save Image As…** now opens a save dialog. WebKit's stock item routes the image through a `WebKitDownload`, which needs a network-session `decide-destination` handler to choose a file — there wasn't one, so the item silently did nothing. Every image the reader draws inline is a `data:` URI whose bytes are already in the document, so `MessageView` now replaces that item (`connect_context_menu`) with one that decodes the URI in-process and opens a `gtk::FileDialog`, reusing the attachment drawer's save path. The item keeps its position in the menu. Remote (http) images still get WebKit's original item, which remains a no-op — resolving that needs the download handler.
- The save dialog pre-fills `image.<ext>` derived from the MIME type: a `data:` URI carries no filename. The correctly named copy is in the attachment list (below).
- Inline `cid:` images of **64 KiB or more now count as attachments** — they show the paperclip, appear in the attachment drawer under their real filename, and feed the gallery. `extract_attachments` and `structure_has_attachment` previously skipped every part carrying a Content-ID, which is what keeps newsletters from showing a paperclip for their logo, spacers and social icons (the false-attachment noise fixed in 1.4.1) — but a photo someone emails you arrives in exactly the same `multipart/related` shape, so that rule threw out real content with the decoration. Size is the only honest discriminator; 64 KiB sits well above logos and icons and well below any photo worth keeping. Both paths share the threshold so the paperclip and the drawer agree, and the BODYSTRUCTURE path scales it by 4/3 since IMAP reports the base64-encoded size.
- No cache bump: body HTML is unaffected. The `has_attachment` flag on already-synced messages corrects itself on the next folder sync, since freshly fetched summaries overwrite cached ones in `merge_index`.

## 1.7.1 — 2026-08-10
- Fixed inline images referenced by `cid:` not rendering — Gmail's `multipart/related` photo mail showed the image's filename (its `alt` text) where the picture should be. `extract_body` passed such HTML through untouched and nothing resolved the URL: `cid:` names another MIME part of the same message (RFC 2392) and WebKit has no handler for the scheme, so the `<img>` simply failed to load. Because `extract_attachments` deliberately skips parts carrying a Content-ID (they're meant to be rendered in place), the picture wasn't reachable from the attachment list either.
- `inline_cid_images` (worker.rs) now rewrites each `cid:` reference to a `data:` URI built from the part it names, in both the lone-HTML-part fast path and the composed multi-part path. The bytes arrived with the message, so there's no network fetch and no CSP change — `data:` was already permitted while remote content is blocked. Matching is case-insensitive, tolerates angle brackets, and percent-decodes the reference (Gmail Content-IDs often contain an `@`, sometimes written `%40`).
- Only image parts are resolved (reusing `image_mime`'s subtype validation so nothing can break out of the `data:` URI), each is charged once against the existing 16 MB inline-image budget however many times it's referenced, and unresolvable references are left verbatim rather than guessed at. Rewrites happen only in resource position (`src=`, `url(`) — never in prose, and never in an `href`, where a click hands the URL to the external browser. Parts rendered in place are no longer also appended as standalone images by the multi-body path.
- Cache `SCHEMA_VERSION` → 10: bodies are cached as rendered HTML and served without re-fetching, so already-broken copies are dropped and re-rendered on first open. Only the `bodies` table is dropped; the message index survives, so no re-sync.
- **Discontinued the Arch, Debian/Ubuntu and Snap packages** (added in 1.5.1 and 1.7.0). Releases now carry the Flatpak — repo, plus a standalone `.flatpak` bundle — and the Fedora RPM only. Each dropped package needed its own container image and a full from-source compile per release for a distribution the Flatpak already covers; `packaging/{arch,debian,snap}/` are removed and `tools/build-packages.sh` builds just the RPM. Existing installs keep working but won't see new versions — switch to the Flatpak (`flatpak install --from https://vireo.hyprlab.co/flatpak/co.hyprlab.Vireo.flatpakref`).

## 1.7.0 — 2026-08-05
- New **Debian/Ubuntu package** (`vireo_<ver>-1_amd64.deb`), published on each GitHub release. Built from source with `dpkg-buildpackage` in an Ubuntu 24.04 container (`packaging/debian/`), so `Depends:` are computed from the real linked libraries (dpkg-shlibdeps); targets Ubuntu 24.04+/Debian 13+. Uses a rustup toolchain because noble's rustc is older than the GTK4 crate stack's MSRV.
- New **Snap package** (`vireo_<ver>_amd64.snap`), also on each release. Strict confinement, `base: core24`, GNOME extension, with `network` and `password-manager-service` plugs; built by snapcraft in destructive mode inside the `ghcr.io/canonical/snapcraft:8_core24` container (`packaging/snap/`, SDK snaps unpacked by `prepare-sdk.sh` since the container has no snapd). Install with `snap install --dangerous ./vireo_<ver>_amd64.snap`.
- `tools/build-packages.sh` grew `deb` and `snap` subcommands (`all` now builds rpm + arch + deb + snap).

## 1.6.1 — 2026-08-03
- Flatpak reinstalls now migrate the old Veem app's data automatically. The manifest grants read-only access to the old sandbox (`--filesystem=~/.var/app/com.getveem.Veem:ro`), and on first run `migrate_flatpak_data()` (main.rs) copies `config/veem` and `cache/veem` from it into Vireo's own sandbox dirs — accounts, settings and cached mail all carry over. Copy, not rename: the legacy mount is read-only, and the old install stays untouched. Runs only under Flatpak (`/.flatpak-info` present) and only when Vireo's dirs don't exist yet; combined with the keyring fallback from 1.6.0, a Flatpak user's first launch of Vireo restores everything without re-adding accounts.

## 1.6.0 — 2026-08-03
- **Veem is now Vireo.** The app has been renamed to avoid confusion with similarly named products (Veeam Software, Veem payments). Same app, same code, new name and a new icon.
- App ID renamed `com.getveem.Veem` → `co.hyprlab.Vireo`; the binary is now `vireo`, and the GitHub repository moved to `hyprlab/vireo` (old URLs redirect). Distribution moved from getveem.com to https://vireo.hyprlab.co.
- Existing data migrates automatically on native installs: `~/.config/veem` and `~/.cache/veem` are moved to their `vireo` counterparts on first launch, and keyring entries stored under the old service name are read via a fallback and moved to the new service on first use — accounts stay signed in.
- **Flatpak installs do not carry over** (a Flatpak app's identity is its app ID): install Vireo fresh from https://vireo.hyprlab.co and remove the old Veem app. Accounts need to be re-added there (sandboxed data can't cross app IDs).
- The `VEEM_GOOGLE_CLIENT_ID`/`VEEM_GOOGLE_CLIENT_SECRET`/`VEEM_MICROSOFT_CLIENT_ID`/`VEEM_MICROSOFT_CLIENT_SECRET` build/env overrides are now `VIREO_*`.
- Embedded symbolic icons re-prefixed `co.hyprlab.Vireo-*` (gresource path `/co/hyprlab/Vireo`); `resources/veem.gresource.xml` → `resources/vireo.gresource.xml`.
- Fedora RPM and Arch package (added after 1.5.1) are named `vireo` and published on the GitHub release alongside the Flatpak.

## 1.5.1 — 2026-07-28
- A collapsed conversation now stays marked unread until every message in it is read. The thread head row carries a new aggregate `thread_unread` flag (any member unread), which keeps the unread dot and bold sender/subject visible and adds a heavier `thread-unread` accent highlight (28% vs the normal 12% for a single unread message) so unread replies hidden under a collapsed head can't be missed. Previously the head reflected only its own read state, so once the newest message was read the thread looked fully read while unread replies sat hidden beneath it.
- The flag updates in place: the message list now records rendered thread membership (message → conversation key → members) during each rebuild, and any read-state change (`MarkRead`/`SetRead`) recomputes the conversation's aggregate unread state and pushes it to the head row — so the heavy highlight clears the moment the last unread reply is read, with no list rebuild. Opening a thread still marks only the message you opened as read; hidden replies keep their unread state until individually read (expand + select, palette, context menu, or bulk actions).
- New setting: Settings → Message List → "Expand conversations by default". Off (default) keeps the existing behavior — threads start collapsed to their newest message; on renders every conversation expanded. Persisted as `threads_expanded` in `privacy.toml`, applied live. The per-thread chevron still toggles individual conversations away from whichever default is chosen (`expanded_threads` now stores exceptions to the default rather than "expanded" keys), and flipping the setting resets those per-thread toggles.

## 1.5.0 — 2026-07-19
- Reply, Reply All and Forward from the reader toolbar now open an inline compose panel that drops down over the message body (a SlideDown `gtk::Revealer` prepended into the reader pane's content box) instead of spawning a separate window. The inline panel shows only the reply body (quoted message + signature); From/To/Cc/Bcc/Subject stay hidden until the panel is expanded.
- Added an expand/collapse toggle (`view-fullscreen` / `view-restore`) to the compose header bar: inline → promote to the full compose window, windowed → collapse back into the reader. The **same** live component (and its WebKit editor) is reparented between the reader's revealer and an app-owned `adw::Window`, so the draft body, caret, selection and undo history survive the move with no reload — a `WebKitWebView` keeps its web process across a cross-toplevel reparent (verified with a spike before building). "New message", compose-to and edit-draft still open standalone windows.
- Navigating to another message while an inline reply has unsent edits auto-saves it to Drafts and closes the panel; a pristine, quote-only reply is discarded without creating a Drafts entry. Dirtiness is tracked from recipient/subject edits and an editor `input` flag (`RichEditor::is_dirty`, read via JS which also works while the pane is unrealized).
- Refactored `Compose` to be host-agnostic: its root is now an `adw::ToolbarView` (was `adw::Window`); window-coupled calls (`root.close()`, file/contacts dialog parents) resolve the live toplevel dynamically; and the app tracks composers by id (`ComposeHost` / `ReaderCompose`, `ComposeOutput::{ToggleWindow,Close}`) to host, reparent and tear them down. The inline pane uses a fixed editor height (300px, scrolls internally) with `vexpand` disabled so the panel height is deterministic regardless of reply length.

## 1.4.6 — 2026-07-19
- Changed the message-list Actions Palette toggle from a chevron to a horizontal ellipsis (⋯), the more conventional "more actions" affordance. Because the ellipsis reads the same open or closed, the icon is now static — the palette sliding open (the revealer) is the state cue — replacing the previous open/closed chevron-direction switch (`pan-start`/`pan-end`) in `src/ui/message_list.rs`. Added the `view-more-horizontal-symbolic` icon to the embedded, app-ID-prefixed icon set (sourced from Adwaita; regenerated via `tools/gen-icon-gresource.sh`).

## 1.4.5 — 2026-07-19
- Fixed sidebar unread chips not updating in real time while viewing a single account. IMAP IDLE watches one folder per account (each account idles on its own inbox after the default "All Inboxes" load), and on new mail it re-synced the message list but never re-emitted the unread *count* — so a background inbox's chip stayed stale until an explicit reload (e.g. clicking "All Inboxes"). The IDLE `NewData` handler in `worker.rs` now also runs `selected_unseen` and emits `WorkerEvent::FolderUnread`, which flows through `AppMsg::FolderUnread` → `push_unread_counts` → in-place chip update (including the "All Inboxes" total), so chips tick up on their own without a manual refresh.
- Collapsing an account's section now shows its inbox unread count on the account's avatar circle instead of hiding it. Previously the Inbox row's chip slid into the (hidden) folder-list revealer on collapse, so the count disappeared until the account was expanded again. The avatar is now wrapped in the same mini unread-badge overlay used by the collapsed "All Inboxes" rail (new `account_circle_badges` map in `sidebar.rs`), visible only while the section is collapsed and kept in sync on build, on live count updates (`SetUnread`), and on live collapse/expand toggles (`ToggleCollapseLocal`).

## 1.4.4 — 2026-07-16
- Fixed messages showing a blank date in the message list and the reader header when the sender omits (or sends an unparseable) `Date:` header. Some bulk mailers — e.g. the "Trusted Servants Pro" notifications delivered to `public@dccma.com` — emit no `Date:` line at all, so Veem derived an empty label and a `0` sort timestamp, leaving those rows dateless and sinking them to the bottom of the list.
- Veem now falls back to the IMAP `INTERNALDATE` (the server's delivery date — i.e. the date of receipt) whenever the `Date:` header is missing or fails to parse. `INTERNALDATE` was added to the four summary FETCH item lists (the structured-`ENVELOPE` and raw-header paths in both `fetch_window` and `fetch_summaries_by_uid`), and a new `internal_date_summary()` helper feeds the fallback in `build_summary` and `summary_from_headers`. Both the list row and the opened-message header key off the same `timestamp`, so populating it fixes both places and restores correct sort order. Existing cached rows self-correct on the next folder sync (summaries are written with `INSERT OR REPLACE`).

## 1.4.3 — 2026-07-15
- Fixed contact names in the contacts browser displaying in all lowercase. EDS stores each book's `full_name` column case-folded for search (e.g. `aaron arnwine`); the properly-cased name lives only in the vCard's `FN` property. The reader now selects the vCard column (`ECacheOBJ` for CardDAV caches, `vcard` for the local book) and parses `FN` — preserving the original capitalisation — via a new `vcard_display_name()` that handles line folding, property parameters, and text escapes, falling back to the email when `FN` is empty. Covered by unit tests.

## 1.4.2 — 2026-07-15
- Fixed GNOME Contacts integration under Flatpak: the contacts browser showed an empty list and the "Open GNOME Contacts" button did nothing. Both were sandbox-only (native builds were unaffected).
- Empty list: inside the sandbox `dirs::{data,cache,config}_dir()` are redirected into `~/.var/app/com.getveem.Veem/`, so the Evolution Data Server SQLite books were never found. Under Flatpak the reader now resolves the EDS address-book dirs from the real home (`~/.local/share`, `~/.cache`, `~/.config`) and opens the books with `immutable=1` (a read-only host mount can't service a WAL database otherwise). Added `--filesystem=xdg-{data,cache,config}/evolution:ro` so the caches are visible, and `read_book_db` now logs failures instead of silently returning empty. This also fixes book enumeration for "Add to Contacts".
- "Open GNOME Contacts" did nothing because it exec'd `gnome-contacts` inside the sandbox, where it doesn't exist. Under Flatpak it now D-Bus-activates the host app (`org.gnome.Contacts` → `org.freedesktop.Application.Activate`), with a `flatpak-spawn --host` fallback. Added `--talk-name=org.gnome.Contacts` and `--talk-name=org.freedesktop.Flatpak`.
- Verified in the actual sandbox: 191 contacts read (matching native), and GNOME Contacts launches on the host.

## 1.4.1 — 2026-07-14
- Fixed false attachment indicators (the paperclip) on iCloud messages that have no real attachments — typically marketing/HTML mail (e.g. TheraPlatform, Atlas Arts). iCloud sends non-compliant BODYSTRUCTURE, so Veem falls back to a header-only path that guessed "has attachment" from a top-level `Content-Type: multipart/mixed` — which is also how newsletters wrap their HTML plus inline `cid:` images, so they were wrongly flagged even though `extract_attachments` (which the drawer uses) correctly skips inline parts.
- Added `Cache::attachmentless_uids` (messages in `attachments_checked` with zero stored attachments) and reconcile the `has_attachment` flag against it in `cache::load_messages` and after a fresh server fetch (`worker::reconcile_attachment_flags`). This is robust against re-sync re-deriving the flag and can never hide a real attachment (a message is only reconciled once its full body has been extracted).
- Added a live correction: background prefetch now emits `WorkerEvent::NoAttachments` when a flagged message turns out to hold no real attachments, and the message row drops its paperclip immediately (the indicator is now `#[watch]`ed) — no refresh needed.

## 1.4.0 — 2026-07-14
- Added an in-message attachment drawer: a resizable footer beneath the reader body that shows every attachment on the open message as a wrapping grid of square (1:1) thumbnails — images as cover-cropped previews, everything else as colour-coded type icons — each with its filename beneath it. New module `src/ui/attachment_drawer.rs`.
- The drawer owns a vertical `GtkPaned` whose top pane is the reader body and bottom pane is the drawer, so the divider is a smooth native resize grip and the reader shrinks rather than the window growing. A collapse/expand chevron in the drawer header hides the grid to just the header; a size slider scales the thumbnails (thumbnail size and height do not affect each other). Only the collapsed/expanded state is remembered across launches (via `state.toml`); height defaults to 160px and thumbnails to the slider minimum each session.
- Thumbnails use a fixed-size `SquareBox` widget so images can't blow the cell out to their native pixel width; the grid flows left-to-right and wraps. Hovering a cell reveals Download/Open quick actions (matching the gallery, ~25% smaller); right-click gives an Open/Download menu; single-clicking an image opens a modal lightbox (prev/next, ←/→, Esc), and clicking a non-image opens it in its default app.
- The reader header's attachments dropdown now shows an image thumbnail (or type icon) per row and Preview / Open / Download actions. Preview reuses the drawer's lightbox and Download reuses its file chooser.
- Reused the attachments gallery's thumbnail/icon/open helpers (`texture_from`, `icon_for`, `icon_color_class`, `open_bytes`) — now `pub(crate)` — across the drawer and popover.

## 1.3.9 — 2026-07-13
- Fixed the app not pulling new mail after the system resumes from sleep. IMAP worker sessions are long-lived (one persistent connection per account, plus a parked ~29-minute IMAP IDLE when push is enabled); suspending the machine silently kills those sockets, and previously nothing detected the resume — so no new mail arrived, and even the Refresh button couldn't help because a `LoadMessages` request could sit behind a worker parked in an IDLE wait, until the app was restarted.
- Added a systemd-logind watcher (`src/power.rs`, new `power` module) that subscribes to `PrepareForSleep` on the **system** D-Bus (Flatpak-safe) and fires `AppMsg::SystemResumed` on the resume edge (`start == false`), modeled on `goa::watch_removals`. It no-ops silently if logind/the system bus is unavailable.
- On resume, Veem sends `MailRequest::Reconnect` to every worker — this drops the stale session, logs in fresh and re-arms IMAP IDLE, and also unsticks any worker parked in an IDLE wait (the request breaks its `select!` loop) — then triggers a `Refresh` to reload the visible folder and re-arms the auto-fetch timer whose monotonic countdown was frozen during sleep.

## 1.3.8 — 2026-07-12
- Added a "Keyring Setup Help" row to the About window for Linux Mint / Cinnamon users. It reopens the one-time keyring setup tip (added in 1.3.7), so anyone who dismissed it can bring it back. The row only appears on Mint/Cinnamon (gated on the same `platform::is_mint_cinnamon()` check as the tip), and activating it emits `AppMsg::ShowKeyringHelp { problem: false }`.

## 1.3.7 — 2026-07-12
- Passwords that fail to save to the system keyring no longer fail silently. Veem stores account passwords in the Secret Service (never on disk); if the keyring doesn't actually persist the password (e.g. no keyring is set up, or it's locked), the account would previously look saved but couldn't sign in after a restart. Veem now verifies the password round-trips after saving and, if not, shows a dialog explaining how to set up the keyring.
- Added Linux Mint / Cinnamon detection (`src/platform.rs`, Flatpak-aware via `/run/host/os-release`) and a one-time, dismissible setup tip shown there. It covers installing gnome-keyring + seahorse, creating a default "Login" keyring, and — crucially — how to stop the keyring prompting for an unlock password at every login (match the Login-keyring password to your user login password and avoid automatic login, or blank the keyring password to remove the prompt entirely, at the cost of at-rest encryption). The "don't show again" choice is saved in `~/.config/veem/state.toml`.

## 1.3.6 — 2026-07-12
- The symbolic icons used throughout the UI are now embedded in the binary instead of pulled from the host icon theme, so they look identical on every distribution. On some systems (e.g. Zorin) icons previously rendered differently or went missing because the local icon theme drew them its own way or lacked them entirely.
- Every icon Veem draws (59 of them) is bundled as a GResource compiled into the binary by `build.rs` and registered at startup, each renamed with a `com.getveem.Veem-` prefix so no host theme can override it. Sources: `resources/icons/` + `resources/veem.gresource.xml`; regenerate with `tools/gen-icon-gresource.sh`. GTK's own window chrome (close button, back arrows) still follows the host theme.
- No filesystem icon install is needed anymore (works the same under Flatpak); the dev-only theme search path is retained just for the app icon when running uninstalled.

## 1.3.5 — 2026-07-12
- Sidebar folders are now split per account: the essential folders (Inbox, Sent, Drafts, Archive, Junk, Trash, Starred) stay visible, while user-created folders are tucked under a collapsible "Folders (N)" section that's hidden by default. Its expanded/collapsed state is saved per account and persists between restarts. Drag-and-drop, right-click actions, and selection all work through the collapsed section.
- Also in the attachments gallery: the thumbnail hover actions now include Download and Go to Message alongside Open (Go to Message shows even for attachments that aren't cached yet).

## 1.3.4 — 2026-07-12
- The attachments gallery gained a search bar: filter by sender, subject, filename, folder, or file-type keywords (e.g. "pdf", "image", "spreadsheet"). Multiple words are matched together, and a "No matching attachments" page shows when a search comes up empty.
- Added a sort control with Newest/Oldest, Name (A–Z / Z–A), Sender (A–Z / Z–A), Largest/Smallest first, and Type (A–Z / Z–A).
- Each attachment now shows the source message's date in its meta line (and in the lightbox caption), alongside the folder and size.

## 1.3.3 — 2026-07-12
- Attachment type icons in the gallery are now colour-coded by file kind: PDFs red, Word/documents blue, spreadsheets green, presentations orange, archives amber, audio purple, video pink, calendars teal, images cyan, and everything else grey. Applied to both the grid cells and the lightbox preview icon; colours read on light and dark themes.

## 1.3.2 — 2026-07-12
- Fixed subjects that arrived as raw `=?utf-8?Q?…?=` code. Some senders (notably Mailchimp newsletters like The Marginalian) pack the whole subject into one RFC 2047 encoded-word far longer than the 75-character limit; the decoder aborted on those and left the raw text. It now decodes them, as Apple Mail and Thunderbird do. Subjects already cached this way are re-decoded in place on upgrade — no re-sync needed.
- An extreme subject can no longer push the toolbar and window controls off-screen: the reader subject now breaks mid-word for unbreakable tokens, so its minimum width stays small regardless of content.

## 1.3.1 — 2026-07-12
- The Attachments gallery now spans every folder — archived mail and mail filed in folders, not just inboxes — excluding only Trash, Spam and Drafts. Attachments in those folders are prefetched in the background so they appear without opening each message.
- Right-click an attachment for Download…, Open, and Go to Message; the menu now opens at the pointer instead of below the thumbnail. Double-click a thumbnail to open the file in its default app.
- Each thumbnail has a hover "Open" button in its bottom-right corner (for files already cached).
- The grid is now responsive: a minimum of 3 thumbnails per row that adds columns as the window widens, with each thumbnail locked to a 4:3 ratio that fills its cell (implemented via a custom height-for-width `RatioBox` widget).
- Fixed the window controls (close/minimize) disappearing while the gallery was open — the gallery page now carries its own header bar.

## 1.3.0 — 2026-07-11
- New Attachments gallery: a sidebar entry (under All Inboxes, above the accounts) that shows every attachment across your connected inboxes in a grid — image thumbnails and type icons for other files, with the sender and size.
- Clicking an attachment opens a large lightbox preview with previous/next navigation (arrow keys and Escape too), an "Open" button (opens the file in its default app), and "Go to Message" to jump to the source email.
- The gallery is built entirely from the local attachment cache, so it's instant and works offline. Files under 6 MB are preview-ready immediately; larger files are opened on demand. Capped at 300 items per inbox, newest first.

## 1.2.3 — 2026-07-11
- Add Account now starts with a single Provider dropdown that sets everything up for you. Pick your provider and the sign-in method and IMAP/SMTP servers + ports are chosen automatically.
- Password providers with auto-filled servers: iCloud, Yahoo, Proton Mail (Bridge), Fastmail, AOL, Zoho, GMX, Yandex, and Mail.com — each with a hint (e.g. app-specific password, or Proton Bridge).
- OAuth providers (Google, Microsoft/Outlook, and Custom OAuth) are in the same dropdown; selecting one shows the browser sign-in and hides the manual server fields. "Other (IMAP/POP3)…" remains for manual setup.
- Removed the separate Authentication dropdown (its options moved into the Provider list) and the non-working password-based Gmail and Outlook/Hotmail entries (those providers require OAuth now).
- Editing an account auto-selects its matching provider in the dropdown.

## 1.2.2 — 2026-07-11
- Desktop (system) notifications: Veem now posts a notification for new inbox mail and for genuine error alerts (send/auth failures) via GNotification. Notifications appear only when Veem isn't the focused window; transient connection blips that auto-recover are excluded.
- Clicking a new-mail notification raises Veem, navigates to the message's folder, and opens it (the summary notification opens the newest of a batch). Error notifications raise the window.
- A new-mail notification is withdrawn once its mail is read — by clicking the notification or opening any unread message from that account.
- Added a "Desktop notifications" toggle in Preferences → Mail (on by default), persisted to privacy.toml.

## 1.2.1 — 2026-07-11
- Bare URLs (http/https and `www.`) in plain-text message bodies are now clickable links that open in the browser. Links are only ever http(s) — a bare `www.` host is forced to `https://` — so no `javascript:`-style link can be forged; trailing sentence punctuation is trimmed while balanced parentheses in a URL are kept. Cached bodies are re-rendered on upgrade (cache `user_version` 7 → 8) so previously-read mail picks up the links.
- In the message list, Delete or Backspace now deletes the selected message(s), moving them to Trash and advancing the reader. It's scoped to the list, so Backspace still edits text in the search box.
- Right-clicking an account under "All Inboxes" now offers "Account Settings…" in its context menu.

## 1.2.0 — 2026-07-11
- Search now spans every folder of every account, not just the folder you're viewing. A scope selector beside the search box switches between "All folders" (the default) and "This folder".
- Cross-folder search runs entirely over the locally indexed messages (subject, sender, preview), so it's instant and works offline. The whole mailbox is covered once background indexing finishes; the pool is snapshotted when a search begins.
- Multi-account search results are tinted by account (as in the unified inbox) so each hit's origin is legible, and opening a result works from whichever folder it lives in.
- A search now survives a background re-sync of the folder you're viewing instead of being cleared; switching folders still clears it.

## 1.1.5 — 2026-07-11
- Reordered the reader header's message-action buttons to Archive, Delete, Spam, View Source (left to right).
- The message list now launches at its minimum width — just wide enough for a row's Actions Palette to fit — instead of a slightly-too-wide fixed value. The divider position isn't hardcoded: `shrink_start_child` is false, so GtkPaned clamps the launch position up to the pane's natural minimum, which self-adjusts to font/theme changes.

## 1.1.4 — 2026-07-10
- Fixed 1.1.3's message-body fix not applying to mail that had already been read. Bodies are cached as rendered HTML and `LoadBody` serves that cache without ever re-fetching, so any message opened under an earlier build kept its old (blank) rendering forever — including the iPhone photo mail 1.1.3 was meant to fix. The cache is now invalidated on upgrade (`user_version` 6 → 7).
- Cache upgrades that only change how bodies are rendered now drop just the derived `bodies` table instead of the whole cache, so the message index survives and no whole-mailbox re-sync is triggered.

## 1.1.3 — 2026-07-10
- Fixed photo mail from iPhones (Apple Mail) arriving as a blank message with no attachment. Apple sends photos as `Content-Disposition: inline` parts of a `multipart/mixed`, which broke two things: the reader rendered only the *first* body part — an empty text part — so the message looked blank, and attachment detection required a disposition of `attachment`, so no paperclip appeared and the image was never downloaded.
- Message bodies are now composed from every display part in order rather than just the first, and inline images are embedded as `data:` URIs, so photos render in place. Nothing is fetched from the network: the bytes arrive with the message, and remote content stays blocked as before. Embedding is capped at 16 MiB per message; larger images remain available as attachments.
- Attachment detection now counts any non-text part, except one carrying a `Content-ID` — that marks a `cid:` resource referenced from the HTML body (e.g. a newsletter logo), which is rendered in place rather than listed. The attachment list follows the same rule, so it can no longer contradict the paperclip.
- Plain-text messages now follow the app's light/dark theme instead of always rendering on white.

## 1.1.2 — 2026-07-08
- Mass delete/archive of large selections is now fast and reliable: the whole selection is moved in a single server-side operation per folder (previously one slow request per message, which could freeze the UI and silently drop moves on big mailboxes such as Gmail's All Mail), with a spinner shown over the list until it completes.
- Fixed deletes/archives being routed to the wrong folder on Gmail: destinations now prefer the real RFC 6154 SPECIAL-USE folder (e.g. `[Gmail]/Trash`) over a same-named stray label, so mail actually leaves All Mail instead of just gaining a label.
- GNOME Online Accounts are kept in sync: an account removed in GNOME Settings is now dropped from Veem automatically — both on startup and live via a D-Bus watcher — instead of lingering. Reconciliation is skipped when GOA is unreachable, so a momentary outage never wipes accounts.

## 1.1.1 — 2026-07-07
- Fixed the sidebar's unread count chips: they no longer revert to a stale number when the sidebar is collapsed/expanded (in-place unread updates are now persisted, not just applied to the visible label), and empty inboxes report the correct count as soon as an account connects — the inbox count now comes from an accurate SEARCH UNSEEN instead of the STATUS count some servers (e.g. iCloud) report unreliably.

## 1.1.0 — 2026-07-06
- Compact (icon-only) sidebar improvements: inboxes now show unread count chips on their icons, the per-account inboxes under "All Inboxes" stay visible (with a button to expand/collapse them), and the collapsed rail is narrower.

## 1.0.9 — 2026-07-05
- Added a Changelog section to the About window, backed by this file so the version history stays in sync everywhere.

## 1.0.8 — 2026-07-05
- Quit cleanly on window close: save window state, then exit the process directly instead of running the full GTK/WebKit/worker teardown, which could abort with SIGABRT (surfaced as a crash notification under Flatpak).

## 1.0.7 — 2026-07-05
- Removed all user-facing Flathub references (About link, demo email, README/manifest/comment wording); Veem is distributed from its own signed repo, not Flathub.

## 1.0.6 — 2026-07-05
- Use "Jason M." rather than a full name in the bundled demo/sample content.

## 1.0.5 — 2026-07-05
- Renamed the application ID to `com.getveem.Veem` across the desktop file, metainfo, icons, GTK/D-Bus app ID, keyring service, and Flatpak manifest (Flathub requires an ID matching a controlled domain).

## 1.0.4 — 2026-07-05
- Manage GNOME Online Accounts from the account editor: an enable/disable toggle plus an "Open Online Accounts" button instead of Remove.
- Badge each account in the list as an "Online Account" (GOA) or "Veem" account.
- Dropped the "experimental" label from Google OAuth, which now routes through GNOME Online Accounts.

## 1.0.3 — 2026-07-05
- Guide Google sign-in to GNOME Online Accounts from the account editor when no OAuth client is configured.

## 1.0.2 — 2026-07-05
- Open sign-in and About-window links through the XDG OpenURI portal so OAuth works inside the Flatpak sandbox.
- Initial Flatpak packaging.

## 1.0.1 — 2026-07-04
- Built-in Microsoft OAuth client; Google client injected at build time (later replaced by GNOME Online Accounts).

## 1.0.0 — 2026-07-04
- First release: multi-account IMAP/POP with OAuth, a unified inbox, whole-mailbox sync and search, conversation threading, compose/reply/forward, and privacy-first reading (remote content blocked by default).
