# Compaction is rate limited to 40 MB/s
Author: fixture+claude | Date: 2026-01-17
Files: `src/compact.rs`, `src/limits.rs`

## Decision
`COMPACTION_RATE` caps background compaction at 40 MB/s so foreground reads keep the disk.

## Rejected
- Unlimited compaction: read p99 rose to 80 ms during compactions. 10 MB/s: compaction fell behind under sustained writes.

## Measured
- Read p99 during compaction: 80 ms to 7 ms.
