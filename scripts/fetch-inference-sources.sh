#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
# shellcheck source=SCRIPTDIR/../packaging/upstream.env
source "$REPOSITORY_ROOT/packaging/upstream.env"

DESTINATION=${1:-"$REPOSITORY_ROOT/target/release-upstream"}
mkdir -p "$DESTINATION"

fetch_checkout() {
  local name=$1
  local repository=$2
  local release=$3
  local revision=$4
  local directory=$5

  if [[ -e "$directory" && ! -d "$directory/.git" ]]; then
    echo "error: $directory exists but is not a Git checkout" >&2
    return 1
  fi

  if [[ ! -d "$directory/.git" ]]; then
    git clone --filter=blob:none --no-checkout "$repository" "$directory"
  fi

  git -C "$directory" fetch --depth 1 origin "refs/tags/$release:refs/tags/$release"
  local commit
  commit=$(git -C "$directory" rev-list -n 1 "$release")
  if [[ "$commit" != "$revision" ]]; then
    echo "error: $name release $release resolved to unexpected commit $commit" >&2
    return 1
  fi
  git -C "$directory" checkout --detach --force "$revision"
  printf '%s=%s\n' "$name" "$commit"
}

{
  echo "# Resolved by scripts/fetch-inference-sources.sh"
  fetch_checkout \
    whisper.cpp \
    "$OPENFLOW_WHISPER_CPP_REPOSITORY" \
    "$OPENFLOW_WHISPER_CPP_RELEASE" \
    "$OPENFLOW_WHISPER_CPP_REVISION" \
    "$DESTINATION/whisper.cpp"
  fetch_checkout \
    llama.cpp \
    "$OPENFLOW_LLAMA_CPP_REPOSITORY" \
    "$OPENFLOW_LLAMA_CPP_RELEASE" \
    "$OPENFLOW_LLAMA_CPP_REVISION" \
    "$DESTINATION/llama.cpp"
} >"$DESTINATION/RESOLVED_REVISIONS"

echo "Inference sources are ready in $DESTINATION"
