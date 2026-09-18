#!/usr/bin/env bash
# Emit the notes for ONE release, for its GitHub release body (issue #46 —
# release pages used to carry the entire history, which made them unreadable).
#
# The version's section is taken from RELEASE_NOTES.md (the user-facing
# overview) when it has one, else from CHANGELOG.md (which covers every
# release). A footer links to the full history either way.
#
#   tools/release-notes.sh 1.14.3 > /tmp/notes.md
set -euo pipefail
cd "$(dirname "$0")/.."

ver="${1:?usage: release-notes.sh X.Y.Z (no leading v)}"

section() { # file, heading-regex, stop-regex
  awk -v head="$2" -v stop="$3" '
    $0 ~ stop { on = ($0 ~ head) ; if (on) next }
    on { print }
  ' "$1"
}

# "## What's new in X.Y.Z" (newest) or "## In X.Y.Z" (older), with optional
# suffixes like " — security release".
notes=$(section RELEASE_NOTES.md \
  "^## (What.s new in|In) ${ver}($| )" \
  "^## ")
if [ -z "${notes//[[:space:]]/}" ]; then
  # "## X.Y.Z — <date>"
  notes=$(section CHANGELOG.md "^## ${ver} " "^## ")
fi
if [ -z "${notes//[[:space:]]/}" ]; then
  echo "no notes found for ${ver} in RELEASE_NOTES.md or CHANGELOG.md" >&2
  exit 1
fi

printf '%s\n' "$notes" | awk 'NF{f=1} f' | tac | awk 'NF{f=1} f' | tac
printf '\n---\n_The full release history lives in [RELEASE_NOTES.md](https://github.com/hyprlab/hylki/blob/main/RELEASE_NOTES.md)._\n'
