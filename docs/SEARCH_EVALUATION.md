# Search ranking evaluation

This is a small, reproducible known-item regression corpus, not a claim about
ranking accuracy on every user's files. Run it again when changing relevance
weights or embedding models. No personal files or query history are collected.

## Cases and results

The storage benchmark covers five pairs: recently modified Documents copy;
recently modified Downloads copy; stronger LocalSearch rank; recently accessed
copy with an old modification date; and a document with matching text versus a
recent download whose name matches but whose contents do not. The unwanted copy
gets the stronger synthetic semantic vector, so recency/text evidence must do
useful work rather than benefit from a convenient embedding fixture.

On this corpus GSeek achieves **Success@1 5/5** and **Success@3 5/5**. The global
Nautilus score reference (filename match + LocalSearch rank + recency, no query
location) achieves **Success@1 4/5**. That reference is a formula comparison, not
a substitute for running GNOME Files.

The installed-app fixture also compares **the actual Nautilus 51.0.1
SearchProvider2 service** on the same TinySPARQL index for the matching-text vs
recent-download case. Nautilus's intended document is in the first three but
not first; GSeek returns it first. The comparison waits for Nautilus's index
connection to initialize, avoiding a first-activation empty-result artifact.

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

No location-scoped proximity bonus, speculative version grouping, OCR-quality
estimate, folder-category preference, or activation-history database is used.
