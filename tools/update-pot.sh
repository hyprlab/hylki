#!/usr/bin/env bash
# Regenerate the translation template (po/hylki.pot) from the source, and
# refresh every po/<lang>.po against it.
#
# Strings reach the template through the helpers in src/i18n.rs — i18n(),
# ni18n(), i18n_f(), ni18n_f() and i18n_noop() — extracted by xtr (the
# Rust-aware xgettext: `cargo install xtr`), plus the launcher and metainfo
# entries via xgettext. Translators never touch the source: they edit
# po/<lang>.po (Poedit, Gtranslator, or any editor) and send that back.
#
#   tools/update-pot.sh            # run after strings change, before a release
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/header.pot" <<'HDR'
# Translation template for Hylki.
# This file is distributed under the same license as the Hylki package.
#
msgid ""
msgstr ""
"Project-Id-Version: hylki\n"
"Report-Msgid-Bugs-To: https://github.com/hyprlab/hylki/issues\n"
"MIME-Version: 1.0\n"
"Content-Type: text/plain; charset=UTF-8\n"
"Content-Transfer-Encoding: 8bit\n"
"Plural-Forms: nplurals=2; plural=(n != 1);\n"
HDR
xtr -k i18n -k ni18n:1,2 -k i18n_f -k ni18n_f:1,2 -k i18n_noop \
    -o "$tmp/src.pot" src/main.rs
xgettext --from-code=UTF-8 --language=Desktop \
    -k --keyword=Name --keyword=GenericName --keyword=Comment --keyword=Keywords \
    -o "$tmp/desktop.pot" data/co.hyprlab.Hylki.desktop
# The metainfo's name, summary and description — not the release history,
# which nobody should have to translate.
sed '/<releases>/,/<\/releases>/d' data/co.hyprlab.Hylki.metainfo.xml > "$tmp/metainfo.xml.in"
xgettext --from-code=UTF-8 --its=/usr/share/gettext/its/metainfo.its \
    -o "$tmp/metainfo.pot" "$tmp/metainfo.xml.in"
msgcat --use-first --no-location "$tmp/header.pot" "$tmp/src.pot" "$tmp/desktop.pot" "$tmp/metainfo.pot" \
  | sed '/^"POT-Creation-Date/d' > po/hylki.pot

for po in po/*.po; do
  [ -e "$po" ] || continue
  msgmerge --quiet --update --backup=none "$po" po/hylki.pot
done
echo "==> $(grep -c '^msgid ' po/hylki.pot) strings in po/hylki.pot"
