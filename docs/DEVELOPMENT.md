# Development

The application targets GNOME 51. Rust 1.99.0 is pinned in `rust-toolchain.toml`.
The Flatpak uses the GNOME 51 platform and a verified build-only Rust toolchain;
no Rust toolchain is shipped to users. All crate sources are pinned by Cargo.lock
and staged for an offline build in `build-aux/cargo-sources.json`.

## Flatpak build

```sh
flatpak remote-add --user --if-not-exists flathub https://flathub.org/repo/flathub.flatpakrepo
flatpak-builder --assumeyes --user --install-deps-from=flathub --force-clean \
  --repo=flatpak-repo flatpak-build build-aux/com.fariszr.gseek.json
flatpak remote-add --user --if-not-exists --no-gpg-verify gseek-dev "$PWD/flatpak-repo"
flatpak install --user gseek-dev com.fariszr.gseek
flatpak run com.fariszr.gseek
```

The unsigned local remote is for development only. Distributed packages use the
signed repository at `https://flatpak.fariszr.com`.

After changing Cargo.lock, regenerate `build-aux/cargo-sources.json` with
[flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo).
The manifest builds entirely offline after fetching checksummed sources. Update
Poppler and the build-only Rust archives in the manifest with their upstream
SHA-256 checksums. SDK/platform updates are supplied by the runtime remote.

## Native development

For a quick source build, install GTK4/libadwaita development tools, TinySPARQL,
OpenSSL 3, and readers through your distribution, then run:

```sh
cargo build --locked
cargo run -- prepare-model
cargo run
```

`scripts/ci-deps.sh` records CI dependencies for disposable Fedora 45 and Ubuntu
26.10 containers. Those development/test packages are not app dependencies.
Native development uses your XDG paths. The installed Flatpak uses only
`~/.var/app/com.fariszr.gseek/`; no data is imported from another installation.

`data/runtimes.json` pins unmodified official llama.cpp binaries for both
architectures. Updating it requires reference and multimodal tests. GSeek never
compiles CUDA, ROCm, or an inference engine. For offline development,
`GSEEK_RUNTIME_DIR` can point to upstream distributions in `cpu/`, `vulkan/`,
and `cuda/`.

## Tests

```sh
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo test --locked --no-default-features --lib
python3 -m unittest discover -s tests -p 'test_*.py' -v
GSEEK_TEST_INFERENCE=true bash tests/flatpak-smoke.sh
```

The installed Flatpak smoke test checks packaged readers and dynamic linking,
then downloads the real model/runtime and compares CPU and software Vulkan to
the pinned 768-dimensional reference vector. Both architectures run these checks
on releases and on relevant pull requests.

Real inference, multimodal readers, and recovery:

```sh
cargo test --locked --test real_inference -- --ignored --test-threads=1
cargo test --locked --lib inference::tests::worker_restarts_after_process_failure -- --ignored --test-threads=1
```

D-Bus, real TinySPARQL, multilingual retrieval, and GTK views:

```sh
GSEEK_TEST_MODEL_CACHE="$HOME/.local/share/gseek/models" \
GSEEK_GUI_TEST_COMMAND='cargo test --lib ui::tests::native_views -- --ignored --test-threads=1' \
GSEEK_SCREENSHOT_DIR="$PWD/artifacts/screenshots" \
xvfb-run -a bash tests/run-e2e.sh
```

This fixture owns a disposable LocalSearch endpoint with GNOME’s actual
TinySPARQL library and ontology. Its XDG paths and session bus are isolated from
your files. Add `GSEEK_TEST_SHELL=1 GSETTINGS_BACKEND=keyfile` for real headless
GNOME Activities search. The fixture starts a private GNOME Shell; its test
extension is never packaged. Snapshots come from real mapped widgets.

To test an installed development Flatpak instead, set `GSEEK_TEST_FLATPAK=1`
and omit `GSEEK_GUI_TEST_COMMAND`. This uses Flatpak’s actual exported activation,
provider, and desktop files. Run only in a disposable account/container: the
fixture temporarily sets and then resets that app’s per-user test overrides.
Set `GSEEK_TEST_UPDATE_REPO` to that disposable installation’s local update
repository to verify that an installed update replaces both the running service
and its open window while preserving settings and search.

CI always runs lint, unit contracts, and both Flatpak builds. Source search/UI
changes also run native and installed-package CPU integration on Fedora and
Ubuntu. Real-model stress tests and packaged CPU/software-Vulkan reference checks
run only for inference, model, reader, runtime,
packaging, or relevant dependency changes. Version and GTK updates alone do not
trigger Vulkan. `scripts/ci-scope.py` defines and tests this boundary. Manual CI
runs the full suite. Green CPU/software-Vulkan CI does not establish physical GPU
performance: representative AMD/NVIDIA hardware still needs validation.

## Releases

Bump the version in Cargo.toml, Cargo.lock, and the AppStream release metadata.
After the exact commit passes CI on `main`, the Release workflow builds both
Flatpak architectures, installs each exact package, and checks real CPU and
software-Vulkan reference embeddings before publishing bundles and SHA256SUMS.
Tags and manual releases are also supported. Published versions keep their
verified assets; reruns can finish an existing draft. The separate [distribution repository](https://github.com/FarisZR/flatpak)
imports these immutable bundles, signs OSTree app commits and summary metadata,
checks Cloudflare Pages limits, and deploys `flatpak.fariszr.com`. It prunes
unreachable objects while retaining up to three commit parents, and creates no
static deltas. Models and runtimes continue to
download directly from upstream.

Normal Flatpak updates preserve settings, indexes, and downloads. The Background
portal owns login startup; Flatpak owns exported desktop integration. The app’s
update monitor restarts only after the new deployment has been installed.
