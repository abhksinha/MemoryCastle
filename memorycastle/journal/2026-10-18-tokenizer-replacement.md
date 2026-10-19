# Replace the HuggingFace tokenizer crate with a compact BPE
Author: abhi+claude | Date: 2026-10-18
Files: src/embed/tokenizer.rs, src/embed/ort_rt.rs, Cargo.toml

## Context
First meaning search measured 629 MB resident with the int8 model. The budget in the design is 300 MB. A breakdown showed the `tokenizers` crate alone took 376 MB after parsing the 32 MB tokenizer.json; the ONNX session was only 181 MB.

## Decision
Own tokenizer: Gemma's tokenizer.json is Replace(space, ▁) + BPE over the whole text + byte fallback + bos/eos. Convert once to `tokenizer.bin` (pieces blob, offsets, piece-sorted ids, key-sorted merge table), load as one buffer, binary search for lookups, heap-driven merging. Special tokens inside notes are encoded as text on purpose: memory is data.

## Rejected
Keeping the crate with `fancy-regex` instead of `onig`: same memory, the cost is the parsed vocab, not the regex engine.

## Measured
Resident 376 MB to 14 MB; load 3 ms; 480-token encode 0.85 ms; parity test against the crate passes on English, Hindi, code, emoji and whitespace runs. Resident with model loaded after queries: 271 MB.

## Open
Linux x86 build of the whole crate untested.
