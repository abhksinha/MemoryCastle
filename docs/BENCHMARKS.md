# Benchmarks

Measured on an M5 MacBook Air, release build, 2026-10-18. Reproduce with
`cargo run --release --example bench_embed` and `memcastle eval`.

**Retrieval quality.** 34 hand-written questions over two real journals (24
for an Android messaging app, 10 for this repo), scored by whether the note
that holds the answer is in the top k.

| Mode | recall@3 | recall@10 | MRR | p50 | peak RSS |
| --- | --- | --- | --- | --- | --- |
| grep baseline | 0.971 | 1.000 | 0.792 | 0.2 ms | 5 MB |
| keyword (BM25) | 0.971 | 1.000 | 0.958 | 0.4 ms | 6 MB |
| keyword + meaning (int8) | 1.000 | 1.000 | 0.951 | 126 ms cold | 285 MB |

**Scale.** 700 synthetic notes built from real journal sentences, 3,619 chunks.

| Path | Result |
| --- | --- |
| Save 700 notes, keyword only | 0.64 s, 9 MB |
| Keyword search p50 / p95 | 1.4 ms / 3.9 ms, 8 MB |
| Meaning search, model cold | 0.25 s, 286 MB |
| Meaning search stages, warm | BM25 2.7 ms, vector scan 3 ms |
| Embed all 3,619 chunks once | 13.5 min on CPU (0.22 s per chunk), 662 MB peak |
| Store on disk | 19 MB (2.5 MB text, 3.5 MB vectors) |

**Bit-packed prefilter.** Above 512 vectors in scope, the vector channel
scores one sign bit per dimension (72 bytes per vector, one contiguous blob
per project) by XOR and popcount, then reranks 300 candidates exactly. At
3,619 chunks: 3.0 ms to 1.6 ms, top-1 identical on every test query, gold
recall unchanged. The pack is dropped on any vector write and rebuilt on the
next search (16 ms). Below the threshold the exact scan is already under
0.5 ms, so nothing runs. Bloom and XOR filters elsewhere were measured as
unnecessary: FTS5 answers term presence in under 1 ms.

**Graph.** 700 notes, 7,813 edges: `graph` writes 638 KB in 9 ms; `links
<file>` answers in under 1 ms at 7 MB resident. Every bound that shapes
memory is a named constant in `src/limits.rs` with compile-time relations.

**Agent tokens.** Codex, same question with memory rules vs rg/cat only,
two runs per mode. AbhiGram (79 KB journal), six decision questions: 15.4k
vs 19.5k tokens, 21% saved (best 56%, one loss). This repo (9 short notes),
with `search --sections`: 7.7k vs 8.0k, 3%, within a 2x run-to-run noise.
Questions that need source reading saved nothing. Savings scale with journal
size. After budgeting search output (one block per note, 6 KB per call):
retrieval tokens 44% to 91% below grep on three AbhiGram questions (average
63%), whole run 32% to 53%; the run floor (prompt, schemas, answer) is about
9k tokens either way.

**Second agent, Antigravity**, same six questions, two runs per mode:
42.1k vs 54.8k tokens, 23% saved, six of six questions cheaper. Its floor
(system prompt, tool schemas, thinking) is about 33k per one-shot run, so the
same 12.8k absolute saving reads as a smaller percentage than on Codex.

**One session, six turns** (Antigravity, stream-json, same six questions
as six turns of one conversation, two repeats per mode). Prompt caching is
real in sessions: after turn 2 each model call reads 17k to 45k from cache
and adds 2.5k to 4k new. Averages: model calls 22 vs 32, new input 102k vs
149k, cache reads 652k vs 1,108k, output 11k vs 15k, context at the end
114k vs 165k. Weighted at Gemini 3.8 Flash paid rates (input $0.75, cached
$0.075, output $3.75 per 1M): $0.167 vs $0.253 per session, 34% saved. The
grep session also reaches compaction about 30% sooner. Repeats differ by up
to 25% in the grep mode, so treat these as ranges.

**Precision, 44-question gold set** (five of them off-topic, expecting no
result): auto recall@3 1.000, recall@10 1.000, MRR 0.921; keyword only
0.952 / 0.952 / 0.857. Cosine calibration: right notes 0.67 to 0.83,
off-topic questions 0.54 to 0.65 against their best chunk.

**Code spans.** 40 random function definitions from the AbhiGram index,
span judged by whether its last line closes a brace at the definition's
indent (or the next non-blank line returns to it, for Python): 38 of 40
correct. Re-anchoring after inserting three lines at the top of a file:
`StatusRender.kt:64-118` printed as `67-121` without reindexing.

**Spans in use.** Two "constraints plus where is it defined" questions
through Codex, two runs per mode: with spans the agent ran `links`, one
search, then `sed -n` on the printed range in all four runs. Retrieval
tokens 2.3k vs 13.1k and 2.8k vs 7.9k against grep; whole run 53% less on
one question, more on the other (plugin noise and a lucky single-rg grep
run). The class that cost more with memory in the morning now costs less
on retrieval in both cases.

**Bytes versus tokens.** The savings counter is a share of bytes. Checked
with the built-in Gemma tokenizer (`examples/tok_ratio.rs`) on one real
call: 87.2% by bytes, 87.8% by tokens. Bytes per token: journal prose 3.6,
Rust 3.0, Kotlin 3.25. Printed output is prose-heavier than the code it
stands in for, so the byte figure understates the token figure by under one
point.

**Code graph.** AbhiGram checkout, 974 source files, 12 MB of Rust and
Kotlin: first index 0.58 s (30,136 definitions, 1,106 imports, 40,984
mention rows), incremental re-run 39 ms, 59 MB peak. Graph build 0.78 s;
TSVs 6.7 MB; the HTML navigation view 2.0 MB with 5,139 nodes, parsed by a
browser in 8 ms. This repo: 32 files, 362 definitions, 8 ms.

**GPU.** The CoreML execution provider was measured and rejected: session
load 12 s, query 180 ms, peak 1.27 GB against 0.11 s, 109 ms and 364 MB on
the CPU. The multimodal export splits into many partitions with CPU copies
between them. The knob stays (`MEMORYCASTLE_ORT_EP=coreml`) for re-testing.

### Compared with cognee

[cognee](https://github.com/topoteretes/cognee) is a well-built memory layer
and a fair representative of the LLM-extraction approach: `add` stores
documents, `cognify` runs an LLM over every chunk to extract entities and
relations into a graph store plus a vector store, and graph-aware search
modes call the LLM again at query time. That design can answer questions a
plain retriever cannot, such as multi-hop questions across documents. It
pays for that in moving parts and in per-token cost.

| | memorycastle | cognee |
| --- | --- | --- |
| Language, runtime | Rust, one binary | Python package, async service |
| Stores | one SQLite file | vector store (LanceDB by default) + graph store (Kuzu by default) + relational metadata |
| Ingest | chunk, embed locally | chunk, embed, LLM extraction of entities and relations per chunk |
| Needs an LLM API key | no | yes, for cognify and for graph search modes |
| Ingest cost per note | milliseconds of CPU, 0 tokens | one or more LLM calls per chunk |
| Query | BM25 + local embedding, under 0.3 s cold | embedding plus optional LLM completion over retrieved graph |
| Memory footprint | 5 MB keyword, 285 MB with model loaded | Python process plus two database engines |
| Where the memory lives | markdown in your repo, under git | the framework's stores |
| Multi-hop reasoning over a graph | no, by design | yes |

### Measured against cognee and grep (2026-10-19)

A pre-registered, single-session run on an M5 MacBook Air. The plan, with
the questions, tasks and pass criteria, was written before any cognee
number was seen; the only changes after were setup fixes, logged with a
time. The questions and notes are private; the aggregates are below.

**Systems.** grep: term-presence count over whole notes (the `GrepMemory`
baseline in the harness). memorycastle 0.11.1: keyword only (BM25, no model)
and keyword plus meaning (EmbeddingGemma 2 int8, local). cognee 1.6.5 in two
configurations: *local*, with the GLiNER extractor and fastembed embeddings
and no key, which is the apples-to-apples comparison; and *llm*, cognee's
defaults (an OpenAI chat model for extraction, `text-embedding-3-large`)
with a key. Search types: CHUNKS (nearest chunks) and CHUNKS_LEXICAL. A
result counts when the note file it came from is the gold note.

**Corpora.** AbhiGram journal, 10 notes, 113 chunks, 29 answered questions.
memorycastle journal, 40 notes, 193 chunks, 57 answered questions. Plus 11
absent-topic questions whose right answer is nothing.

#### Retrieval

| AbhiGram, 29 questions | recall@3 | recall@10 | MRR | p50 | absent: returned something |
| --- | --- | --- | --- | --- | --- |
| grep | 0.862 | 1.000 | 0.637 | 0.2 ms | 11 of 11 |
| memorycastle keyword | 0.966 | 0.966 | 0.851 | 2.5 ms | 3 of 11 |
| memorycastle keyword + meaning | **1.000** | 1.000 | 0.856 | 53 ms warm, 0.23 s cold | **1 of 11** |
| cognee local, CHUNKS | 0.897 | 0.931 | 0.767 | 65 ms | 11 of 11 |
| cognee local, CHUNKS_LEXICAL | 1.000 | 1.000 | **0.902** | 59 ms | 11 of 11 |
| cognee llm, CHUNKS | 1.000 | 1.000 | 0.787 | 1.0 s | 11 of 11 |
| cognee llm, CHUNKS_LEXICAL | 1.000 | 1.000 | 0.925 | 85 ms | 11 of 11 |

| memorycastle journal, 57 questions | recall@3 | recall@10 | MRR | p50 | absent: returned something |
| --- | --- | --- | --- | --- | --- |
| grep | 0.789 | 0.912 | 0.676 | 0.1 ms | 11 of 11 |
| memorycastle keyword | 0.825 | 0.895 | 0.725 | 2.3 ms | 3 of 11 |
| memorycastle keyword + meaning | **0.965** | **1.000** | **0.851** | 52 ms warm | **1 of 11** |
| cognee local, CHUNKS | 0.772 | 0.860 | 0.657 | 290 ms | 11 of 11 |
| cognee local, CHUNKS_LEXICAL | 0.860 | 0.965 | 0.753 | 186 ms | 11 of 11 |
| cognee llm, CHUNKS | 0.807 | 0.965 | 0.717 | 0.9 s | 11 of 11 |
| cognee llm, CHUNKS_LEXICAL | 0.860 | 0.965 | 0.787 | 50 ms | 11 of 11 |

cognee's lexical mode wins MRR on the small corpus, where the questions
reuse the notes' words. On the larger corpus memorycastle's fused search
leads on every metric. cognee has no relevance floor: every absent-topic
question got an answer in every mode, so an agent is handed the nearest
wrong note. memorycastle's p50 is through the warm `serve` process; cold it
is 0.23 s. cognee's local p50 is after its models are loaded in the same
process (760 MB resident); its llm CHUNKS mode makes a provider call per
query. The resident size of the answering process: grep 6 MB, memorycastle
keyword 9 MB, memorycastle meaning 300 MB, cognee local 730 to 760 MB,
cognee llm 520 to 610 MB.

#### Ingest and upkeep

| | memorycastle | cognee local | cognee llm |
| --- | --- | --- | --- |
| AbhiGram, 10 notes, from empty | 21 s, 535 MB peak | 193 s, 8.8 GB peak | 135 s, 511 MB peak, 74k prompt + 53k completion + 56k embedding tokens |
| memorycastle journal, 40 notes, from empty | 55 s, 471 MB peak | 164 s, 4.7 GB peak | 82 s, 604 MB peak, 102k + 99k + 74k tokens |
| One note edited, re-ingest | 0.19 s (one chunk re-embedded) | 13.1 s, 3.0 GB peak | not measured |
| Store on disk, AbhiGram | 0.9 MB for the 40-note journal; the 10-note one is smaller | 25 MB | 70 MB |
| Left running after a session | one `serve` process: 270 MB, 66 MB after 5 min idle, exits after 30 min | none (Codex starts and stops the MCP server) | none |

The 8.8 GB peak is the GLiNER extractor over every chunk. The llm
configuration is lighter locally because the extraction runs at the
provider; it costs tokens instead.

#### Agents end to end

Codex 0.163, `codex exec`, a fresh session per run, three homes that differ
only in the memory: grep (no memory section, no tools), memorycastle (MCP
server, hooks, the installed instruction section) and cognee (its MCP
server with `recall`, an instruction section of the same length). Same
AbhiGram clone, same four tasks, one run each. All three arms passed all
four rubrics: found the recorded rejection, reported the measured numbers
and the jank reason correctly, respected the API-level constraint, and
added the flag without citing an invented decision. The difference is cost.

| Task | grep | memorycastle | cognee |
| --- | --- | --- | --- |
| Rejected approach: total tokens / uncached input / retrieval bytes | 411k / 37k / 111 KB | 379k / 32k / 90 KB | 483k / 39k / 133 KB |
| Measured number | 79k / 15k / 13 KB | 77k / 18k / 15 KB | 155k / 29k / 90 KB |
| Function with a note | 170k / 26k / 78 KB | 151k / 17k / 34 KB | 185k / 25k / 82 KB |
| No note (add a flag) | 548k / 29k / 58 KB | 268k / 24k / 52 KB | 578k / 33k / 71 KB |
| Four tasks | 1,208k / 106k / 261 KB | **875k / 91k / 190 KB** | 1,401k / 126k / 376 KB |

Total tokens are dominated by cached prefix re-reads; uncached input and
retrieval bytes are the cleaner cost signals. The "no note" task varies
most between arms for reasons unrelated to memory (how many build attempts
the agent made), and one run per cell is not a distribution: treat the
per-task rows as a single sample and the totals as a direction. Measured
one way, memorycastle's agent read 27% fewer retrieval bytes than grep and
49% fewer than cognee's.

#### Honesty

- Absent topic: memorycastle says "no relevant note" on 10 of 11; grep and
  cognee answer all 11.
- Superseded note: asked about batch embedding, cognee serves the
  superseded note first, unlabelled, in both modes. memorycastle ranks the
  newer note above it and labels the old one.
- Changed function: memorycastle labels the hit `code changed since`;
  cognee and grep have no equivalent (not applicable, not a loss).

#### What the run found in memorycastle

The session-start hook started one embedding server per session: a server
that exited on idle removed the socket file that by then belonged to a
newer server, so the next session found nothing and started another. Ten
were resident at once during the agent runs. Fixed in 0.11.2: the server
claims the socket before loading the model, stands down if another answers,
and removes the file only if it is still its own.

The harness is `memcastle eval --gold <file>`; any system that implements
the `Memory` trait in `src/search.rs` can be scored by it.

The honest summary: if your memory is "what did we decide, try, measure, and
why", written by the agent in a fixed shape, retrieval over those notes is
the whole problem, and memorycastle solves it with one binary and no
network. If you need a knowledge graph built from unstructured documents,
use a tool that builds one.
