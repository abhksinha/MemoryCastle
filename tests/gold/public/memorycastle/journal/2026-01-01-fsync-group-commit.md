# Group commit batches fsync every 2 ms
Author: fixture+claude | Date: 2026-01-18
Files: `src/wal.rs`, `src/commit.rs`

## Decision
`GroupCommit::flush` waits up to 2 ms for more writers, then issues one fsync for the whole batch. Durability is unchanged: no writer is acknowledged before its fsync.

## Rejected
- fsync per write: 180 writes per second on the laptop disk. Async fsync without waiting: loses acknowledged writes on power loss.

## Measured
- Throughput 180 to 9,400 writes per second at 64 writers; p99 latency 2.9 ms.
