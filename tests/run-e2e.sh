#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ "${GSEEK_TEST_FLATPAK:-}" == 1 ]]; then
  export FLATPAK_USER_DIR="${FLATPAK_USER_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/flatpak}"
fi
root=$(mktemp -d -t gseek-e2e-XXXXXXXX)
cleanup() {
  if [[ -n "${session_pid:-}" ]]; then
    # Shell can activate desktop helpers (for example Evolution). dbus-run-session
    # does not wait for those processes when its main client exits. Keep all of
    # them in a private process group and stop it before removing their XDG data.
    kill -TERM -- "-$session_pid" 2>/dev/null || true
    for _ in {1..40}; do
      kill -0 -- "-$session_pid" 2>/dev/null || break
      sleep 0.05
    done
    kill -KILL -- "-$session_pid" 2>/dev/null || true
  fi
  if [[ "${GSEEK_TEST_FLATPAK:-}" == 1 ]]; then
    python3 tests/flatpak-data.py "$root" --reset
    flatpak override --user --reset com.fariszr.gseek
  fi
  rm -rf "$root"
}
trap cleanup EXIT
export GSEEK_TEST_ROOT="$root"
export XDG_DATA_HOME="$root/data" XDG_CONFIG_HOME="$root/config" XDG_CACHE_HOME="$root/cache"
export XDG_RUNTIME_DIR="$root/runtime"
mkdir -m700 "$XDG_RUNTIME_DIR"
export GSETTINGS_BACKEND="${GSETTINGS_BACKEND:-memory}"
export GSEEK_BINARY="${GSEEK_BINARY:-$PWD/target/debug/gseek}"
mkdir -p "$XDG_DATA_HOME/gseek" "$XDG_CONFIG_HOME/gseek" "$XDG_CACHE_HOME/gseek"
# Native development fixtures use the same static export assets as the Flatpak.
# Installation itself is tested separately through flatpak install/build-export.
if [[ "${GSEEK_TEST_FLATPAK:-}" == 1 ]]; then
  mkdir -p "$XDG_CONFIG_HOME/xdg-desktop-portal"
  printf '[preferred]\ndefault=gtk\n' > "$XDG_CONFIG_HOME/xdg-desktop-portal/portals.conf"
  python3 tests/flatpak-fixture.py
  export GSEEK_BINARY="$root/gseek"
else
  python3 tests/install-fixture.py
fi
export XDG_DATA_DIRS="$root/system/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
if [[ -n "${GSEEK_TEST_MODEL_CACHE:-}" ]]; then
  mkdir -p "$GSEEK_TEST_MODEL_CACHE"
  ln -s "$GSEEK_TEST_MODEL_CACHE" "$XDG_DATA_HOME/gseek/models"
fi
if [[ -n "${GSEEK_TEST_RUNTIME_CACHE:-}" ]]; then
  mkdir -p "$GSEEK_TEST_RUNTIME_CACHE"
  ln -s "$GSEEK_TEST_RUNTIME_CACHE" "$XDG_DATA_HOME/gseek/runtimes"
fi
setsid dbus-run-session -- "${GSEEK_TEST_PYTHON:-/usr/bin/python3}" tests/e2e.py "$@" &
session_pid=$!
wait "$session_pid"
