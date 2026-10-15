# Third-party components

The Rust application is GPL-3.0-or-later. GSeek uses the host GNOME GTK,
libadwaita, GLib, and TinySPARQL libraries. Media readers are Homebrew dependencies,
with their own licenses and notices.

GSeek downloads unmodified official llama.cpp distributions (MIT), including
upstream's license, tools, libraries, and notices. It does not build or modify
llama.cpp. The pinned runtime release is [b11669](https://github.com/ggml-org/llama.cpp/releases/tag/b11669);
`data/runtimes.json` records the exact assets and checksums. CUDA installations
also download the corresponding official CUDA redistributable archive; NVIDIA's
redistribution terms apply to those upstream libraries.

EmbeddingGemma 2 weights are downloaded separately from ggml-org's conversion at
revision `bfcd298762cc34d0357ece5ebdd31791a3a374d8` (Apache-2.0); they are not part
of the application download. Model: https://huggingface.co/google/embeddinggemma-2.
Conversion: https://huggingface.co/ggml-org/embeddinggemma-2-GGUF.

DigUp (MIT), by Abdur Rahim, informed GSeek's model selection, pinned model file
metadata, and inference framing. Its source and license:
https://github.com/ARahim3/DigUp. GSeek has its own Rust application, index, and
GNOME integration.

Rust dependency licenses and versions are recorded in Cargo.lock and their
published crates. Release CI produces a dependency license inventory.
