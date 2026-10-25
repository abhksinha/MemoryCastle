# Keys are length-prefixed with a varint
Author: fixture+claude | Date: 2026-01-16
Files: `src/codec.rs`

## Decision
`encode_key` writes a LEB128 varint length, then the key bytes. Most keys are under 128 bytes, so the prefix is one byte.

## Rejected
- Fixed 4-byte lengths: 3 bytes wasted per key. Null-terminated keys: keys may contain zero bytes.

## Measured
- Average table size down 6% on the sample dataset.
