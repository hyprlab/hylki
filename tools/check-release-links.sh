#!/usr/bin/env bash
# Find release pages whose "Full changelog" link compares from a tag GitHub no
# longer has. The link keeps working only as long as that tag does, and it 404s
# without anything saying so. Run this after dropping a tag (docs/RELEASING.md).
#
#   tools/check-release-links.sh         list them; exits 1 if there are any
#   tools/check-release-links.sh --fix   point them at the tag release-notes.sh picks
set -euo pipefail
cd "$(dirname "$0")/.."

repo="hyprlab/hylki"
fix=
[ "${1:-}" = "--fix" ] && fix=1

tags=$(git ls-remote --tags --refs origin 'v*' | sed 's#.*refs/tags/##')
notes=$(mktemp)
trap 'rm -f "$notes"' EXIT
dead=0

# On its own descriptor, as gh and git read standard input and would eat the list.
while read -r head <&3; do
  body=$(gh release view "$head" -R "$repo" --json body --jq .body)
  base=$(printf '%s\n' "$body" | sed -nE "s#.*/compare/([^ )]+)\.\.\.${head}([ )].*)?\$#\1#p" | head -n 1)
  [ -n "$base" ] || continue
  grep -qxF -- "$base" <<<"$tags" && continue

  new=$(tools/release-notes.sh --base "${head#v}")
  if [ -n "$fix" ] && [ -n "$new" ]; then
    printf '%s\n' "${body//"$base...$head"/"$new...$head"}" >"$notes"
    gh release edit "$head" -R "$repo" --notes-file "$notes" >/dev/null
    echo "$head: $base is gone, now compares from $new"
  else
    echo "$head: $base is gone, should compare from ${new:-nothing}"
    dead=1
  fi
done 3< <(gh release list -R "$repo" --limit 1000 --json tagName --jq '.[].tagName')

[ "$dead" -eq 0 ] && echo "Every compare link resolves."
exit "$dead"
