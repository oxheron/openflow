#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
TARGET_TRIPLE=${1:-$(rustc --print host-tuple)}
STAGE_DIR=${2:-"$REPOSITORY_ROOT/apps/desktop/src-tauri/binaries"}

for name in openflow-server openflow-asr-worker openflow-llm-worker; do
  binary="$STAGE_DIR/$name-$TARGET_TRIPLE"
  if [[ ! -f "$binary" || ! -x "$binary" ]]; then
    echo "error: missing executable Tauri sidecar: $binary" >&2
    exit 1
  fi

  if command -v file >/dev/null && ! file "$binary" | grep -Eq 'executable|Mach-O'; then
    echo "error: staged sidecar is not a native executable: $binary" >&2
    exit 1
  fi
done

if [[ "$TARGET_TRIPLE" == *-apple-darwin ]] && command -v otool >/dev/null; then
  for binary in "$STAGE_DIR"/*-"$TARGET_TRIPLE"; do
    if otool -L "$binary" | grep -E '/(target|release-upstream|release-native)/|lib(ggml|llama|whisper)\.'; then
      echo "error: sidecar contains a non-portable inference-library reference: $binary" >&2
      exit 1
    fi
  done
fi

if [[ "$TARGET_TRIPLE" == *-unknown-linux-gnu ]] && command -v ldd >/dev/null; then
  for binary in "$STAGE_DIR"/*-"$TARGET_TRIPLE"; do
    if ldd "$binary" 2>&1 | grep -E 'not found|/(target|release-upstream|release-native)/|lib(ggml|llama|whisper)\.so'; then
      echo "error: sidecar contains an unresolved or build-local library: $binary" >&2
      exit 1
    fi
  done
fi

HOST_TRIPLE=$(rustc --print host-tuple)
if [[ "$TARGET_TRIPLE" == "$HOST_TRIPLE" ]]; then
  if ! command -v python3 >/dev/null; then
    echo "error: Python 3 is required for the sidecar protocol handshake" >&2
    exit 1
  fi
  if [[ "${OPENFLOW_NATIVE_MOCK:-0}" == 1 ]]; then
    python3 "$REPOSITORY_ROOT/native/tests/worker_protocol_test.py" \
      "$STAGE_DIR/openflow-asr-worker-$TARGET_TRIPLE" \
      "$STAGE_DIR/openflow-llm-worker-$TARGET_TRIPLE"
  else
    OPENFLOW_EXPECT_REAL_BACKENDS=1 \
      OPENFLOW_EXPECT_COMPUTE_BACKEND="${OPENFLOW_EXPECT_COMPUTE_BACKEND:-}" \
      python3 "$REPOSITORY_ROOT/native/tests/worker_protocol_test.py" \
      "$STAGE_DIR/openflow-asr-worker-$TARGET_TRIPLE" \
      "$STAGE_DIR/openflow-llm-worker-$TARGET_TRIPLE"
  fi
else
  echo "note: cannot execute $TARGET_TRIPLE sidecars on $HOST_TRIPLE; static checks only" >&2
fi

echo "Verified three sidecars for $TARGET_TRIPLE"
