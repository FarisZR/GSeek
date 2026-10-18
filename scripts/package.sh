#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
arch="${1:-$(uname -m)}"
case "$arch" in x86_64|aarch64) ;; *) exit 2;; esac
test -f target/release/gseek
bundle=dist/package/gseek
rm -rf dist/package
mkdir -p "$bundle/bin" "$bundle/licenses"
cp target/release/gseek "$bundle/bin/gseek-native"
cp scripts/gseek-launcher "$bundle/bin/gseek"
chmod 755 "$bundle/bin/gseek"
cp LICENSE "$bundle/licenses/GSeek-GPL-3.0"
cp docs/THIRD_PARTY.md "$bundle/licenses/THIRD_PARTY.md"
cp data/icons/COPYING-LGPL-3.0 "$bundle/licenses/Adwaita-LGPL-3.0"
cp data/io.github.FarisZR.GSeek.ini dist/package/gseek-search-provider.ini
cargo metadata --locked --format-version 1 | python3 scripts/dependency-notices.py > "$bundle/licenses/RUST-DEPENDENCIES.txt"
tar -C dist/package -czf "dist/gseek-linux-$arch.tar.gz" gseek gseek-search-provider.ini
sha256sum "dist/gseek-linux-$arch.tar.gz"
