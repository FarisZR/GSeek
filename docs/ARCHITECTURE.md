# Architecture

GSeek has three small process boundaries:

1. **GTK4/libadwaita app:** a normal single-instance GNOME application. It calls the background service over the session bus. GTK objects stay on the main thread; network and database work stay off it.
2. **Background service:** owns `io.github.FarisZR.GSeek.Service`, exports GNOME’s `SearchProvider2` plus a private JSON control interface, and survives the window closing. D-Bus activation and XDG autostart handle its lifecycle without user systemd configuration.
3. **Upstream inference runtime:** an unmodified, version-pinned `llama-server` process with its native multimodal embedding API over an owner-only Unix socket. No TCP listener is opened. Separate query and indexing processes prevent a long media job from blocking queries. Workers exit with their parent, have startup/request deadlines, and release models when idle.

## Discovery and indexing

The service queries the host TinySPARQL C ABI through a small adapter. It supports
LocalSearch 3 and the older Tracker 3 service name. There is no second filesystem
crawler. `GraphUpdated` batches carry event types and resource IDs; repeated event
types must not be collapsed into a map. Delta queries resolve only changed IDs.
Paged snapshots reconcile missed events every five minutes. Failed snapshots do
not prune unseen entries. Unreadable configured roots fail reconciliation safely.

Discovery and indexing use separate workers and SQLite connections. Inputs are
checked against current scope, symlink policy, and stat metadata before and after
inference. File changes discard incomplete work. Smaller pending files can take
priority between checkpoints. Extraction emits one part at a time, bounding media
temporary storage and memory.

Content identifiers combine the extraction/model pipeline fingerprint and a file
hash. A rename therefore reuses existing vectors. Each completed part commits
independently; interrupted content remains incomplete and cannot be reused as a
finished document. Restarting skips committed parts and continues extraction.

## Search and storage

SQLite owns metadata, the indexing queue, content-addressed chunks, FTS5, normalized
768-dimensional vectors, and bounded activity history. WAL supports discovery,
indexing, and queries concurrently. Versioned transactional migrations preserve
existing indexed content; newer unknown schemas are rejected.

Search requires a validated embedding. Filenames and paths, exact text, and
semantic cosine similarity contribute to ranking. Weak semantic neighbors are
filtered relative to the best match. Results are deduplicated by file. The service
bounds its queue and drops stale requests separately for Shell and diagnostic clients; it
returns a complete result set rather than continuously inserting reordered rows.
Vector scans stream from SQLite with a bounded page cache instead of loading the
entire collection. This favors a minimal dependency tree; ANN is a future option
if measured library sizes require it.

## Inference contracts

The model revision, GGUF sizes and SHA-256 values, upstream llama.cpp release, and
embedding reference vector are pinned together. Managed text, image, and audio
inputs share one embedding space; video and PDFs emit bounded frame/page parts.
A reference canary validates query framing and pooling at setup. There is no
keyword-only mode when inference is unavailable. The upstream server owns model loading,
encoding, and GPU execution. GSeek uses a small query context and a larger media
context, requests mean pooling and L2 normalization, and validates every vector.
Auto hardware
selection retries with another backend if initialization or inference fails; an
explicit backend reports its failure.

Preference updates merge individual fields against the latest saved settings on
the service's D-Bus thread. The UI serializes those updates. Replacing settings
cancels both index and query workers, including blocked local inference requests,
before another GPU context starts. Runtime downloads and startup report progress.
The requested and active backends remain separate in status.

Scope changes commit a policy to SQLite and prune files, orphaned content,
checkpoints, and excluded activity together. Discovery checks that committed
policy within its insertion transaction. Checkpoints attach to an existing file
within the same transaction, so an older scan or interrupted worker cannot
reinsert excluded data. Included duplicate files retain their shared vectors.

External endpoints expose text only. The service requires the configured model
identifier, finite normalized 768-dimensional vectors, and agreement with the
reference canary. Redirects are disabled, loopback bypasses proxies, and remote
content transfer requires explicit HTTPS consent. Multimodal API compatibility is
never inferred from an OpenAI-compatible text endpoint.

## Desktop and packaging

GNOME Shell reads search-provider definitions from system data directories,
**not** the user data directory. Homebrew installs the static, default-disabled
provider into `/usr/local/share/gnome-shell/search-providers`. This is the only
system registration. Per-user app entries, icons, activation, and autostart use
stable absolute launcher paths under `~/.local/share/gseek-app`.

Integration tracks checksums and the previous Files/GSeek provider settings.
Uninstallation removes only unchanged owned files and restores only those settings
it changed. Index data survives. The launcher uses host GTK/GLib/LocalSearch; it
does not override those libraries with Homebrew copies. GSeek reuses
optional readers from the host, preferring `/usr/bin` over Homebrew. The cask
only supplies OpenSSL 3 when the host lacks that ABI. Its compatibility path is
passed to the inference child only when needed. All inference backends are
downloaded directly from official llama.cpp release assets. The embedded `data/runtimes.json` pins release URLs, exact sizes,
and checksums for CPU, Vulkan, and CUDA on both architectures. CUDA includes its
upstream redistributable libraries. GSeek verifies downloads, serializes installation,
and atomically installs each backend. The app does not compile, patch, or rehost
inference engines. Private sockets and child-only library paths isolate the
runtime from the native GNOME process.

The welcome icon loads directly from an embedded PNG resource, independent of icon
installation, SVG loaders, and stale theme caches. Desktop entries reference an
owned, installed PNG by absolute path. Icon files participate in the same upgrade
and uninstall manifest as other desktop registrations.
