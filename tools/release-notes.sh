#!/usr/bin/env bash
# Emit the notes for ONE release, for its GitHub release body (issue #46 —
# release pages used to carry the entire history, which made them unreadable).
#
# The body is two parts:
#
#   Notes            the version's section of docs/RELEASE_NOTES.md: New
#                    features, Fixes and Translations, one short line each
#                    (see docs/RELEASING.md), or of CHANGELOG.md when it has none.
#   Full changelog   the compare link to the previous release of the same kind.
#
# GitHub's generated "What's changed" list is left out on purpose: at the rate
# this repository commits it is the compare view again, only longer.
#
#   tools/release-notes.sh 1.14.3 > /tmp/notes.md
set -euo pipefail
cd "$(dirname "$0")/.."

ver="${1:?usage: release-notes.sh X.Y.Z (no leading v)}"
repo="hyprlab/hylki"

section() { # file, heading-regex, stop-regex
  awk -v head="$2" -v stop="$3" '
    $0 ~ stop { on = ($0 ~ head) ; if (on) next }
    on { print }
  ' "$1"
}

trim() { # drop leading and trailing blank lines
  awk 'NF{f=1} f' | tac | awk 'NF{f=1} f' | tac
}

# The Markdown files are wrapped at 76 columns, but GitHub keeps every line
# break in a release body, so wrapped paragraphs showed up there as ragged
# short lines (#277). Rejoin each paragraph and list item onto one line;
# headings, new list items, quotes, tables and code blocks stay as they are.
unwrap() {
  awk '
    function flush() { if (buf != "") print buf; buf = "" }
    /^[ \t]*```/ { flush(); print; fence = !fence; next }
    fence { print; next }
    /^[ \t]*$/ { flush(); print; next }
    /^(#+ |> |\||---)/ || /^[ \t]*([-*+]|[0-9]+\.) / { flush(); buf = $0; next }
    buf != "" { line = $0; sub(/^[ \t]+/, "", line); buf = buf " " line; next }
    { buf = $0 }
    END { flush() }
  '
}

# An @ on a release page is for the people whose work is in it (#277): the
# handles in data/CONTRIBUTORS and data/TRANSLATORS. Anyone else the notes
# name, such as whoever reported a bug or asked for a feature, is named
# without one, so the page neither links them as an author nor notifies them.
credit() {
  python3 -c '
import re, sys
authors = set()
for path in ("data/CONTRIBUTORS", "data/TRANSLATORS"):
    for line in open(path, encoding="utf-8"):
        m = re.search(r"<([^>]+)>", line)
        if m and not line.lstrip().startswith("#"):
            authors.add(m.group(1).lower())
def plain(m):
    handle = m.group(1) or m.group(2)
    return m.group(0) if handle.lower() in authors else handle
link = r"\[@([A-Za-z0-9-]+)\]\(https://github\.com/[A-Za-z0-9-]+/?\)"
bare = r"(?<![\w.@/])@([A-Za-z0-9-]+)\b"
for line in sys.stdin:
    sys.stdout.write(re.sub(link + "|" + bare, plain, line))
'
}

# "## What's new in X.Y.Z" (newest) or "## In X.Y.Z" (older), with optional
# suffixes like " — security release".
notes=$(section docs/RELEASE_NOTES.md \
  "^## (What.s new in|In) ${ver}($| )" \
  "^## ")
if [ -z "${notes//[[:space:]]/}" ]; then
  # "## X.Y.Z — <date>"
  notes=$(section CHANGELOG.md "^## ${ver} " "^## ")
fi
if [ -z "${notes//[[:space:]]/}" ]; then
  echo "no notes found for ${ver} in docs/RELEASE_NOTES.md or CHANGELOG.md" >&2
  exit 1
fi

printf '%s\n' "$notes" | trim | unwrap | credit

# The release before this one, in version order, and of the same kind: a beta
# is measured against the previous beta, a stable against the previous stable.
# (Measuring a catch-up beta against the stable it catches up with would list
# nothing, which is how this was found.) Betas are vX.Y.Z-beta.N, or the older
# vX.Y.Zb.
beta_re='-beta\.|[0-9]b$'
if printf '%s' "v${ver}" | grep -Eq "$beta_re"; then
  kind() { grep -E -- "$beta_re"; }
else
  kind() { grep -Ev -- "$beta_re"; }
fi
before() { awk -v cur="v${ver}" 'seen { print; exit } $0 == cur { seen = 1 }'; }
prev=$(git tag --list 'v*' --sort=-v:refname | kind | before)
# A superseded release loses its tag, so the first beta after a stable finds
# no beta to measure against; the stable before it is the next best thing.
if [ -z "$prev" ]; then
  prev=$(git tag --list 'v*' --sort=-v:refname | before)
fi

if [ -n "$prev" ]; then
  printf '\nFull changelog: [%s...v%s](https://github.com/%s/compare/%s...v%s)\n' \
    "$prev" "$ver" "$repo" "$prev" "$ver"
fi
