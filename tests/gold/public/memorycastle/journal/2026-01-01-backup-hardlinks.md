# Backups hard-link immutable tables
Author: fixture+claude | Date: 2026-01-15
Files: `src/backup.rs`

## Decision
`backup::create` hard-links every table file (they never change after writing) and copies only the current WAL and manifest.

## Rejected
- Copying every table: a 50 GB database took 9 minutes and doubled disk use.

## Measured
- Backup of 50 GB: 9 minutes to 1.2 seconds.
