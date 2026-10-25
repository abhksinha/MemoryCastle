# The memtable allocates from an arena
Author: fixture+claude | Date: 2026-01-24
Files: `src/memtable.rs`, `src/arena.rs`

## Decision
Skiplist nodes come from a bump `Arena` freed all at once on flush, instead of one allocation per node.

## Rejected
- The global allocator per node: 30% of insert time in malloc.

## Measured
- Insert throughput up 1.4x; flush frees 32 MiB in one call.
