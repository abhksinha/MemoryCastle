# Old versions are kept for 60 seconds after the last snapshot
Author: fixture+claude | Date: 2026-01-27
Files: `src/snapshot.rs`, `src/compact.rs`

## Decision
Compaction drops shadowed versions older than the oldest live snapshot minus `VERSION_GRACE` (60 s), so a snapshot opened just after a compaction still sees consistent data.

## Rejected
- Dropping versions immediately: a snapshot taken during compaction read a missing version.
