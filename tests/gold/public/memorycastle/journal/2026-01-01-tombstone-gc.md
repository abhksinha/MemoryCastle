# Tombstones are dropped at the bottom level only
Author: fixture+claude | Date: 2026-01-06
Files: `src/compact.rs`

## Decision
A delete writes a tombstone; compaction drops it only when merging into the last level, where no older version can exist below it.

## Rejected
- Dropping tombstones at any level: deleted keys came back from lower levels in the fuzz test.

## Measured
- Fuzz test: 0 resurrected keys in 10 million operations.
