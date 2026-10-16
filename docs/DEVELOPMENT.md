# Development

Use a recent Linux GNOME desktop, Rust 1.99.0 (pinned in `rust-toolchain.toml`),
GTK4/libadwaita and GLib development tools, TinySPARQL, and system media readers.
On Fedora 45 or Ubuntu 26.10, `scripts/ci-deps.sh` records the distribution packages
used by CI; run it only in a disposable development machine/container or install
the listed packages through your package manager.

## Build

```sh
cargo build --locked
cargo run -- prepare-model
```

`prepare-model` downloads and verifies the real 865 MB model. First inference
automatically installs the selected official prebuilt llama.cpp runtime. Install
`openssl3-libs` on Fedora or `libssl3t64` on Ubuntu for that runtime; the Homebrew
cask supplies `openssl@3` only if the host lacks the OpenSSL 3 ABI. No
inference compilation, GPU SDK, or manually configured server is needed.

The embedded `data/runtimes.json` pins official release URLs, byte sizes, and
SHA-256 digests for CPU, Vulkan, and CUDA on both architectures. To update the
runtime, use GitHub's release asset digests, update the pipeline fingerprint in
`src/model.rs`, and run the reference and multimodal tests before merging.
The cask declares no media formulas. Homebrew lifecycle tests assert an empty
formula dependency graph on the Ubuntu hosts; native tests use distro readers.

For offline development, `GSEEK_RUNTIME_DIR` can point to a directory containing
unmodified upstream distributions in `cpu/`, `vulkan/`, and/or `cuda/`.

For a development desktop install, place `data/io.github.FarisZR.GSeek.ini` in a
system `gnome-shell/search-providers` directory, then run `cargo run -- integrate`
and open `cargo run`. The system file requires administrator permission. The
isolated tests below create a private system data directory and need no root.

## Tests

```sh
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo test --locked --no-default-features --lib
python3 -m unittest discover -s tests -p 'test_*.py' -v
cargo test --locked --test real_inference -- --ignored --test-threads=1
cargo test --locked --lib inference::tests::worker_restarts_after_process_failure -- --ignored --test-threads=1
```

Real inference tests deliberately require the model. They check the fixed
embedding canary, image/audio vectors in the same space, video frame/audio readers,
and visual PDF pages. These tests never substitute mock vectors for inference.
For Vulkan set the backend in GSeek’s settings; CI uses Mesa’s software ICD to
exercise the real Vulkan path without assuming runner GPU hardware.

Run the actual D-Bus, TinySPARQL, and model integration suite:

```sh
GSEEK_TEST_MODEL_CACHE="$HOME/.local/share/gseek/models" \
GSEEK_GUI_TEST_COMMAND='cargo test --lib ui::tests::native_views -- --ignored --test-threads=1' \
GSEEK_SCREENSHOT_DIR="$PWD/artifacts/screenshots" \
xvfb-run -a bash tests/run-e2e.sh
```

The fixture owns a disposable LocalSearch endpoint backed by the real GNOME
TinySPARQL library and Nepomuk ontology. It isolates XDG paths and the session bus;
it does not change your running LocalSearch instance or index your files. Tests
cover actual multilingual retrieval, automatic file events, opening results,
provider metadata, pause/resume, exclusions, concurrent searches, desktop lifecycle,
GTK views, and inference failures. Native snapshots are generated from mapped GTK
widgets, including setup and a narrow window.

To test the **real GNOME overview**, add `GSEEK_TEST_SHELL=1` and
`GSETTINGS_BACKEND=keyfile` to that command. GNOME Shell must be installed and able
to create a headless Wayland display. The fixture uses a private session bus as its
system bus, with an isolated runtime directory. A test-only extension enables
Shell’s Eval/Screenshot API inside that disposable session; it is never packaged
with GSeek. The test verifies Shell discovers the provider and displays a result
from the real model, then captures `overview.png`.

CI executes these checks on GNOME 51 / Fedora 45 and Ubuntu 26.10, for CPU and
Vulkan. Software Vulkan is slower; CI permits a 300-second inference timeout.
Physical CUDA/AMD GPU validation remains a hardware task; green CI does not
establish GPU performance or support for every driver. Both architectures verify installation and dynamic linking of the official CPU,
Vulkan, and CUDA archives, including CUDA redistributables. No CI job builds an
inference engine or GPU SDK.

## Releases

A `vX.Y.Z` tag or a manual Release workflow builds the Rust application for
x86-64 and ARM64. Each architecture installs the pinned upstream CPU runtime and
runs the packaged encoder against the reference vector before upload. Assembly
preserves the exact tested packages, generates the Homebrew cask and `SHA256SUMS`,
and stages a **draft** GitHub release. Rust license inventory and third-party
notices travel with the package. Runtime downloads come directly from upstream.

Verify the staged assets, publish the release, then synchronize the existing
`FarisZR/homebrew-tap` cask. No custom Flatpak repository is created.
`python3 scripts/test-homebrew.py` exercises an exact local release package through
Homebrew install/reinstall/uninstall, checks native GUI startup and desktop
registrations, and asserts indexed data survives.
