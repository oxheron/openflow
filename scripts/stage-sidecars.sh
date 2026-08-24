#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
TARGET_TRIPLE=${1:-$(rustc --print host-tuple)}
PROFILE=${OPENFLOW_INFERENCE_PROFILE:-auto}
UPSTREAM_DIR=${OPENFLOW_UPSTREAM_DIR:-"$REPOSITORY_ROOT/target/release-upstream"}
STAGE_DIR=${OPENFLOW_SIDECAR_STAGE_DIR:-"$REPOSITORY_ROOT/apps/desktop/src-tauri/binaries"}
BUILD_ROOT=${OPENFLOW_NATIVE_BUILD_DIR:-"$REPOSITORY_ROOT/target/release-native/$TARGET_TRIPLE"}
ASR_BUILD_ROOT=${OPENFLOW_NATIVE_ASR_BUILD_DIR:-"$BUILD_ROOT/asr"}
LLM_BUILD_ROOT=${OPENFLOW_NATIVE_LLM_BUILD_DIR:-"$BUILD_ROOT/llm"}
MOCK_NATIVE=${OPENFLOW_NATIVE_MOCK:-0}

case "$TARGET_TRIPLE" in
  *-apple-darwin)
    HOST_OS=macos
    ;;
  *-unknown-linux-gnu)
    HOST_OS=linux
    ;;
  *)
    echo "error: unsupported release target: $TARGET_TRIPLE" >&2
    exit 2
    ;;
esac

if [[ "$PROFILE" == auto ]]; then
  if [[ "$HOST_OS" == macos ]]; then
    PROFILE=metal
  else
    PROFILE=vulkan
  fi
fi

case "$PROFILE" in
  cpu | metal | vulkan | cuda | rocm) ;;
  *)
    echo "error: inference profile must be auto, cpu, metal, vulkan, cuda, or rocm" >&2
    exit 2
    ;;
esac
if [[ "$HOST_OS" != macos && "$PROFILE" == metal ]]; then
  echo "error: the Metal profile requires macOS" >&2
  exit 2
fi
if [[ "$HOST_OS" != linux && "$PROFILE" == rocm ]]; then
  echo "error: the ROCm profile requires Linux" >&2
  exit 2
fi

if [[ "$MOCK_NATIVE" == 1 ]]; then
  WHISPER_DIR=
  LLAMA_DIR=
else
  if [[ -n "${OPENFLOW_WHISPER_CPP_DIR:-}" || -n "${OPENFLOW_LLAMA_CPP_DIR:-}" ]]; then
    if [[ -z "${OPENFLOW_WHISPER_CPP_DIR:-}" || -z "${OPENFLOW_LLAMA_CPP_DIR:-}" ]]; then
      echo "error: both OPENFLOW_WHISPER_CPP_DIR and OPENFLOW_LLAMA_CPP_DIR are required" >&2
      exit 2
    fi
    WHISPER_DIR=$OPENFLOW_WHISPER_CPP_DIR
    LLAMA_DIR=$OPENFLOW_LLAMA_CPP_DIR
  else
    "$SCRIPT_DIR/fetch-inference-sources.sh" "$UPSTREAM_DIR"
    WHISPER_DIR=$UPSTREAM_DIR/whisper.cpp
    LLAMA_DIR=$UPSTREAM_DIR/llama.cpp
  fi
fi

CMAKE_COMMON_ARGUMENTS=(
  -S "$REPOSITORY_ROOT/native"
  -DCMAKE_BUILD_TYPE=Release
  -DOPENFLOW_BUILD_TESTS=OFF
  -DBUILD_SHARED_LIBS=OFF
  -DGGML_CCACHE=OFF
  -DGGML_NATIVE=OFF
)
CMAKE_PROFILE_ARGUMENTS=()

case "$PROFILE" in
  cpu)
    CMAKE_PROFILE_ARGUMENTS+=(
      -DGGML_METAL=OFF
      -DGGML_VULKAN=OFF
      -DGGML_CUDA=OFF
      -DGGML_HIP=OFF
    )
    ;;
  metal)
    CMAKE_PROFILE_ARGUMENTS+=(
      -DGGML_METAL=ON
      -DGGML_METAL_EMBED_LIBRARY=ON
      -DGGML_VULKAN=OFF
      -DGGML_CUDA=OFF
      -DGGML_HIP=OFF
    )
    ;;
  vulkan)
    CMAKE_PROFILE_ARGUMENTS+=(
      -DGGML_METAL=OFF
      -DGGML_VULKAN=ON
      -DGGML_CUDA=OFF
      -DGGML_HIP=OFF
    )
    ;;
  cuda)
    CMAKE_PROFILE_ARGUMENTS+=(
      -DGGML_METAL=OFF
      -DGGML_VULKAN=OFF
      -DGGML_CUDA=ON
      -DGGML_HIP=OFF
    )
    ;;
  rocm)
    CMAKE_PROFILE_ARGUMENTS+=(
      -DGGML_METAL=OFF
      -DGGML_VULKAN=OFF
      -DGGML_CUDA=OFF
      -DGGML_HIP=ON
    )
    if [[ -n "${OPENFLOW_AMDGPU_TARGETS:-}" ]]; then
      CMAKE_PROFILE_ARGUMENTS+=("-DGPU_TARGETS=$OPENFLOW_AMDGPU_TARGETS")
    fi
    ;;
esac

if [[ "$HOST_OS" == macos ]]; then
  case "$TARGET_TRIPLE" in
    aarch64-apple-darwin) CMAKE_COMMON_ARGUMENTS+=(-DCMAKE_OSX_ARCHITECTURES=arm64) ;;
    x86_64-apple-darwin) CMAKE_COMMON_ARGUMENTS+=(-DCMAKE_OSX_ARCHITECTURES=x86_64) ;;
  esac
else
  # The distribution may rely on glibc and the system GPU driver/loader, but
  # should not depend on the build machine's libstdc++ or libgcc versions.
  CMAKE_COMMON_ARGUMENTS+=("-DCMAKE_EXE_LINKER_FLAGS=-static-libgcc -static-libstdc++")
fi

if [[ "$MOCK_NATIVE" == 1 ]]; then
  cmake "${CMAKE_COMMON_ARGUMENTS[@]}" -B "$BUILD_ROOT"
  cmake --build "$BUILD_ROOT" \
    --config Release \
    --parallel "${OPENFLOW_BUILD_JOBS:-2}" \
    --target openflow-asr-worker openflow-llm-worker
  ASR_BINARY=$BUILD_ROOT/openflow-asr-worker
  LLM_BINARY=$BUILD_ROOT/openflow-llm-worker
else
  # Each upstream carries its own ggml revision. Isolated CMake graphs prevent
  # one project from accidentally compiling against the other's ggml headers.
  cmake \
    "${CMAKE_COMMON_ARGUMENTS[@]}" \
    "${CMAKE_PROFILE_ARGUMENTS[@]}" \
    -B "$ASR_BUILD_ROOT" \
    -DOPENFLOW_ENABLE_WHISPER_CPP=ON \
    "-DOPENFLOW_WHISPER_CPP_DIR=$WHISPER_DIR" \
    -DOPENFLOW_ENABLE_LLAMA_CPP=OFF
  cmake --build "$ASR_BUILD_ROOT" \
    --config Release \
    --parallel "${OPENFLOW_BUILD_JOBS:-2}" \
    --target openflow-asr-worker

  cmake \
    "${CMAKE_COMMON_ARGUMENTS[@]}" \
    "${CMAKE_PROFILE_ARGUMENTS[@]}" \
    -B "$LLM_BUILD_ROOT" \
    -DOPENFLOW_ENABLE_WHISPER_CPP=OFF \
    -DOPENFLOW_ENABLE_LLAMA_CPP=ON \
    "-DOPENFLOW_LLAMA_CPP_DIR=$LLAMA_DIR"
  cmake --build "$LLM_BUILD_ROOT" \
    --config Release \
    --parallel "${OPENFLOW_BUILD_JOBS:-2}" \
    --target openflow-llm-worker
  ASR_BINARY=$ASR_BUILD_ROOT/openflow-asr-worker
  LLM_BINARY=$LLM_BUILD_ROOT/openflow-llm-worker
fi

cargo build --locked --release --target "$TARGET_TRIPLE" -p openflow-server

mkdir -p "$STAGE_DIR"
stage_binary() {
  local source=$1
  local name=$2
  local destination="$STAGE_DIR/$name-$TARGET_TRIPLE"
  install -m 0755 "$source" "$destination"
  echo "staged $destination"
}

stage_binary \
  "$REPOSITORY_ROOT/target/$TARGET_TRIPLE/release/openflow-server" \
  openflow-server
stage_binary "$ASR_BINARY" openflow-asr-worker
stage_binary "$LLM_BINARY" openflow-llm-worker

OPENFLOW_EXPECT_COMPUTE_BACKEND=$PROFILE \
  "$SCRIPT_DIR/verify-sidecars.sh" "$TARGET_TRIPLE" "$STAGE_DIR"
