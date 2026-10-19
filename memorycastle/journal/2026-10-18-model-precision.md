# Choose int8 over q4 for EmbeddingGemma 2
Author: abhi+claude | Date: 2026-10-18
Files: src/embed/download.rs, src/embed/mod.rs

## Context
Three ONNX exports exist: int8 (314 MB), q4 (174 MB), q4f16 (157 MB). q4 is twice as fast per query and 120 MB lighter.

## Decision
int8 stays the default. On the 24-question AbhiGram gold set it is the only setting with recall@3 of 1.0; q4 and q4f16 score 0.958. Precision is part of the space definition, so switching is `reindex --space new` with `MEMORYCASTLE_MODEL=q4`.

## Measured
int8 recall@3 1.000, MRR 0.938, p50 124 ms cold. q4 recall@3 0.958, MRR 0.917, p50 58 ms. Cosine between int8 and q4 vectors 0.985 to 0.990.

## Open
Revisit if the gold set grows past 100 questions and q4 closes the gap.
