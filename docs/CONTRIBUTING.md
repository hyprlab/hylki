# Contributing

Contributions are welcome, and not only in Rust. If you use Hylki and
something about it is wrong, awkward or missing, that report is worth as much
as a patch — most of what has shipped since 1.15 started as somebody's issue.
Everyone whose work is in the app is named in
[CREDITS.md](CREDITS.md) and in the app's About window.

## Ways in

- **Report a bug** — [open an issue](https://github.com/hyprlab/hylki/issues).
  The version, your distribution and what the server is (Gmail, iCloud, Proton
  Bridge, a self-hosted box) narrow most problems down fast. For anything the
  app did over the network, *Main Menu → Export Log* writes a redacted log of
  every command and answer; attaching it usually settles the question.
- **Suggest a feature** — an issue describing what you are trying to do, rather
  than the control you imagine, gets the best result.
- **Translate** — no Rust needed, and everything a translator wants is in
  [po/README.md](../po/README.md).
- **Design and UX** — GNOME HIG corrections, layout and wording feedback are
  welcome as issues.
- **Code** — send a pull request; see below.
- **Package it** — the Gentoo and Nix packages are maintained by their users.
- **Security** — please report privately: see [SECURITY.md](../SECURITY.md).

## Pull requests

- Branch from `main` and keep the change to one subject.
- Build it and run it before opening the PR (`cargo build`, then the app).
  [BUILDING.md](BUILDING.md) has the dependencies.
- Match the surrounding code: the codebase comments *why*, not *what*, and
  user-facing strings go through `i18n()`.
- New or changed strings mean the template needs a refresh:
  `tools/update-pot.sh`.
- There's no CLA. By opening a pull request you agree your contribution ships
  under the [AGPL-3.0-or-later](../LICENSE), and that it may be adapted before
  it lands — with the change explained on the pull request.
- Contributions are credited by name and handle, so say if you would rather be
  credited differently, or not at all.

## Documentation

Documentation lives in `docs/`, one subject per file, and the
[README](../README.md) is the front door: what the app is, ten or so features,
how to install it, and links. It reached 667 lines once by taking "just one
more paragraph" forty times ([#230](https://github.com/hyprlab/hylki/issues/230)),
so a change that documents something goes where that something already lives:

| What you have | Where it goes |
| --- | --- |
| A feature worth pitching | [FEATURES.md](FEATURES.md) |
| How to set something up, or use it | [DOCUMENTATION.md](DOCUMENTATION.md) |
| A keyboard change | [KEYBOARD_SHORTCUTS.md](KEYBOARD_SHORTCUTS.md) |
| A package or install caveat | [INSTALLING.md](INSTALLING.md) |
| A build dependency or step | [BUILDING.md](BUILDING.md) |
| Credit | [`data/CONTRIBUTORS`](../data/CONTRIBUTORS) or [`data/TRANSLATORS`](../data/TRANSLATORS) for the name, [CREDITS.md](CREDITS.md) for the work |
| A licence or trademark note | [LICENSE.md](LICENSE.md) |
| What changed in a release | [CHANGELOG.md](../CHANGELOG.md) and [RELEASE_NOTES.md](../RELEASE_NOTES.md) |
| Artwork | `data/repo/` — `docs/` holds Markdown only |

A new page belongs in [docs/README.md](README.md)'s index, and
`python3 tools/check-docs.py` checks all of this — every relative link and
anchor included — in about a second. Run it after touching any `.md`.

## AI-assisted contributions

Hylki is built with AI assistance itself (see the
[AI notice](../README.md#ai-notice)), so patches written with AI tools are as
welcome as any other. Everything merged gets the same human review, and the
same rule applies either way: you are responsible for what your patch does.

## Releases

Notes for each release are written as short highlights plus the generated list
of what changed; the format is in [RELEASING.md](RELEASING.md).
