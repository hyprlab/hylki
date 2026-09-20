# Credits

Hylki is maintained by [Hyprlab](https://github.com/hyprlab). This page says
what each contributor's work was; the names alone live in
[`data/CONTRIBUTORS`](../data/CONTRIBUTORS) and
[`data/TRANSLATORS`](../data/TRANSLATORS), which is what the app's About window
shows. Want to join them? See [CONTRIBUTING.md](CONTRIBUTING.md).

## Code

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
- [**Yiannis Ioannides**](https://github.com/yioannides) ([#75](https://github.com/hyprlab/hylki/pull/75),
  [#227](https://github.com/hyprlab/hylki/pull/227)) — the Greek translation; the
  `--user` flag in the Flatpak install instructions, so a local install no
  longer asks for root; and a long run of requests and design feedback that
  shaped tags, split replies, the reader's own font and colours, Empty Trash
  and the reply panel's fields.


## Reports, design and ideas

Not every contribution is code.

Thanks to
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
weighing in, all in 1.33; and to [**EmmanuelP**](https://github.com/EmmanuelP)
for the meeting-invitations request and
[**yioannides**](https://github.com/yioannides) for the repository cleanup
([#230](https://github.com/hyprlab/hylki/issues/230)) this documentation
follows, both in 1.36; and to everyone who files issues and ideas.

## Translations

Every language Hylki speaks was given to it by somebody:

- **French** — [frenchy82](https://github.com/frenchy82)
- **Greek** — [Yiannis Ioannides](https://github.com/yioannides), with the bulk
  of one round's strings generated by [somePaulo](https://github.com/somepaulo)
- **Hungarian** — [Laszlo Lang](https://github.com/7system7)
- **Portuguese (Portugal and Brazil)** — [Paulo Fino](https://github.com/somepaulo)
- **Russian** — [Ilya Semenkovich](https://github.com/iliasen)

Adding or updating one is the easiest way in: see [po/README.md](../po/README.md).

## Packaging

- [**bennypowers**](https://github.com/bennypowers) — the Gentoo ebuild, in
  [his overlay](https://github.com/bennypowers/gentoo-overlay).
- [**tbaumann**](https://github.com/tbaumann) — the Nix flake, in
  [his fork](https://github.com/tbaumann/hylki).

## Third-party work

The brand marks, sender logos and appearance themes Hylki bundles are listed
with their sources and licences in [LICENSE.md](LICENSE.md).
