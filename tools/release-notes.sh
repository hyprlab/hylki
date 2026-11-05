#!/usr/bin/env bash
# Emit the notes for ONE release, for its GitHub release body (issue #46 —
# release pages used to carry the entire history, which made them unreadable).
#
# The body is two parts:
#
#   Notes            the version's section of docs/RELEASE_NOTES.md: New
#                    features, Fixes and Translations, one short line each
#                    (see docs/RELEASING.md), or of CHANGELOG.md when it has none.
#   Full changelog   the compare link from a tag that outlasts this page.
#
# GitHub's generated "What's changed" list is left out on purpose: at the rate
# this repository commits it is the compare view again, only longer.
#
#   tools/release-notes.sh 1.14.3 > /tmp/notes.md
#   tools/release-notes.sh --base 1.14.3    only the tag its link compares from
set -euo pipefail
cd "$(dirname "$0")/.."

only_base=
if [ "${1:-}" = "--base" ]; then
  only_base=1
  shift
fi
ver="${1:?usage: release-notes.sh [--base] X.Y.Z (no leading v)}"
repo="hyprlab/hylki"

# The newest stable tag on GitHub that sorts before the one given. Local tags
# are no use here, as a clone keeps the tags GitHub has since dropped.
newest_stable_before() {
  { git ls-remote --tags --refs origin 'v*' | sed 's#.*refs/tags/##' |
      grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$'
    echo "$1"
  } | sort -uV | awk -v tag="$1" '$0 == tag { print last; exit } { last = $0 }'
}

# The tag a release's compare link starts from. A dropped tag kills every link
# naming it, and docs/RELEASING.md drops a superseded beta and all but the last
# tag of an old stable series, so the link can only start from a tag those rules
# keep: a beta from the last stable, a stable from the last tag of the series
# before its own. A beta that only catches up with a stable lists little, which
# beats a 404.
compare_base() {
  local base
  if printf '%s' "$1" | grep -Eq -- '-beta\.|[0-9]b$'; then
    base=${1%%-beta.*}
    newest_stable_before "v${base%b}"
    return
  fi
  base=$(newest_stable_before "v${1%.*}.0")
  # The first series has no series before it.
  [ -n "$base" ] || base=$(newest_stable_before "v$1")
  printf '%s\n' "$base"
}

if [ -n "$only_base" ]; then
  compare_base "$ver"
  exit
fi

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

prev=$(compare_base "$ver")

if [ -n "$prev" ]; then
  printf '\nFull changelog: [%s...v%s](https://github.com/%s/compare/%s...v%s)\n' \
    "$prev" "$ver" "$repo" "$prev" "$ver"
fi
