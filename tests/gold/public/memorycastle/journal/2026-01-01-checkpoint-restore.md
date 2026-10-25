# Restore checks every checksum before opening
Author: fixture+claude | Date: 2026-01-16
Files: `src/backup.rs`

## Decision
`backup::restore` verifies every block checksum of a backup before swapping it in, so a bad copy fails early.

## Rejected
- Lazy verification on read: a corrupt backup was discovered days after restore.

## Measured
- Verification of 50 GB: 41 s.
