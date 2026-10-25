# The block cache is a sharded LRU
Author: fixture+claude | Date: 2026-01-17
Files: `src/cache.rs`

## Decision
`BlockCache` is 16 shards of a plain LRU, each behind its own mutex, keyed by table id and block offset.

## Rejected
- A single global LRU: lock contention at 8 threads. CLOCK: no measurable hit-rate difference and harder to reason about.

## Measured
- Hit rate 93% both ways; lookups at 8 threads 4.1x faster sharded.
