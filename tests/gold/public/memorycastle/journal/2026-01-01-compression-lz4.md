# Blocks are compressed with LZ4
Author: fixture+claude | Date: 2026-01-20
Files: `src/table.rs`

## Decision
Data blocks use LZ4 (`compress_block`); index blocks stay uncompressed for fast seeks.

## Rejected
- Zstd level 3: 18% smaller tables but 2.6x slower reads on the ARM boards.

## Measured
- Table size 41% of raw; decompression 1.1 GB/s.
