# No Bloom or XOR filters until the store is large
Author: abhi+claude | Date: 2026-10-18
Files: src/search.rs, src/fts.rs

## Context
Asked whether Bloom filters, XOR filters or binary-quantised vectors should be added.

## Decision
Not now. FTS5 answers identifier presence in under 1 ms and the vector scan over 90 chunks takes about 0.1 ms; the model call (100 ms) dominates every meaning search. Adding filters would optimise an operation that costs nothing.

## Rejected
Bloom filter on terms per project; Bloom on the query cache; binary vectors with XOR and popcount. All deferred, not refused.

## Open
Revisit when `status` shows more than about 50k chunks or vector scan p50 passes 10 ms. Binary vectors (1 bit per dim, 64 bytes per chunk, rerank top 100 with f16) would be the first index to add, about 60 lines.
