#!/usr/bin/env bash
# Compile po/<lang>.po into the message catalogues a source-tree run picks
# up (po/.build/<lang>/LC_MESSAGES/hylki.mo — see src/i18n.rs). Installs
# don't need this: the Flatpak build and the RPM spec compile the same
# files into their own share/locale.
#
#   tools/build-locale.sh
#   LANG=fr_FR.UTF-8 ./target/debug/hylki
set -euo pipefail
cd "$(dirname "$0")/.."
for po in po/*.po; do
  [ -e "$po" ] || continue
  lang=$(basename "$po" .po)
  mkdir -p "po/.build/$lang/LC_MESSAGES"
  msgfmt --check -o "po/.build/$lang/LC_MESSAGES/hylki.mo" "$po"
  echo "==> $lang"
done
