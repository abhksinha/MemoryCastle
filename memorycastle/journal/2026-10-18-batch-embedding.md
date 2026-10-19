# Embed chunks in batches of sixteen
Author: abhi+claude | Date: 2026-10-18
Files: src/store.rs

## Decision
`embed_missing` sorts pending chunks by length and sends batches of sixteen to the model so padding stays small and session setup is amortised.

## Measured
Batch of 16 x 300-word chunks: 2.3 s, peak RSS 1.3 GB.
