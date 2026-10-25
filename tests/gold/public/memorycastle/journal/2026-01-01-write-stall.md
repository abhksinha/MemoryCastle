# Writes stall when level 0 has 12 tables
Author: fixture+claude | Date: 2026-01-15
Files: `src/db.rs`

## Decision
`Db::put` sleeps 1 ms per write when level 0 holds 8 tables and blocks at 12, so compaction can catch up.

## Rejected
- No stall: level 0 grew to 300 tables and reads slowed 40x.

## Measured
- Under sustained load, level 0 stays at 6 to 9 tables.
