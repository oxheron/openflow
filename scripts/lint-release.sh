#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)

for script in "$SCRIPT_DIR"/*.sh; do
  bash -n "$script"
done

if command -v shellcheck >/dev/null; then
  shellcheck -x "$SCRIPT_DIR"/*.sh
else
  echo "note: shellcheck is not installed; bash syntax checks still passed" >&2
fi

node -e \
  "JSON.parse(require('fs').readFileSync(process.argv[1], 'utf8'))" \
  "$REPOSITORY_ROOT/packaging/tauri.release.conf.json"
"$SCRIPT_DIR/verify-version.sh"
"$SCRIPT_DIR/generate-third-party-licenses.mjs" --check-inputs

if ! grep -Eq '^OPENFLOW_WHISPER_CPP_REVISION=[0-9a-f]{40}$' "$REPOSITORY_ROOT/packaging/upstream.env" ||
  ! grep -Eq '^OPENFLOW_LLAMA_CPP_REVISION=[0-9a-f]{40}$' "$REPOSITORY_ROOT/packaging/upstream.env"; then
  echo "error: inference source revisions must be immutable full commit hashes" >&2
  exit 1
fi
if grep -q '"externalBin"' "$REPOSITORY_ROOT/apps/desktop/src-tauri/tauri.conf.json"; then
  echo "error: sidecars belong in the release overlay, not the development config" >&2
  exit 1
fi
if ! grep -q 'com.apple.security.device.audio-input' \
  "$REPOSITORY_ROOT/apps/desktop/src-tauri/Entitlements.plist"; then
  echo "error: the macOS release must carry the audio-input entitlement" >&2
  exit 1
fi

echo "Release scripts and configuration passed static verification"
