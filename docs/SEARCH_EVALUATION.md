# Search ranking evaluation

This is a small, reproducible known-item regression corpus, not a claim about
ranking accuracy on every user's files. Run it again when changing relevance
weights or embedding models. No personal files or query history are collected.

## Cases and results

The storage benchmark covers five pairs: recently modified Documents copy;
recently modified Downloads copy; stronger LocalSearch rank; recently accessed
copy with an old modification date; and a fresh, not-yet-indexed copy versus an
old copy with matching text and a stronger embedding. The unwanted copy always
gets the stronger synthetic semantic vector, so content evidence must not pick
between copies of a named file.

On this corpus GSeek achieves **Success@1 5/5** and **Success@3 5/5**, matching
the global Nautilus score reference (filename match + LocalSearch rank + recency,
no query location). That reference is a formula comparison, not a substitute for
running GNOME Files. Earlier releases preferred the indexed copy with matching
text in the last case. In practice this returned an outdated copy whenever the
newest one had not been embedded yet.

A separate passport regression (`the_latest_copy_of_a_known_file_wins...`) uses
four copies: an indexed original, an indexed old scan, a fresh `(1)` download,
and an unindexed `renewed` copy, plus an unrelated file with a near-perfect
embedding. The previous ranking put the 400-day-old original first and the fresh
download fourth, behind the unrelated file. The bucketed ranking puts the fresh
download first, keeps every named copy above the unrelated file, and gives the
same order with and without inference.

The installed-app fixture also compares **the actual Nautilus 51.0.1
SearchProvider2 service** on the same TinySPARQL index for an indexed 180-day-old
Documents copy versus a fresh, filename-only Downloads copy. GSeek must return
the fresh copy first, as GNOME's recency does. The comparison waits for
Nautilus's index connection to initialize, avoiding a first-activation
empty-result artifact.

Other regressions cover a filename beyond 5,005 weaker candidates, byte-identical
copies (zero redundant Overview results), changed/unhashed copies that must stay
separate, equal BM25 evidence across files, 510 chunks in one document, Unicode,
and real multilingual embeddings. Existing multimodal reference tests remain in
the inference/release suites.

Observed local installed-app known-file lookup latency was about **50–180 ms**;
the stalled-inference/continued-typing case completed in **164 ms**. These are
fixture observations, not latency guarantees or a comparison with Nautilus's
startup time. CI asserts a one-second bound for the local stalled-inference
filename case and records the measured value in its integration log.

## Reproduce

Run the storage corpus with a development toolchain:

```sh
cargo test --locked --lib store::tests::known_file_benchmark -- --nocapture
```

The normal `tests/run-e2e.sh` suite records installed/native benchmark metrics and
uses real EmbeddingGemma inference. Run in a disposable account/container as
described in [DEVELOPMENT.md](DEVELOPMENT.md). For the optional stock comparison,
install Nautilus and set `GSEEK_TEST_NAUTILUS=1`. The fixture owns both LocalSearch
bus aliases so neither provider activates a real file crawler. CI does not add
Nautilus as an application dependency.

## GNOME sources inspected

- [Global relevance and recency](https://github.com/GNOME/nautilus/blob/main/src/nautilus-search-hit.c)
- [Filename match position and specificity](https://github.com/GNOME/nautilus/blob/main/src/nautilus-query.c)
- [LocalSearch rank and timestamps](https://github.com/GNOME/nautilus/blob/main/src/nautilus-search-engine-localsearch.c)
- [Overview's global search provider](https://github.com/GNOME/nautilus/blob/main/src/nautilus-shell-search-provider.c)

No location-scoped proximity bonus, speculative version grouping (copy suffixes
are only treated as the same name, not merged), OCR-quality
estimate, folder-category preference, or activation-history database is used.
