# Bloom filters use 10 bits per key
Author: fixture+claude | Date: 2026-01-12
Files: `src/bloom.rs`

## Decision
`BloomFilter::new` sizes filters at 10 bits per key with 7 hash functions, a false positive rate near 1%.

## Rejected
- 16 bits per key: 0.05% false positives but 60% more filter memory, which pushed the block cache out of RAM.

## Measured
- Point lookups for absent keys: 1.02 disk reads at 10 bits, 1.00 at 16.
