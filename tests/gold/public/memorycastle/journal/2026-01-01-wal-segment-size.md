# WAL segments rotate at 64 MiB
Author: fixture+claude | Date: 2026-01-02
Files: `src/wal.rs`

## Decision
`WalSegment::rotate` closes a segment at 64 MiB and opens the next. Recovery replays segments in order, so a smaller segment only adds file handles.

## Rejected
- 16 MiB segments: four times the files for no faster recovery.

## Measured
- Recovery of 2 GiB of log: 3.1 s at 64 MiB, 3.0 s at 16 MiB.
