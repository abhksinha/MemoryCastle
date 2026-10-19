# GPU via CoreML measured slower than CPU
Author: abhi+claude | Date: 2026-10-18
Files: src/embed/ort_rt.rs, examples/bench_embed.rs

## Context
Reindex of 3,619 chunks took 13.5 minutes on the CPU. The question was whether to auto-detect a GPU and use it with CPU fallback.

## Decision
CPU stays the only path. A knob `MEMORYCASTLE_ORT_EP=coreml` exists for re-testing; it is not selected automatically because it loses.

## Rejected
Auto-detecting CoreML: load 12 s against 0.11 s, warm query 180 ms against 109 ms, batch 2.9 s against 0.22 s, peak 1,269 MB against 364 MB for int8; q4 similar. Cause: the export's multimodal merge has dynamic-shape media inputs, so the CoreML provider takes the graph as many partitions with CPU copies between them, and quantised weights are expanded to fp16 on the GPU side. CoreML also refuses zero-element inputs, so a one-row dummy media tensor is needed (`MEMORYCASTLE_MEDIA_ROWS=1`).

## Open
Whether a one-row zero media tensor changes the text embedding was not verified in-process (one ONNX environment per process); compare vectors across two runs before ever enabling it by default. A text-only ONNX export would remove both problems.
