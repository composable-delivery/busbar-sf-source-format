#!/usr/bin/env bash
set -euo pipefail

TAG="${1:-13.13.0}"
REPO_URL="https://github.com/forcedotcom/source-deploy-retrieve.git"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Cloning forcedotcom/source-deploy-retrieve at tag $TAG..."
git clone --depth 1 --branch "$TAG" "$REPO_URL" "$TMP_DIR/sdr"

COMMIT_SHA="$(git -C "$TMP_DIR/sdr" rev-parse HEAD)"
echo "Checked out $TAG ($COMMIT_SHA)"

mkdir -p "$ROOT_DIR/crates/source-format/data/presets"
cp "$TMP_DIR/sdr/src/registry/metadataRegistry.json" "$ROOT_DIR/crates/source-format/data/metadataRegistry.json"
cp "$TMP_DIR/sdr/src/registry/stdValueSetRegistry.json" "$ROOT_DIR/crates/source-format/data/stdValueSetRegistry.json"
cp "$TMP_DIR/sdr/src/registry/presets/"*.json "$ROOT_DIR/crates/source-format/data/presets/"

rm -rf "$ROOT_DIR/fixtures/sdr-snapshots"
mkdir -p "$ROOT_DIR/fixtures/sdr-snapshots"
cp -R "$TMP_DIR/sdr/test/snapshot/sampleProjects/." "$ROOT_DIR/fixtures/sdr-snapshots/"

cat > "$ROOT_DIR/fixtures/sdr-snapshots/UPSTREAM_VERSION" <<EOF
tag=$TAG
commit=$COMMIT_SHA
repo=$REPO_URL
EOF

echo "Synced registry data and $(find "$ROOT_DIR/fixtures/sdr-snapshots" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ') snapshot projects."
