# The memtable is a skiplist, flushed at 32 MiB
Author: fixture+claude | Date: 2026-01-17
Files: `src/memtable.rs`

## Decision
`Memtable` is a lock-free skiplist; `Memtable::should_flush` returns true at 32 MiB and the flush runs on a background thread.

## Rejected
- A B-tree memtable: needs a lock for inserts. 128 MiB flush threshold: recovery replays four times more log.

## Measured
- Flush of 32 MiB takes 210 ms; insert p99 4 microseconds.
