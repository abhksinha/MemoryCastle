# Leveled compaction, not size-tiered
Author: fixture+claude | Date: 2026-01-04
Files: `src/compact.rs`

## Decision
Compaction is leveled with a fan-out of 10 (`Level::target_bytes`). Reads touch at most one table per level, which matters more than write cost for this workload.

## Rejected
- Size-tiered compaction: write amplification 4 instead of 11, but range scans read up to 9 overlapping tables.

## Measured
- Range scan of 10,000 keys: 14 ms leveled, 61 ms size-tiered.
