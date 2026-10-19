# Scale test at 3,619 chunks and the vectors table layout
Author: abhi+claude | Date: 2026-10-18
Files: src/db.rs, src/search.rs

## Context
Design targets are stated for 5,000 chunks. Only 114 real chunks existed, so 700 synthetic notes were built from real journal sentences in an isolated data dir with the model files shared.

## Decision
`vectors` became a rowid table (schema v4 migration). `--explain` now prints bm25, embed and scan milliseconds. The f16 scan converts each row with the `half` crate's vectorised slice path and four accumulators.

## Rejected
Keeping `vectors` WITHOUT ROWID: each 1 KB blob spilled to an overflow page, 16 MB on disk for 3.5 MB of vectors, and the scan walked a page per row. An ANN index: the scan is 3 ms at 3,619 rows, far below the 30 ms warm budget; the design's rule of no index under about 100k chunks stands.

## Measured
Keyword save of 700 notes 0.64 s at 9 MB. Keyword search p50 1.4 ms, p95 3.9 ms, 8 MB. Meaning search cold 0.25 s at 286 MB; warm stages bm25 2.7 ms, scan 4.0 ms before and 3.0 ms after the SIMD convert. Reindex 13.5 min, 0.22 s per chunk, 662 MB peak. Store 30.4 MB before the layout change, 19 MB after.

## Open
Scan cost is now SQLite row iteration, about 0.8 microseconds per row. Reindex of 5,000 chunks on the CPU is about 19 minutes against a 2-minute target that assumed Metal; CoreML execution provider untested.
