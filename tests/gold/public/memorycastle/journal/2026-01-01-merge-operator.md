# Counters use a merge operator, not read-modify-write
Author: fixture+claude | Date: 2026-01-22
Files: `src/merge.rs`

## Decision
`MergeOperator::merge` folds increments at read and compaction, so an increment is one write with no read.

## Rejected
- Read-modify-write in a transaction: conflicts under concurrent increments.

## Measured
- 50 concurrent incrementers: 1,200 to 41,000 increments per second.
