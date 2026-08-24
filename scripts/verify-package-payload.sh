#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPOSITORY_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd)
TARGET_TRIPLE=${1:?usage: scripts/verify-package-payload.sh TARGET_TRIPLE BUNDLE_ROOT BUNDLES}
BUNDLE_ROOT=${2:?usage: scripts/verify-package-payload.sh TARGET_TRIPLE BUNDLE_ROOT BUNDLES}
BUNDLES=${3:?usage: scripts/verify-package-payload.sh TARGET_TRIPLE BUNDLE_ROOT BUNDLES}
TEMPORARY_DIRECTORY=
SERVER_PID=

cleanup() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ -n "$TEMPORARY_DIRECTORY" && -d "$TEMPORARY_DIRECTORY" ]]; then
    rm -rf -- "$TEMPORARY_DIRECTORY"
  fi
}
trap cleanup EXIT

bundle_requested() {
  [[ ",$BUNDLES," == *",$1,"* ]]
}

one_match() {
  local pattern=$1
  local matches=()
  while IFS= read -r match; do
    [[ -n "$match" ]] && matches+=("$match")
  done < <(compgen -G "$pattern" || true)
  if [[ ${#matches[@]} -ne 1 ]]; then
    echo "error: expected exactly one package matching $pattern, found ${#matches[@]}" >&2
    exit 1
  fi
  printf '%s\n' "${matches[0]}"
}

require_payload_file() {
  local root=$1
  local name=$2
  local matches=()
  while IFS= read -r path; do
    matches+=("$path")
  done < <(find "$root" -type f -name "$name" -print)
  if [[ ${#matches[@]} -ne 1 ]]; then
    echo "error: package must contain exactly one $name, found ${#matches[@]}" >&2
    exit 1
  fi
  printf '%s\n' "${matches[0]}"
}

smoke_installed_payload() {
  local root=$1
  local server
  local asr
  local llm
  local sidecar_directory
  server=$(require_payload_file "$root" openflow-server)
  asr=$(require_payload_file "$root" openflow-asr-worker)
  llm=$(require_payload_file "$root" openflow-llm-worker)
  for binary in "$server" "$asr" "$llm"; do
    if [[ ! -x "$binary" ]]; then
      echo "error: packaged sidecar is not executable: $binary" >&2
      exit 1
    fi
  done
  sidecar_directory=$(dirname -- "$server")
  if ! [[ "$sidecar_directory" == "$(dirname -- "$asr")" && \
    "$sidecar_directory" == "$(dirname -- "$llm")" ]]; then
    echo "error: packaged server and workers are not sibling executables" >&2
    exit 1
  fi

  require_payload_file "$root" LICENSE >/dev/null
  require_payload_file "$root" THIRD_PARTY_NOTICES.md >/dev/null
  require_payload_file "$root" THIRD_PARTY_LICENSES.txt >/dev/null

  if [[ "${OPENFLOW_NATIVE_MOCK:-0}" == 1 ]]; then
    python3 "$REPOSITORY_ROOT/native/tests/worker_protocol_test.py" "$asr" "$llm"
  else
    OPENFLOW_EXPECT_REAL_BACKENDS=1 \
      python3 "$REPOSITORY_ROOT/native/tests/worker_protocol_test.py" "$asr" "$llm"
  fi

  local smoke_port=${OPENFLOW_PACKAGE_SMOKE_PORT:-38765}
  local bootstrap="$TEMPORARY_DIRECTORY/bootstrap.json"
  local server_log="$TEMPORARY_DIRECTORY/server.log"
  OPENFLOW_BIND="127.0.0.1:$smoke_port" \
    OPENFLOW_AUTH_STORE="$TEMPORARY_DIRECTORY/auth.json" \
    OPENFLOW_MODEL_CACHE="$TEMPORARY_DIRECTORY/models" \
    OPENFLOW_ROTATE_BOOTSTRAP_ADMIN_TOKEN=1 \
    OPENFLOW_WORKER_BACKEND=mock \
    "$server" >"$bootstrap" 2>"$server_log" &
  SERVER_PID=$!
  local ready=0
  for _ in {1..100}; do
    if curl -fsS "http://127.0.0.1:$smoke_port/health" | grep -q '"status":"ok"'; then
      ready=1
      break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
      echo "error: packaged server exited during startup" >&2
      sed -n '1,120p' "$server_log" >&2
      exit 1
    fi
    sleep 0.1
  done
  if [[ $ready != 1 ]]; then
    echo "error: packaged server did not answer its health check" >&2
    sed -n '1,120p' "$server_log" >&2
    exit 1
  fi
  node -e '
    const fs = require("fs");
    const lines = fs.readFileSync(process.argv[1], "utf8").trim().split("\n");
    if (lines.length !== 1) throw new Error("expected one bootstrap envelope");
    const value = JSON.parse(lines[0]);
    if (value.event !== "bootstrap" || typeof value.admin_token !== "string" || value.admin_token.length < 24) {
      throw new Error("invalid bootstrap envelope");
    }
  ' "$bootstrap"
  kill "$SERVER_PID"
  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=
}

TEMPORARY_DIRECTORY=$(mktemp -d "${TMPDIR:-/tmp}/openflow-package-smoke.XXXXXXXX")

case "$TARGET_TRIPLE" in
  *-apple-darwin)
    app=$(one_match "$BUNDLE_ROOT/macos/*.app")
    smoke_installed_payload "$app"
    ;;
  *-unknown-linux-gnu)
    if bundle_requested appimage; then
      appimage=$(one_match "$BUNDLE_ROOT/appimage/*.AppImage")
      if [[ ! -x "$appimage" ]]; then
        echo "error: AppImage is not executable: $appimage" >&2
        exit 1
      fi
    fi
    if bundle_requested deb; then
      deb=$(one_match "$BUNDLE_ROOT/deb/*.deb")
      deb=$(realpath "$deb")
      deb_root="$TEMPORARY_DIRECTORY/deb"
      mkdir -p "$deb_root"
      if command -v dpkg-deb >/dev/null; then
        dpkg-deb -x "$deb" "$deb_root"
        deb_dependencies=$(dpkg-deb -f "$deb" Depends)
      elif command -v ar >/dev/null; then
        deb_archive="$TEMPORARY_DIRECTORY/deb-archive"
        deb_control="$TEMPORARY_DIRECTORY/deb-control"
        mkdir -p "$deb_archive" "$deb_control"
        (cd "$deb_archive" && ar x "$deb")
        data_archive=$(one_match "$deb_archive/data.tar.*")
        control_archive=$(one_match "$deb_archive/control.tar.*")
        tar -xf "$data_archive" -C "$deb_root"
        tar -xf "$control_archive" -C "$deb_control"
        deb_dependencies=$(sed -n 's/^Depends: //p' "$deb_control/control")
      else
        echo "error: dpkg-deb or ar is required to verify the Debian package payload" >&2
        exit 1
      fi
      for dependency in libasound2 libgomp1; do
        if [[ ",$deb_dependencies," != *"$dependency"* ]]; then
          echo "error: Debian package does not declare runtime dependency $dependency" >&2
          exit 1
        fi
      done
      smoke_installed_payload "$deb_root"
    fi
    if bundle_requested rpm; then
      if ! command -v rpm >/dev/null; then
        echo "error: rpm is required to verify the RPM package payload" >&2
        exit 1
      fi
      rpm_package=$(one_match "$BUNDLE_ROOT/rpm/*.rpm")
      for path in \
        /usr/bin/openflow-desktop \
        /usr/bin/openflow-server \
        /usr/bin/openflow-asr-worker \
        /usr/bin/openflow-llm-worker; do
        if ! rpm -qlp "$rpm_package" | grep -Fxq "$path"; then
          echo "error: RPM payload is missing $path" >&2
          exit 1
        fi
      done
      if ! rpm -qlp "$rpm_package" | grep -Eq '/THIRD_PARTY_LICENSES\.txt$'; then
        echo "error: RPM payload is missing third-party licenses" >&2
        exit 1
      fi
      rpm_dependencies=$(rpm -qp --requires "$rpm_package")
      for dependency in alsa-lib libgomp vulkan-loader; do
        if ! grep -Fxq "$dependency" <<<"$rpm_dependencies"; then
          echo "error: RPM package does not declare runtime dependency $dependency" >&2
          exit 1
        fi
      done
    fi
    ;;
  *)
    echo "error: unsupported package target: $TARGET_TRIPLE" >&2
    exit 2
    ;;
esac

echo "Verified installer payloads for $TARGET_TRIPLE"
