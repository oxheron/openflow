#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
EXPECTED_TAG=${1:-}

CARGO_VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' "$REPOSITORY_ROOT/Cargo.toml")
TAURI_VERSION=$(node -p "require('$REPOSITORY_ROOT/apps/desktop/src-tauri/tauri.conf.json').version")
NPM_VERSION=$(node -p "require('$REPOSITORY_ROOT/apps/desktop/package.json').version")

if [[ -z "$CARGO_VERSION" || "$CARGO_VERSION" != "$TAURI_VERSION" || "$CARGO_VERSION" != "$NPM_VERSION" ]]; then
  echo "error: Cargo ($CARGO_VERSION), Tauri ($TAURI_VERSION), and npm ($NPM_VERSION) versions differ" >&2
  exit 1
fi

if [[ -n "$EXPECTED_TAG" && "$EXPECTED_TAG" != "v$CARGO_VERSION" ]]; then
  echo "error: release tag $EXPECTED_TAG does not match application version v$CARGO_VERSION" >&2
  exit 1
fi

echo "OpenFlow version $CARGO_VERSION is consistent"
