# Iterators prefetch the next block
Author: fixture+claude | Date: 2026-01-02
Files: `src/iter.rs`

## Decision
`TableIter::next` issues a read-ahead for the following block when it is two thirds through the current one.

## Rejected
- No prefetch: scans waited on every block. Prefetching four blocks: wasted reads on short scans.

## Measured
- Full scan of 1 GiB: 2.8 s to 1.9 s.
