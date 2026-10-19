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
mkdir -p .flatpak-inference/readers
root=$(realpath .flatpak-inference)
python3 tests/flatpak-readers.py "$root/readers"
flatpak run --user --filesystem="$root" --command=sh "$app" -c '
  set -eu
  cd "$1/readers"
  pdfinfo sample.pdf
  pdftotext sample.pdf - | grep "GSeek PDF reader"
  pdftoppm -scale-to 128 -singlefile -png sample.pdf page
  ffmpeg -nostdin -v error -y -i page.png -frames:v 1 image.ppm
  ffmpeg -nostdin -v error -y -f lavfi -i sine=frequency=440:duration=1 -ar 16000 audio.wav
  ffprobe -v error -show_entries stream=codec_type audio.wav | grep audio
  unzip -p sample.docx word/document.xml | grep "GSeek Office reader"
' sh "$root"
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
