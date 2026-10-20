# Third-party components

The Rust application is GPL-3.0-or-later. The shared GNOME 51 Flatpak platform
provides GTK, libadwaita, GLib, TinySPARQL, FFmpeg, and unzip under their respective
licenses. The application packages Poppler (GPL-2.0-or-later/GPL-3.0-or-later
components); upstream license files are installed under `/app/share/licenses/poppler`. Model/runtime downloads are separate.

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
published crates. The Flatpak includes a dependency license inventory.

UI symbolic icons are unmodified artwork from the [GNOME Project](https://www.gnome.org),
[Adwaita icon theme 51.0](https://gitlab.gnome.org/GNOME/adwaita-icon-theme/-/tree/51.0)
(commit `70ecd52fbe844304ed4f845a04a1757fa6284884`). GSeek distributes them under
LGPL-3.0; the license is in `data/icons/COPYING-LGPL-3.0` and in the Flatpak at `/app/share/licenses/gseek/Adwaita-LGPL-3.0`.
