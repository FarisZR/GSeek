#!/usr/bin/env bash
# Test the installed application, not flatpak-builder's development sandbox.
set -euo pipefail
app=com.fariszr.gseek
flatpak run --user "$app" --version
flatpak run --user --command=sh "$app" -c '
  set -eu
  for tool in ffmpeg ffprobe unzip pdfinfo pdftotext pdftoppm; do command -v "$tool"; done
  for library in /app/bin/gseek /app/lib/libpoppler.so.*; do
    if ldd "$library" | grep "not found"; then exit 1; fi
  done
  test -e /usr/lib/"$(uname -m)"-linux-gnu/libtinysparql-3.0.so.0
  test -e /app/share/gnome-shell/search-providers/com.fariszr.gseek.ini
'
if [[ "${GSEEK_TEST_INFERENCE:-false}" != true ]]; then exit 0; fi
# Retain the verified download cache between CI runs; no model is bundled.
mkdir -p .flatpak-inference/{data/gseek,config/gseek,cache/gseek}
root=$(realpath .flatpak-inference)
printf '{"backend":"cpu"}\n' > "$root/config/gseek/settings.json"
args=(--user --filesystem="$root" --env=XDG_DATA_HOME="$root/data" --env=XDG_CONFIG_HOME="$root/config" --env=XDG_CACHE_HOME="$root/cache")
flatpak run "${args[@]}" "$app" prepare-model
flatpak run "${args[@]}" "$app" embed 'task: search result | query: the clause about pets in the lease' > "$root/vector.json"
python3 - "$root/vector.json" <<'PY'
import json, math, sys
actual = json.load(open(sys.argv[1]))
expected = json.load(open('data/reference-embedding.json'))
assert len(actual) == 768 and all(math.isfinite(v) for v in actual)
cosine = sum(a*b for a,b in zip(actual, expected))
assert cosine > .999, f'Flatpak inference reference drift: {cosine}'
print(f'Flatpak CPU reference cosine: {cosine:.6f}')
PY
