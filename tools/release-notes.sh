#!/usr/bin/env bash
# Emit the notes for ONE release, for its GitHub release body (issue #46 —
# release pages used to carry the entire history, which made them unreadable).
#
# The body has three parts (#230):
#
#   Highlights   the version's section of docs/RELEASE_NOTES.md (the
#                user-facing overview), or of CHANGELOG.md when it has none.
#   What's changed   the list GitHub generates for the tag: every merged pull
#                request and commit, with its author, since the previous
#                release. Skipped when gh or the tag is missing.
#   Full changelog   links to the compare view and to the files that hold the
#                whole history.
#
#   tools/release-notes.sh 1.14.3 > /tmp/notes.md
#
# The tag has to be pushed before the generated list can be asked for, which
# is the order the ship steps already run in.
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

printf '%s\n' "$notes" | trim

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
prev=$(git tag --list 'v*' --sort=-v:refname \
  | kind \
  | awk -v cur="v${ver}" 'seen { print; exit } $0 == cur { seen = 1 }')

# GitHub's own list, which names every merged pull request and its author.
# Empty when the release carries no pull requests (most of them: the work
# lands as commits on main), in which case the commits themselves are listed.
changed=""
if command -v gh >/dev/null 2>&1; then
  body=$(gh api --method POST "repos/${repo}/releases/generate-notes" \
    -f tag_name="v${ver}" \
    ${prev:+-f previous_tag_name="$prev"} \
    --jq .body 2>/dev/null || true)
  # Its pull-request lines only; the heading and compare link are ours below.
  changed=$(printf '%s\n' "$body" | grep '^\* ' | sed 's/^\* /- /' || true)
fi
if [ -z "${changed//[[:space:]]/}" ] && [ -n "$prev" ]; then
  changed=$(git log --no-merges --reverse --format='- %s (%h)' "${prev}..v${ver}" 2>/dev/null || true)
fi

if [ -n "${changed//[[:space:]]/}" ]; then
  printf "\n## What's changed\n\n%s\n" "$changed"
fi
if [ -n "$prev" ]; then
  printf '\n**Full changelog**: https://github.com/%s/compare/%s...v%s\n' \
    "$repo" "$prev" "$ver"
fi

printf '\n---\n_Every release is listed in [RELEASE_NOTES.md](https://github.com/%s/blob/main/docs/RELEASE_NOTES.md), commit by commit in [CHANGELOG.md](https://github.com/%s/blob/main/CHANGELOG.md)._\n' \
  "$repo" "$repo"
