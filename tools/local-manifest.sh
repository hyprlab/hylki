#!/usr/bin/env bash
#
# Emit a copy of the Flatpak manifest that builds the *working tree* instead of
# the tagged commit pinned in co.hyprlab.Hylki.yml.
#
#   tools/local-manifest.sh /tmp/hylki-local.yml
#   tools/local-manifest.sh /tmp/hylki-beta.yml path/to/co.hyprlab.Hylki.Beta.yml
#
# The second argument picks another manifest; the tree it builds is the one
# that manifest sits in. The beta's lives on the beta branch only, and the
# aarch64 CI build runs this script from the workflow's own commit against an
# older checkout, so the tree is never taken from where the script is.
#
# Two callers need this:
#
#   - the aarch64 CI build, which runs on the release tag. The manifest's pin is
#     written in a commit *after* the tag (a commit can't contain its own hash),
#     so the manifest as it exists at vX.Y.Z still points at the previous
#     release — building it verbatim would ship the wrong version.
#   - local test builds, to see uncommitted work without pushing anything.
#
# Everything else — runtime, finish-args, build commands, vendored crates — is
# left exactly as the release manifest has it, so the two builds stay honest
# about being the same recipe.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${2:-$HERE/co.hyprlab.Hylki.yml}"
ROOT="$(cd "$(dirname "$SRC")" && pwd)"
SRC="$ROOT/$(basename "$SRC")"
OUT="${1:-$ROOT/co.hyprlab.Hylki.local.yml}"

python3 - "$SRC" "$OUT" "$ROOT" <<'PY'
import re, sys

src, out, root = sys.argv[1], sys.argv[2], sys.argv[3]
manifest = open(src).read()

# The pinned GitHub source becomes the checkout itself. `skip` keeps build
# output and the signed repo out of the copy flatpak-builder makes.
pinned = re.compile(
    r"      - type: git\n"
    r"        url: https://github\.com/hyprlab/hylki\.git\n"
    r"        tag: v[0-9][0-9A-Za-z.-]*\n"
    r"        commit: [0-9a-f]+\n"
)
local = (
    "      - type: dir\n"
    f"        path: {root}\n"
    "        skip:\n"
    "          - .git\n"
    "          - .flatpak-builder\n"
    "          - dist\n"
    "          - target\n"
    "          - packaging/out\n"
)
manifest, count = pinned.subn(local, manifest)
if count != 1:
    raise SystemExit(f"expected exactly one pinned git source, replaced {count}")

# cargo-sources.json is referenced relative to the manifest, which is about to
# move elsewhere.
manifest = manifest.replace("      - cargo-sources.json\n", f"      - {root}/cargo-sources.json\n")

open(out, "w").write(manifest)
print(out)
PY
