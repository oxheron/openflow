#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
cd "$REPOSITORY_ROOT"

violations=()

while IFS= read -r file; do
  [[ "/$file/" == */tests/* ]] || violations+=("test file outside a tests directory: $file")
done < <(
  rg --files \
    -g '*.test.*' \
    -g '*.spec.*' \
    -g '*_test.*' \
    -g 'test_*.py'
)

while IFS= read -r file; do
  violations+=("inline Rust test outside a tests directory: $file")
done < <(
  rg -l '#\[cfg\(test\)\]|#\[(tokio::)?test\]' apps crates \
    -g '*.rs' \
    -g '!**/tests/**' || true
)

while IFS= read -r file; do
  violations+=("Vitest suite outside a tests directory: $file")
done < <(
  rg -l 'vitest' apps \
    -g '*.ts' \
    -g '*.tsx' \
    -g '!**/tests/**' || true
)

if ((${#violations[@]} > 0)); then
  printf 'error: test layout violations found:\n' >&2
  printf '  - %s\n' "${violations[@]}" >&2
  exit 1
fi

echo "All test suites are contained in tests directories."
