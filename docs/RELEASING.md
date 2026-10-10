# Release notes

What a Hylki release page says, and where each part comes from (#230).

## The shape

A release page is the version's section of [RELEASE_NOTES.md](RELEASE_NOTES.md)
and a compare link, nothing else. The same section is what the app's About
window shows. It is a list to scan, not prose to read:

```
## What's new in 1.42.0

New features:

- Recipients from LDAP directories - (#307)
- Replies and forwards quote the message as it looks - (#295)
- Drag files onto the window - (#293)
- New mail notification sound - (#292)

Fixes:

- messages stuck on "Loading…" - (#296)
- plain-text messages arriving empty - (#297)
- a search of all folders mixing conversations - (#317)

Translations:

- Polish by @TomaszBojanowski
- Greek by @yioannides
```

- Three groups, in that order: `New features:`, `Fixes:`, `Translations:`.
  Leave out a group the release has nothing for.
- One line per item: name the thing, then ` - (#issue)` where there is one.
  A feature starts with a capital; a fix is the problem it fixes, in lower
  case. How it works and why belong in [CHANGELOG.md](../CHANGELOG.md).
- No bold, no headings inside the section, no paragraphs.
- No "What's changed" list: at the rate this repository commits, GitHub's
  generated list is the compare view again, only longer.

`tools/release-notes.sh` adds the last line, `Full changelog:` and the compare
link to the previous release of the same kind:

```sh
tools/release-notes.sh 1.36.0 > /tmp/notes.md
gh release create v1.36.0 --title "Hylki 1.36.0" --notes-file /tmp/notes.md ...
```

`python3 tools/check-docs.py` holds the newest section to this shape.

## Credit

An @ link on a release page is for the people whose work is in the release:
code, artwork or a translation, the handles listed in `data/CONTRIBUTORS` and
`data/TRANSLATORS`. Somebody who reported a bug or asked for a feature is named
without one ("asked for by rsx-xp"), since an @ both notifies them and reads as
authorship ([#277](https://github.com/hyprlab/hylki/issues/277)). Write the
notes that way; `tools/release-notes.sh` also strips an @ from any other handle
when it builds the page. It joins wrapped lines too, as GitHub keeps every line
break in a release body.

## Before the tag

`python3 tools/check-docs.py` has to pass: a release page, the About window and
the site all link into the documentation, so a broken link ships as a broken
link. It also catches a README that has crept past its budget, and a
contributor line the About window would silently drop.

## Every release gets a section

`RELEASE_NOTES.md` needs an entry for every version, betas included: the About
window renders the current release's notes from it, so a version with no
section shows an empty page. `CHANGELOG.md` is the detailed record and gets its
own entry per release as usual.
