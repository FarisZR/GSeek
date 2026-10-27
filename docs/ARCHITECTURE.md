# Architecture

GSeek has three small process boundaries:

1. **GTK4/libadwaita app:** a normal single-instance GNOME application. It calls the background service over the session bus. GTK objects stay on the main thread; network and database work stay off it.
2. **Background service:** owns `com.fariszr.gseek.Service`, exports GNOME’s `SearchProvider2` plus a private JSON control interface, and survives the window closing. D-Bus activation and XDG autostart handle its lifecycle without user systemd configuration.
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

Search combines GNOME LocalSearch filename candidates with SQLite FTS5 and optional
validated query embeddings. LocalSearch's numeric FTS rank and access/modification
timestamps survive retrieval. Filename matching retains NautilusQuery's preference
for earlier, tighter matches; recency uses NautilusSearchHit's global bands with a
NULL query location. There is no folder-proximity or Documents/Downloads bonus.
References: GNOME's `nautilus-query.c`, `nautilus-search-hit.c`,
`nautilus-search-engine-localsearch.c`, and `nautilus-shell-search-provider.c`.

Exact filename and stem classes outrank ordinary matches. Within a class, filename
position, LocalSearch rank, recency and matching document text distinguish copies.
SQLite BM25 magnitudes are normalized to the strongest matching file for that
query, rather than replaced by ordinal ranks. A materialized FTS score relation
selects each file's best text chunk before applying the candidate cap, preventing
a long document from occupying every candidate slot. Semantic evidence uses the
existing EmbeddingGemma thresholds and only the active pipeline's vectors.
These relevance scores are heuristic, not probabilities or inferred scan quality.

A separate single-slot inference queue bounds model work. Known-file requests give
a warm model at most 150 ms. Overview and the native window have independent
lexical workers, so a cold window query cannot delay Overview queries. Requests
with no lexical matches allow up to five seconds for initial semantic inference.
Waiting checks request generation every
10 ms so later typing is not stuck behind a cold semantic query. Filename and
text lookup work during model startup/failure, with a separate `semantic_error`
status shown in the native results window. Stale results are dropped separately
for Shell and the app, and settings are rechecked before replying. There is no
new model, dependency, tracking database, or continuously reordering result UI.

Ranking runs over a common, bounded 5,000-file candidate pool before truncation.
LocalSearch's exact-name/stem lookup protects matches beyond its broad FTS pool;
its numeric-rank ordered pages apply eligibility checks before counting results.
No content is read merely to rank a filename candidate. Existing metadata stats
and hashes are reused. Schema 4 adds access/modification columns; migration keeps
all existing text, vectors and model downloads, without a reindex.
Only GNOME's indexed timestamps supply access recency: filesystem atime is not
used, since indexing readers can change it. Metadata-only refreshes preserve an
already known access timestamp, and a changed file stamp clears it unless GNOME
provides a replacement. The schema upgrade rechecks its version under SQLite's
write lock so overlapping opens cannot apply it twice.

After ranking, an existing content identity suppresses a duplicate in Overview
only if the file is fully indexed and its current metadata stamp still matches
the hashed version. No query-time hashing or speculative version/OCR-quality
inference is performed. Show All Results retains copies after distinct files.
Unhashed or changed files remain separate. The same distinct-result ordering is
used in both views. Privacy, canonical-path and explicit-exclusion checks remain
in force, including for files outside the embedding folders.


## Inference contracts

The model revision, GGUF sizes and SHA-256 values, upstream llama.cpp release, and
embedding reference vector are pinned together. Managed text, image, and audio
inputs share one embedding space; video and PDFs emit bounded frame/page parts.
A reference canary validates query framing and pooling at setup. There is no
keyword-only mode when inference is unavailable. The upstream server owns model loading,
encoding, and GPU execution. GSeek uses a small query context and a larger media
context, requests mean pooling and L2 normalization, and validates every vector.
The upstream tokenizer bounds text inputs to the actual query/index batch budget.
Dense chunks are split at UTF-8 boundaries with the task prompt repeated, then
token-weighted vectors are pooled and normalized without dropping content or
changing checkpoint numbering. File/input errors leave the healthy worker alive
and fail only that file; backend/transport errors retain the recovery path.
Auto hardware selection retries with another backend if initialization or inference fails; an
explicit backend reports its failure.

Preference updates merge individual fields against the latest saved settings on
the service's D-Bus thread. The UI serializes reads and writes through one queue,
including reopening Preferences while a save is in flight. All UI settings come
from the service's `Settings` method; the desktop process does not read a second
configuration from its own potentially different XDG environment. Replacing settings
cancels both index and query workers, including blocked local inference requests,
before another GPU context starts. Runtime downloads and startup report progress.
The requested and active backends remain separate in status.

Scope changes commit a policy to SQLite and prune files, orphaned content,
checkpoints, and excluded activity together. Discovery checks that committed
policy within its insertion transaction. Checkpoints attach to an existing file
within the same transaction, so an older scan or interrupted worker cannot
reinsert excluded data. Included duplicate files retain their shared vectors.
Folder selections resolve symbolic links to GNOME's host paths before saving,
querying, and pruning; unavailable locations remain saved for disconnected drives.
Status reads queue counts from the committed database so an interrupted worker
cannot make removed files appear queued again.

External endpoints expose text only. The service requires the configured model
identifier, finite normalized 768-dimensional vectors, and agreement with the
reference canary. Redirects are disabled, loopback bypasses proxies, and remote
content transfer requires explicit HTTPS consent. Multimodal API compatibility is
never inferred from an OpenAI-compatible text endpoint.

## Desktop and packaging

The application identity is `com.fariszr.gseek`. Flatpak installs the desktop
entry, icons, GApplication activation, background service activation, and GNOME
search-provider definition under `/app/share`. Flatpak exports and rewrites these
files into the host’s normal XDG discovery paths. Flatpak exports the provider disabled by
default; users enable it in GNOME Settings, which controls provider selection. GSeek opens that panel using
GNOME Settings’ documented `launch-panel` GApplication action. It never writes
host dconf or installs privileged registration files.

Setup requests background/autostart permission through the Background portal,
with `gseek service` as its login command. Denied permission is shown as an error.
The service watches Flatpak’s `/app/.updated` deployment marker. Its creation triggers the existing graceful restart contract: finish queued
preference saves, close the old UI/service, and reactivate through host D-Bus so
the latest deployment is used. A remote-only update does not interrupt work.

The GNOME 51 platform supplies GTK, libadwaita, TinySPARQL, FFmpeg, unzip, OpenSSL 3,
and host-matched graphics extensions. Poppler’s CLI readers are packaged in the
app. Host files are read-only; indexing still requires LocalSearch membership and
all scope/exclusion checks. Portals handle opening files and selecting folders.
No host executable spawning or unrestricted D-Bus permission is needed.

The embedded `data/runtimes.json` pins official llama.cpp release URLs, sizes, and
checksums for CPU, Vulkan, and CUDA on both architectures. CUDA includes upstream
redistributable libraries. Downloads are verified and installed atomically into
Flatpak’s persistent app data. Child-only library paths isolate the inference
runtime from the GNOME process. No inference engine or GPU SDK is compiled.

The welcome icon and all control icons are embedded in GResource. The desktop
icon is exported under the application ID. Flatpak owns their installation and
removal. Existing Flatpak data survives normal updates; there is no legacy data
import or distribution migration layer.

The separate `FarisZR/flatpak` repository signs app commits and repository metadata,
validates Pages limits against actual OSTree objects, and deploys the complete
repository atomically. Models, inference archives, and static deltas are not
hosted there. Both architectures share one install URL.

## Native setup

Setup stays in the primary window, with a Back button, a single task heading,
clamped content, and standard boxed folder lists. The welcome status page centers
its description and setup checklist. Preferences save immediately. Downloads is
not a default content location; filename search still follows GNOME’s index.

The design follows the GNOME HIG source (cloned and read for these changes):
[placeholder pages](https://developer.gnome.org/hig/patterns/feedback/placeholders.html),
[dialogs](https://developer.gnome.org/hig/patterns/feedback/dialogs.html),
[boxed lists](https://developer.gnome.org/hig/patterns/containers/boxed-lists.html),
[writing style](https://developer.gnome.org/hig/guidelines/writing-style.html), and
[adaptiveness](https://developer.gnome.org/hig/guidelines/adaptive.html).

GNOME’s `LaunchSearch` passes the overview query to `gseek results`. GApplication
forwards command lines to the existing primary window; results use GTK SearchEntry,
ListView/NoSelection and a virtualized factory. Generation checks discard stale
responses. Empty, pending and failed queries use AdwStatusPage. The search worker
keeps semantic results when filename lookup fails, exposing a separate status
warning. LocalSearch FTS word-prefix candidates are paged until enough eligible
results survive the same exclusion policy.

Default exclusion constants are shared by validation and visible preferences.
Backend labels come from the selected device and offload report in that worker’s
upstream startup log; software Vulkan devices are identified as CPU execution.

Results queries use a registered GApplication string option so literal hyphen-prefixed
queries survive both command-line parsers. File eligibility checks and activation run
off the GTK and service main loops. Saving preferences clears stale results and
repeats the current query when the updated backend is ready.
