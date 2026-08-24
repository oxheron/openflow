#!/usr/bin/env bash
set -euo pipefail

DIRECTORY=${1:?usage: scripts/write-checksums.sh DIRECTORY}
if [[ ! -d "$DIRECTORY" ]]; then
  echo "error: checksum directory does not exist: $DIRECTORY" >&2
  exit 2
fi

OUTPUT="$DIRECTORY/SHA256SUMS"
TEMPORARY="$DIRECTORY/.SHA256SUMS.tmp"
trap 'rm -f "$TEMPORARY"' EXIT

(
  cd "$DIRECTORY"
  shopt -s nullglob
  files=()
  for file in *; do
    [[ -f "$file" && "$file" != SHA256SUMS && "$file" != .SHA256SUMS.tmp ]] || continue
    files+=("$file")
  done
  if [[ ${#files[@]} -eq 0 ]]; then
    echo "error: there are no release files to checksum" >&2
    exit 1
  fi
  printf '%s\n' "${files[@]}" | LC_ALL=C sort | while IFS= read -r file; do
    if command -v sha256sum >/dev/null; then
      sha256sum "$file"
    else
      shasum -a 256 "$file"
    fi
  done
) >"$TEMPORARY"
mv "$TEMPORARY" "$OUTPUT"
trap - EXIT
