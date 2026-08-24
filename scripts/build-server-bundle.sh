#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
TARGET_TRIPLE=${1:?usage: scripts/build-server-bundle.sh TARGET_TRIPLE OUTPUT_DIRECTORY}
OUTPUT_DIRECTORY=${2:?usage: scripts/build-server-bundle.sh TARGET_TRIPLE OUTPUT_DIRECTORY}

case "$TARGET_TRIPLE" in
  *-unknown-linux-gnu) ;;
  *)
    echo "error: the standalone server archive is currently supported on Linux" >&2
    exit 2
    ;;
esac

case "$OUTPUT_DIRECTORY" in
  "$REPOSITORY_ROOT"/dist/release/*) ;;
  *)
    echo "error: refusing to write a server bundle outside dist/release" >&2
    exit 1
    ;;
esac

STAGE_DIRECTORY="$REPOSITORY_ROOT/apps/desktop/src-tauri/binaries"
VERSION=$(node -p "require('$REPOSITORY_ROOT/apps/desktop/package.json').version")
ARCHIVE="$OUTPUT_DIRECTORY/OpenFlow-server_${VERSION}_${TARGET_TRIPLE}.tar.gz"
TEMPORARY_DIRECTORY=$(mktemp -d "${TMPDIR:-/tmp}/openflow-server-bundle.XXXXXXXX")
trap 'rm -rf -- "$TEMPORARY_DIRECTORY"' EXIT
BUNDLE_DIRECTORY="$TEMPORARY_DIRECTORY/openflow-server"
mkdir -p "$BUNDLE_DIRECTORY/bin"

for name in openflow-server openflow-asr-worker openflow-llm-worker; do
  source="$STAGE_DIRECTORY/$name-$TARGET_TRIPLE"
  if [[ ! -x "$source" ]]; then
    echo "error: missing staged server executable: $source" >&2
    exit 1
  fi
  install -m 0755 "$source" "$BUNDLE_DIRECTORY/bin/$name"
done
install -m 0755 "$REPOSITORY_ROOT/packaging/server/openflow-host" "$BUNDLE_DIRECTORY/bin/openflow-host"

install -m 0644 "$REPOSITORY_ROOT/LICENSE" "$BUNDLE_DIRECTORY/LICENSE"
install -m 0644 \
  "$REPOSITORY_ROOT/packaging/THIRD_PARTY_NOTICES.md" \
  "$BUNDLE_DIRECTORY/THIRD_PARTY_NOTICES.md"
install -m 0644 \
  "$REPOSITORY_ROOT/packaging/THIRD_PARTY_LICENSES.txt" \
  "$BUNDLE_DIRECTORY/THIRD_PARTY_LICENSES.txt"
install -m 0644 "$REPOSITORY_ROOT/packaging/server/README.md" "$BUNDLE_DIRECTORY/README.md"
install -m 0644 \
  "$REPOSITORY_ROOT/packaging/server/openflow-server.env.example" \
  "$BUNDLE_DIRECTORY/openflow-server.env.example"
install -m 0644 \
  "$REPOSITORY_ROOT/packaging/server/openflow-server.service" \
  "$BUNDLE_DIRECTORY/openflow-server.service"

tar -C "$TEMPORARY_DIRECTORY" -czf "$ARCHIVE" openflow-server
ARCHIVE_LIST="$TEMPORARY_DIRECTORY/archive-contents.txt"
tar -tzf "$ARCHIVE" >"$ARCHIVE_LIST"
for required in \
  openflow-server/bin/openflow-server \
  openflow-server/bin/openflow-asr-worker \
  openflow-server/bin/openflow-llm-worker \
  openflow-server/bin/openflow-host \
  openflow-server/THIRD_PARTY_LICENSES.txt; do
  if ! grep -Fxq "$required" "$ARCHIVE_LIST"; then
    echo "error: standalone server archive is missing $required" >&2
    exit 1
  fi
done
echo "Built standalone server archive $ARCHIVE"
