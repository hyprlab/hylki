# Translating Hylki

Hylki's interface text is translated with gettext. Everything a translator
needs is in this directory; no Rust knowledge is required, and no source
file has to be touched.

## Adding or updating a language

1. Take the template, `hylki.pot`. It lists every string the app shows,
   with an empty slot for the translation.
2. Create `<lang>.po` from it, where `<lang>` is the language code
   (`fr` for French, `pt_BR` for Brazilian Portuguese):

       msginit --locale=fr --input=hylki.pot --output=fr.po

   or open the template in Poedit or Gtranslator and save it as `fr.po`.
   To update an existing translation after the template changed, open the
   `.po` file and use the editor's "update from template" (or
   `msgmerge --update fr.po hylki.pot`); new strings show up untranslated
   and changed ones as "fuzzy".
3. Translate. Keep the `{placeholders}` exactly as they are (they are
   filled in at runtime), but move them around freely to suit the
   language. Strings with two forms are plurals: give the singular and the
   plural your language uses.
4. Add the language code to `LINGUAS` (one per line) if it is new.
5. Check it compiles: `msgfmt --check fr.po -o /dev/null`.
6. Send the `.po` file as a pull request, or attach it to an issue.

## Trying a translation in the app

From a source checkout:

    tools/build-locale.sh              # compiles po/*.po for a source-tree run
    LANG=fr_FR.UTF-8 ./target/debug/hylki

The app follows the desktop's language. Installs pick up translations
through their normal build: the Flatpak, the RPM and `install.sh` all
compile `po/*.po` into their `share/locale` and merge the translated
launcher and metainfo fields.

## For maintainers

`tools/update-pot.sh` regenerates `hylki.pot` from the source (through
`xtr`, the Rust-aware xgettext: `cargo install xtr`) and refreshes every
`.po` against it. Run it whenever strings change and before a release, so
translators see the current set. In the code, user-facing strings go
through the helpers in `src/i18n.rs`: `i18n("...")`, `ni18n` for plurals,
`i18n_f` / `ni18n_f` for named placeholders, and `i18n_noop` for tables
of literals translated where they are shown.
