# Third-party components

GSeek is GPL-3.0-or-later. Native GTK, libadwaita, GLib, TinySPARQL and the readers
are installed as Homebrew dependencies, with their own licenses and notices.

The managed inference bundle contains llama.cpp and ggml (MIT), including their
vendored components. The bundle includes llama.cpp's LICENSE; its source at
commit `4d756bc72bf00a4aacf410ae15a2d315f3db400d` is available at
https://github.com/ggml-org/llama.cpp/tree/4d756bc72bf00a4aacf410ae15a2d315f3db400d.
That source contains the notices for its vendored nlohmann/json, stb, miniaudio,
and other components. Release packaging preserves these notices.

EmbeddingGemma 2 weights are downloaded separately from ggml-org's conversion at
revision `bfcd298762cc34d0357ece5ebdd31791a3a374d8` (Apache-2.0); they are not part
of the application download. Model: https://huggingface.co/google/embeddinggemma-2.
Conversion: https://huggingface.co/ggml-org/embeddinggemma-2-GGUF.

DigUp (MIT), by Abdur Rahim, informed GSeek's model selection, pinned model file
metadata, inference framing, and the small embedding-only allocation patch.
Its source and license: https://github.com/ARahim3/DigUp. GSeek has its own Rust
application, index, GNOME integration, and C++ worker implementation.

Rust dependency licenses and versions are recorded in Cargo.lock and their
published crates. Release CI produces a dependency license inventory.
