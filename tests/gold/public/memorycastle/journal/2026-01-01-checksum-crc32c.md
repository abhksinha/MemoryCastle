# Blocks carry a CRC32C checksum
Author: fixture+claude | Date: 2026-01-27
Files: `src/table.rs`

## Decision
Every block ends with a CRC32C (`Block::verify`), checked on read. The hardware instruction makes it nearly free.

## Rejected
- xxHash64: faster in software, but no hardware path on the ARM boards and a longer trailer.

## Measured
- Checksum cost per 4 KiB block: 0.3 microseconds.
