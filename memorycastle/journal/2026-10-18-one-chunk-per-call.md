# Embed one chunk per model call
Author: abhi+claude | Date: 2026-10-18
Files: src/store.rs, src/chunk.rs
Supersedes: 2026-10-18-batch-embedding.md

## Context
Reindex of the AbhiGram journal peaked at 755 MB. Batching was the suspect.

## Decision
One chunk per call, longest first, and chunk targets of 200 words (max 280). Terminator-less runs such as tables split on lines then words.

## Rejected
Batches of 2, 4, 8 and 16: per-chunk time was 138 to 200 ms regardless, so batching bought no throughput on the CPU while peak memory grew linearly. Disabling the ONNX arena or memory patterns: no change in peak.

## Measured
Reindex of 90 chunks: 21 s, 0.28 s per chunk, peak 535 MB (was 755 MB). Threads 2 vs 4: same reindex wall time, but a single query is 135 ms vs 95 ms, so 4 stays.
