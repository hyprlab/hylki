# Working in this repository

Conventions for anyone — human or AI — editing Hylki. The code's own rules are
in [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md); this file is what has to be
remembered every session.

## Documentation has a shape, and it is enforced

The README once reached 667 lines because every change added "just one
paragraph" to it (#230). It is now the front door and nothing else. Before
writing a word of documentation, decide where it goes:

| What you have | Where it goes |
| --- | --- |
| A new feature worth pitching | `docs/FEATURES.md`; the README only if it displaces one of the eleven already there |
| How to set something up, or use it | `docs/DOCUMENTATION.md` |
| A keyboard change | `docs/KEYBOARD_SHORTCUTS.md` |
| A package, a distribution, an install caveat | `docs/INSTALLING.md` |
| A build dependency or step | `docs/BUILDING.md` |
| Credit for somebody's work | `data/CONTRIBUTORS` or `data/TRANSLATORS` for the name, `docs/CREDITS.md` for what they did |
| A licence or trademark note | `docs/LICENSE.md` |
| What changed in a release | `CHANGELOG.md` (detail) and `RELEASE_NOTES.md` (the user-facing overview) |
| Artwork for the README or the docs | `data/repo/` — never `docs/`, which holds Markdown only |

A new `docs/*.md` has to be listed in `docs/README.md` and linked from
somewhere, or it is a file nobody will ever find.

**After touching any `.md`, run `python3 tools/check-docs.py`.** It checks all
of the above, plus every relative link and anchor in the repository. It is
fast, it has no dependencies, and a release should not go out while it fails
(see [docs/RELEASING.md](docs/RELEASING.md)).

## Prose style

- Factual, plain language. No marketing, no emoji in the changelog, notes or
  documentation.
- Say what the app does, not what it enables you to do.
- Comments in the code explain *why*, not *what*.

## Code

- User-facing strings go through the `i18n()` helpers in `src/i18n.rs`, and
  `tools/update-pot.sh` is run when they change.
- Contributor names are data (`data/CONTRIBUTORS`, `data/TRANSLATORS`), not
  constants in `src/app.rs`, which reads those files at build time.
- `cargo test` before committing; `cargo build` and run the app before saying
  something works.
