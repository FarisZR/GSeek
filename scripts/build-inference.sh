#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
backend="${1:-cpu}"
case "$backend" in cpu|vulkan|cuda|rocm) ;; *) echo 'Expected cpu, vulkan, cuda, or rocm' >&2; exit 2;; esac
source_dir="${LLAMA_SOURCE:-$PWD/build/llama.cpp}"
commit=4d756bc72bf00a4aacf410ae15a2d315f3db400d
if [[ ! -d "$source_dir/.git" ]]; then
  git clone --depth 1 --branch b11461 https://github.com/ggml-org/llama.cpp "$source_dir"
fi
test "$(git -C "$source_dir" rev-parse HEAD)" = "$commit"
patch="$PWD/inference/embedding-memory.patch"
if git -C "$source_dir" apply --check "$patch" 2>/dev/null; then
  git -C "$source_dir" apply "$patch"
else
  git -C "$source_dir" apply --reverse --check "$patch"
fi
args=()
if [[ $(uname -m) == aarch64 && -z "${CC:-}" ]]; then
  # CPU variants include ARMv9.2/SME, which GCC 13 cannot compile. Pick a compiler
  # that accepts those instructions; the runtime still selects the host's actual ISA.
  for pair in gcc-14:g++-14 clang:clang++; do
    if command -v "${pair%%:*}" >/dev/null && command -v "${pair##*:}" >/dev/null &&
      echo 'int main(void) { return 0; }' | "${pair%%:*}" -x c -march=armv9.2-a+sme -c -o /dev/null - 2>/dev/null; then
      export CC=${pair%%:*} CXX=${pair##*:}
      args+=("-DCMAKE_C_COMPILER=$CC" "-DCMAKE_CXX_COMPILER=$CXX")
      break
    fi
  done
fi
case "$backend" in
  vulkan) args+=(-DGGML_VULKAN=ON);;
  cuda) args+=(-DGGML_CUDA=ON "-DCMAKE_CUDA_ARCHITECTURES=${CUDA_ARCHS:-75;80;86;89;90}");;
  rocm) args+=(-DGGML_HIP=ON "-DAMDGPU_TARGETS=${AMDGPU_TARGETS:-gfx1030;gfx1100;gfx1101;gfx1102;gfx1200;gfx1201}");;
esac
cmake -S inference -B "build/inference-$backend" -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DLLAMA_SOURCE="$source_dir" -DCMAKE_INSTALL_RPATH='$ORIGIN' "${args[@]}"
cmake --build "build/inference-$backend" --parallel "${BUILD_JOBS:-4}" --target gseek-inference
out="dist/runtime/$backend"
mkdir -p "$out"
cp "build/inference-$backend/gseek-inference" "$out/"
cp -a "build/inference-$backend/bin/"*.so* "$out/"
cp "$source_dir/LICENSE" "$out/llama.cpp-LICENSE"
cp inference/LICENSE "$out/GSeek-helper-LICENSE"
mkdir -p "$out/notices"
find "$source_dir/vendor" "$source_dir/ggml" -type f \( -iname '*license*' -o -iname '*copying*' \) -print0 |
  while IFS= read -r -d '' notice; do
    relative="${notice#"$source_dir/"}"
    mkdir -p "$out/notices/$(dirname "$relative")"
    cp "$notice" "$out/notices/$relative"
  done
printf '%s\n' "$commit" > "$out/llama.cpp-COMMIT"
