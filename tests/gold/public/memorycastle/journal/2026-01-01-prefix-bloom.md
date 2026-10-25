# Prefix bloom filters for scans by key prefix
Author: fixture+claude | Date: 2026-01-27
Files: `src/bloom.rs`

## Decision
Tables also store a bloom filter over the first 8 key bytes, so a prefix scan skips tables with no matching prefix.

## Rejected
- A prefix filter on every length: memory grew with the number of lengths queried.

## Measured
- Prefix scan touching 1 of 40 tables: 38 ms to 3 ms.
