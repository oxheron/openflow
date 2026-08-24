#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
PROFILE=rocm

usage() {
  cat <<'USAGE'
Usage: scripts/build-arch-bundle.sh [--profile rocm|vulkan|cpu]

Builds an unprivileged, native Arch Linux application directory and .tar.zst.
ROCm targets can be pinned with OPENFLOW_AMDGPU_TARGETS (for example gfx1102).
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile)
      PROFILE=${2:?--profile needs a value}
      shift 2
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

case "$PROFILE" in
  rocm | vulkan | cpu) ;;
  *)
    echo "error: Arch profile must be rocm, vulkan, or cpu" >&2
    exit 2
    ;;
esac

if [[ ! -r /etc/os-release ]] || ! grep -Eq '^ID=arch$' /etc/os-release; then
  echo "error: this native bundle builder is intended for Arch Linux" >&2
  exit 2
fi

required_packages=(
  alsa-lib
  base-devel
  cmake
  libappindicator
  nodejs
  npm
  rustup
  webkit2gtk-4.1
  zstd
)
case "$PROFILE" in
  rocm)
    required_packages+=(hip-runtime-amd hipblas rocblas rocm-hip-sdk)
    ;;
  vulkan)
    required_packages+=(shaderc spirv-headers vulkan-headers vulkan-icd-loader)
    ;;
esac

missing=$(pacman -T "${required_packages[@]}" 2>/dev/null || true)
if [[ -n "$missing" ]]; then
  echo "error: install the missing Arch build/runtime packages first:" >&2
  # pacman -T emits one package per line; translating newlines to spaces keeps
  # the suggested command directly reusable without evaluating its contents.
  echo "  sudo pacman -S --needed $(tr '\n' ' ' <<<"$missing")" >&2
  exit 2
fi

if [[ "$PROFILE" == rocm && -z "${OPENFLOW_AMDGPU_TARGETS:-}" ]]; then
  detected_target=$(rocminfo 2>/dev/null | awk '$1 == "Name:" && $2 ~ /^gfx[0-9]+$/ { print $2; exit }')
  if [[ -z "$detected_target" ]]; then
    echo "error: ROCm could not identify this GPU" >&2
    echo "  verify /dev/kfd access with rocminfo, or set OPENFLOW_AMDGPU_TARGETS=gfxNNNN" >&2
    exit 2
  fi
  export OPENFLOW_AMDGPU_TARGETS=$detected_target
  echo "ROCm target: $OPENFLOW_AMDGPU_TARGETS"
fi

TARGET_TRIPLE=$(rustc --print host-tuple)
if [[ "$TARGET_TRIPLE" != x86_64-unknown-linux-gnu ]]; then
  echo "error: the initial Arch bundle currently supports x86_64, not $TARGET_TRIPLE" >&2
  exit 2
fi
VERSION=$(node -p "require('$REPOSITORY_ROOT/apps/desktop/package.json').version")
BUILD_ROOT=${OPENFLOW_NATIVE_BUILD_DIR:-"$REPOSITORY_ROOT/target/arch-native/$PROFILE"}
STAGE_DIR=${OPENFLOW_SIDECAR_STAGE_DIR:-"$REPOSITORY_ROOT/target/arch-sidecars/$PROFILE"}
OUTPUT_ROOT="$REPOSITORY_ROOT/dist/arch"
BUNDLE_DIR="$OUTPUT_ROOT/OpenFlow-$VERSION-$PROFILE-x86_64"
ARCHIVE="$OUTPUT_ROOT/OpenFlow-$VERSION-$PROFILE-x86_64.tar.zst"

env \
  OPENFLOW_INFERENCE_PROFILE="$PROFILE" \
  OPENFLOW_NATIVE_BUILD_DIR="$BUILD_ROOT" \
  OPENFLOW_SIDECAR_STAGE_DIR="$STAGE_DIR" \
  "$SCRIPT_DIR/stage-sidecars.sh" "$TARGET_TRIPLE"

npm --prefix "$REPOSITORY_ROOT/apps/desktop" ci
if [[ "${OPENFLOW_NATIVE_MOCK:-0}" == 1 ]]; then
  echo "error: Arch user bundles cannot be built with mock inference workers" >&2
  exit 2
fi
"$SCRIPT_DIR/generate-third-party-licenses.mjs" --write
"$SCRIPT_DIR/generate-third-party-licenses.mjs" --check
npm --prefix "$REPOSITORY_ROOT/apps/desktop" run build
cargo build --locked --release --target "$TARGET_TRIPLE" -p openflow-desktop

case "$BUNDLE_DIR" in
  "$OUTPUT_ROOT"/OpenFlow-*) ;;
  *)
    echo "error: refusing to replace unexpected Arch bundle path: $BUNDLE_DIR" >&2
    exit 1
    ;;
esac
rm -rf -- "$BUNDLE_DIR"
rm -f -- "$ARCHIVE" "$ARCHIVE.sha256"
mkdir -p "$BUNDLE_DIR/share/icons/hicolor/128x128/apps"

install -m 0755 \
  "$REPOSITORY_ROOT/target/$TARGET_TRIPLE/release/openflow-desktop" \
  "$BUNDLE_DIR/openflow-desktop"
for name in openflow-server openflow-asr-worker openflow-llm-worker; do
  install -m 0755 "$STAGE_DIR/$name-$TARGET_TRIPLE" "$BUNDLE_DIR/$name"
done
install -m 0644 "$REPOSITORY_ROOT/LICENSE" "$BUNDLE_DIR/LICENSE"
install -m 0644 \
  "$REPOSITORY_ROOT/packaging/THIRD_PARTY_NOTICES.md" \
  "$BUNDLE_DIR/THIRD_PARTY_NOTICES.md"
install -m 0644 \
  "$REPOSITORY_ROOT/packaging/THIRD_PARTY_LICENSES.txt" \
  "$BUNDLE_DIR/THIRD_PARTY_LICENSES.txt"
install -m 0644 "$REPOSITORY_ROOT/packaging/arch/README.md" "$BUNDLE_DIR/README.md"
install -m 0644 "$REPOSITORY_ROOT/packaging/arch/openflow.desktop" "$BUNDLE_DIR/openflow.desktop"
install -m 0755 "$REPOSITORY_ROOT/packaging/server/openflow-host" "$BUNDLE_DIR/openflow-host"
install -m 0644 \
  "$REPOSITORY_ROOT/apps/desktop/src-tauri/icons/128x128.png" \
  "$BUNDLE_DIR/share/icons/hicolor/128x128/apps/openflow.png"

tar --zstd -cf "$ARCHIVE" -C "$OUTPUT_ROOT" "$(basename "$BUNDLE_DIR")"
(
  cd "$OUTPUT_ROOT"
  sha256sum "$(basename "$ARCHIVE")" >"$(basename "$ARCHIVE").sha256"
)
echo "Arch bundle: $ARCHIVE"
