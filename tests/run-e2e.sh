#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
root=$(mktemp -d -t gseek-e2e-XXXXXXXX)
trap 'rm -rf "$root"' EXIT
export GSEEK_TEST_ROOT="$root"
export XDG_DATA_HOME="$root/data" XDG_CONFIG_HOME="$root/config" XDG_CACHE_HOME="$root/cache"
export GSETTINGS_BACKEND=memory
export GSEEK_BINARY="${GSEEK_BINARY:-$PWD/target/debug/gseek}"
export GSEEK_RUNTIME_DIR="${GSEEK_RUNTIME_DIR:-$PWD/dist/runtime}"
mkdir -p "$XDG_DATA_HOME/gseek" "$XDG_CONFIG_HOME/gseek" "$XDG_CACHE_HOME/gseek"
if [[ -n "${GSEEK_TEST_MODEL_CACHE:-}" ]]; then
  mkdir -p "$GSEEK_TEST_MODEL_CACHE"
  ln -s "$GSEEK_TEST_MODEL_CACHE" "$XDG_DATA_HOME/gseek/models"
fi
dbus-run-session -- "${GSEEK_TEST_PYTHON:-/usr/bin/python3}" tests/e2e.py
