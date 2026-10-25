# Range deletes write one range tombstone
Author: fixture+claude | Date: 2026-01-26
Files: `src/compact.rs`, `src/db.rs`

## Decision
`Db::delete_range` writes a single range tombstone; reads check range tombstones before point entries.

## Rejected
- A tombstone per key in the range: deleting 1 million keys wrote 1 million entries.

## Measured
- Delete of 1 million keys: 4.2 s to 2 ms.
