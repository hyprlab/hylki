#!/usr/bin/env bash
#
# Pull the aarch64 build made by .github/workflows/build-arm64.yml into the
# signed distribution repo, and produce the standalone ARM64 bundle.
#
#   tools/import-arm64.sh v1.9.2       # a tag (default: the current version)
#   tools/import-arm64.sh --run 12345  # a specific workflow run id
#   tools/import-arm64.sh --beta --run 12345   # a beta build (co.hyprlab.Hylki.Beta)
#
# A beta tag (vX.Y.Z-beta.N) is a beta build too: it goes to the repo's beta
# branch as co.hyprlab.Hylki.Beta, and its bundle is Hylki-Beta-aarch64.flatpak.
#
# CI never signs anything: it uploads a plain OSTree repo. This re-commits that
# build into dist/repo under the project's key, so both architectures in the
# published repo carry the same signature and `flatpak update` verifies on ARM
# exactly as it does on x86_64.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_ID="co.hyprlab.Hylki"
BRANCH="stable"
BUNDLE="Hylki-aarch64.flatpak"
if [ "${1:-}" = "--beta" ] || [[ "${1:-}" == v*-beta.* ]]; then
    APP_ID="co.hyprlab.Hylki.Beta"
    BRANCH="beta"
    BUNDLE="Hylki-Beta-aarch64.flatpak"
    [ "${1:-}" = "--beta" ] && shift
fi
REPO="$ROOT/dist/repo"
GPG_HOME="$ROOT/dist/gpg-home"
KEYID="$(cat "$ROOT/dist/keyid.txt")"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
OUT="$ROOT/packaging/out"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [ "${1:-}" = "--run" ]; then
    RUN_ARGS=("${2:?run id required}")
else
    TAG="${1:-v$VERSION}"
    # The newest successful run of the workflow for that tag.
    RUN_ID="$(gh run list --repo hyprlab/hylki --workflow build-arm64.yml \
        --branch "$TAG" --status success --limit 1 --json databaseId \
        --jq '.[0].databaseId')"
    [ -n "$RUN_ID" ] || { echo "no successful aarch64 build for $TAG" >&2; exit 1; }
    echo "using workflow run $RUN_ID for $TAG"
    RUN_ARGS=("$RUN_ID")
fi

gh run download "${RUN_ARGS[0]}" --repo hyprlab/hylki --name hylki-aarch64-repo --dir "$WORK"
tar xzf "$WORK/hylki-aarch64-repo.tar.gz" -C "$WORK"
SRC="$WORK/repo-aarch64"

# Re-commit into the signed repo. `build-commit-from` copies the commit's
# content and signs it with our key rather than trusting CI's (unsigned) one.
flatpak build-commit-from \
    --src-repo="$SRC" \
    --gpg-sign="$KEYID" --gpg-homedir="$GPG_HOME" \
    "$REPO" "app/$APP_ID/aarch64/$BRANCH"

flatpak build-update-repo --generate-static-deltas \
    --gpg-sign="$KEYID" --gpg-homedir="$GPG_HOME" "$REPO"

mkdir -p "$OUT"
flatpak build-bundle --arch=aarch64 "$REPO" "$OUT/$BUNDLE" \
    "$APP_ID" "$BRANCH" \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo \
    --repo-url=https://hylki.hyprlab.co/flatpak \
    --gpg-keys="$ROOT/dist/veem.gpg"

echo
echo "imported:"
ostree --repo="$REPO" refs | grep "$APP_ID" | sort
echo "bundle:   $OUT/$BUNDLE"
