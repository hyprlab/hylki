# Bundled sender logos

The marks in this folder fill a sender's avatar in the message list when
"Show sender logos" is on (Settings → Privacy) and the sender's domain
is one of those listed in `domains.txt`. They are shipped inside the app, so
showing one makes no network request; the sender's own BIMI logo, when it
publishes one, is preferred over them, and the site's own icon is the
fallback when there is neither (see `src/logo.rs`).

**They are not covered by Hylki's licence.** Each is a trademark of its
owner, used only to identify mail from that sender, exactly as the mail
provider marks in `data/brands/` are. Hylki is not affiliated with or
endorsed by any of them, and section 7(e) of the AGPLv3 lets the project
decline to grant trademark rights, which it does.

The files come, unmodified, from two collections, pinned to the commits
recorded at the top of `logos.toml`:

| Folder | Collection | Licence of the collection |
| --- | --- | --- |
| `gilbarbara/` | [gilbarbara/logos](https://github.com/gilbarbara/logos) | MIT (the SVG files as published; the marks themselves remain their owners') |
| `simple/` | [Simple Icons](https://simpleicons.org) | CC0 1.0 (the same trademark note applies; see their [legal disclaimer](https://github.com/simple-icons/simple-icons/blob/develop/DISCLAIMER.md)) |

A Simple Icons glyph is drawn in white (or black, whichever reads) on the
brand colour the collection records; a gilbarbara mark is drawn on a white
tile, inset a little. Both are framed at render time (`logo::square_svg`),
the files are not edited.

`logos.toml` is generated: it maps each domain to its file and, for Simple
Icons, the brand colour. `../../resources/logos.gresource.xml` lists the
files `build.rs` compiles into the binary. To add a sender, add its domain
to `domains.txt` (with an explicit `gilbarbara:<file>`, `simple:<slug>` or
`brand:<id>` pick when the automatic match is wrong) and run:

```sh
tools/fetch-logos.py
```

which resolves every domain against both collections, downloads what is
new, drops what is no longer listed, and rewrites the map and the resource
list. Commit the lot. A domain neither collection carries (many banks and
retailers) is simply left out and falls back to BIMI and the site icon.
