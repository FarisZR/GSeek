#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
arch="${1:-$(uname -m)}"
case "$arch" in x86_64|aarch64) ;; *) exit 2;; esac
test -f target/release/gseek
test -f dist/runtime/cpu/gseek-inference
test -f dist/runtime/vulkan/gseek-inference
bundle=dist/package/gseek
rm -rf dist/package
mkdir -p "$bundle/bin" "$bundle/runtime" "$bundle/licenses"
cp target/release/gseek "$bundle/bin/gseek-native"
cp scripts/gseek-launcher "$bundle/bin/gseek"
chmod 755 "$bundle/bin/gseek"
cp -a dist/runtime/cpu dist/runtime/vulkan "$bundle/runtime/"
cp LICENSE "$bundle/licenses/GSeek-GPL-3.0"
cp docs/THIRD_PARTY.md "$bundle/licenses/THIRD_PARTY.md"
if [[ -f dist/runtime-manifest.json ]]; then cp dist/runtime-manifest.json "$bundle/"; fi
tar -C dist/package -czf "dist/gseek-linux-$arch.tar.gz" gseek
sha256sum "dist/gseek-linux-$arch.tar.gz"
