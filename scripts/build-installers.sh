#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
TARGET_TRIPLE=$(rustc --print host-tuple)
BUNDLES=

usage() {
  cat <<'USAGE'
Usage: scripts/build-installers.sh [--target TRIPLE] [--profile PROFILE]
                                   [--bundles LIST] [--mock-native]

Profiles: auto (default), cpu, metal, vulkan, cuda.
For ROCm on Arch Linux, use scripts/build-arch-bundle.sh --profile rocm so the
artifact has an explicit distro runtime contract.
Bundle defaults: app,dmg on macOS; appimage,deb,rpm on Linux.
LIST is a comma-separated list accepted by `tauri build --bundles`.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      TARGET_TRIPLE=${2:?--target needs a value}
      shift 2
      ;;
    --profile)
      export OPENFLOW_INFERENCE_PROFILE=${2:?--profile needs a value}
      shift 2
      ;;
    --bundles)
      BUNDLES=${2:?--bundles needs a value}
      shift 2
      ;;
    --mock-native)
      export OPENFLOW_NATIVE_MOCK=1
      shift
      ;;
    --help | -h)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$TARGET_TRIPLE" in
  *-apple-darwin) BUNDLES=${BUNDLES:-app,dmg} ;;
  *-unknown-linux-gnu) BUNDLES=${BUNDLES:-appimage,deb,rpm} ;;
  *)
    echo "error: no installer set is defined for $TARGET_TRIPLE" >&2
    exit 2
    ;;
esac

if [[ "$TARGET_TRIPLE" == *-unknown-linux-gnu && \
  "${OPENFLOW_INFERENCE_PROFILE:-auto}" == rocm ]]; then
  echo "error: generic Linux installers cannot declare a portable ROCm runtime baseline" >&2
  echo "  on Arch Linux, use scripts/build-arch-bundle.sh --profile rocm" >&2
  exit 2
fi

"$SCRIPT_DIR/stage-sidecars.sh" "$TARGET_TRIPLE"
"$SCRIPT_DIR/verify-version.sh"
npm --prefix "$REPOSITORY_ROOT/apps/desktop" ci
"$SCRIPT_DIR/generate-third-party-licenses.mjs" --write
"$SCRIPT_DIR/generate-third-party-licenses.mjs" --check

(
  cd "$REPOSITORY_ROOT/apps/desktop"
  npm run tauri -- build \
    --target "$TARGET_TRIPLE" \
    --bundles "$BUNDLES" \
    --config ../../packaging/tauri.release.conf.json \
    --ci
)

VERSION=$(node -p "require('$REPOSITORY_ROOT/apps/desktop/package.json').version")
BUNDLE_ROOT="$REPOSITORY_ROOT/target/$TARGET_TRIPLE/release/bundle"
OUTPUT_DIR="$REPOSITORY_ROOT/dist/release/$TARGET_TRIPLE"
"$SCRIPT_DIR/verify-package-payload.sh" "$TARGET_TRIPLE" "$BUNDLE_ROOT" "$BUNDLES"
case "$OUTPUT_DIR" in
  "$REPOSITORY_ROOT"/dist/release/*) ;;
  *)
    echo "error: refusing to clean unexpected release path: $OUTPUT_DIR" >&2
    exit 1
    ;;
esac
rm -rf -- "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"

copy_glob() {
  local pattern=$1
  local found=0
  local artifact
  while IFS= read -r artifact; do
    [[ -n "$artifact" ]] || continue
    found=1
    cp -p "$artifact" "$OUTPUT_DIR/"
  done < <(compgen -G "$pattern" || true)
  [[ $found == 1 ]]
}

bundle_requested() {
  [[ ",$BUNDLES," == *",$1,"* ]]
}

if [[ "$TARGET_TRIPLE" == *-apple-darwin ]]; then
  if bundle_requested dmg; then
    copy_glob "$BUNDLE_ROOT/dmg/*.dmg"
  fi
  if bundle_requested app; then
    APP_PATH=$(compgen -G "$BUNDLE_ROOT/macos/*.app" | head -n 1 || true)
    if [[ -z "$APP_PATH" || ! -d "$APP_PATH" ]]; then
      echo "error: Tauri did not produce an application bundle" >&2
      exit 1
    fi
    APP_ZIP="$OUTPUT_DIR/OpenFlow_${VERSION}_${TARGET_TRIPLE}.app.zip"
    if command -v ditto >/dev/null; then
      ditto -c -k --sequesterRsrc --keepParent "$APP_PATH" "$APP_ZIP"
    else
      echo "error: ditto is required to preserve the signed macOS application" >&2
      exit 1
    fi
  fi
else
  if bundle_requested appimage; then
    copy_glob "$BUNDLE_ROOT/appimage/*.AppImage"
  fi
  if bundle_requested deb; then
    copy_glob "$BUNDLE_ROOT/deb/*.deb"
  fi
  if bundle_requested rpm; then
    copy_glob "$BUNDLE_ROOT/rpm/*.rpm"
  fi
  if [[ "${OPENFLOW_NATIVE_MOCK:-0}" != 1 ]]; then
    "$SCRIPT_DIR/build-server-bundle.sh" "$TARGET_TRIPLE" "$OUTPUT_DIR"
  fi
fi

if command -v syft >/dev/null; then
  syft "dir:$OUTPUT_DIR" -o "spdx-json=$OUTPUT_DIR/OpenFlow_${VERSION}_${TARGET_TRIPLE}.spdx.json"
else
  echo "note: syft is unavailable; CI will generate the release SBOM" >&2
fi

"$SCRIPT_DIR/write-checksums.sh" "$OUTPUT_DIR"
echo "Release artifacts are in $OUTPUT_DIR"
