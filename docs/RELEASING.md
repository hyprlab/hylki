# Release notes

What a Hylki release page says, and where each part comes from (#230).

## The three parts

1. **Highlights** — a short bulletin of what is worth knowing, one line each,
   newest and biggest first. A release usually has three to six:

   ```
   - Meeting invitations show in the message, with Accept, Maybe and Decline (#223)
   - One-click Unsubscribe on mailing-list messages
   - Focus Mode (Ctrl+Shift+F) folds the window down to what you are reading
   - Fixed: message rows redraw when you change how they look
   ```

   This is the version's section of [RELEASE_NOTES.md](../RELEASE_NOTES.md),
   which is also what the app's About window shows. Write for somebody
   deciding whether to update, not for somebody auditing the diff: name the
   thing and what it does, and leave the reasoning to the changelog.

2. **What's changed** — the list of merged pull requests with their authors, as
   GitHub generates it, or the release's commit subjects where the work landed
   without a pull request. Generated, never hand-written.

3. **Full changelog** — the compare link, plus
   [RELEASE_NOTES.md](../RELEASE_NOTES.md) and
   [CHANGELOG.md](../CHANGELOG.md), which hold the detail and the history.

## Building the body

`tools/release-notes.sh` assembles all three:

```sh
tools/release-notes.sh 1.36.0 > /tmp/notes.md
gh release create v1.36.0 --title "Hylki 1.36.0" --notes-file /tmp/notes.md ...
```

Part 2 is asked of GitHub, so the tag has to be pushed first — the order the
ship steps already run in. Without `gh`, or before the tag is up, the script
still emits parts 1 and 3 and says on stderr what it left out.

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
