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
time. The notes and questions are private; the aggregates are below.

**Systems.** grep: term-presence count over whole notes (the `GrepMemory`
baseline in the harness). memorycastle 0.11.x: keyword plus meaning
(EmbeddingGemma 2 int8, local, no key). cognee 1.6.5 in two
configurations: *local*, GLiNER extractor and fastembed embeddings, no key,
the apples-to-apples comparison; and *llm*, cognee's defaults (an OpenAI
chat model for extraction, `text-embedding-3-large`) with a key. cognee's
best search type per cell is shown (CHUNKS_LEXICAL throughout; CHUNKS was
lower everywhere). A result counts when the note file it came from is the
gold note.

**Corpora.** Three real journals, written by agents on three repositories:
Project 1, 40 notes, 193 chunks, 57 questions; Project 2, 22 notes, 36
questions; Project 3, 10 notes, 29 questions. Each corpus also gets 11
absent-topic questions whose right answer is nothing.

#### Retrieval (recall@3 / MRR; winner by MRR)

| corpus | memorycastle | cognee local | cognee llm (key) | winner | by |
| --- | --- | --- | --- | --- | --- |
| Project 1, 40 notes, 57 q | **0.965 / 0.851** | 0.860 / 0.753 | 0.860 / 0.787 | memorycastle | +0.064 MRR vs llm, +0.098 vs local |
| Project 2, 22 notes, 36 q | **1.000 / 0.976** | 0.972 / 0.917 | 0.972 / 0.965 | memorycastle | +0.011 vs llm, +0.059 vs local |
| Project 3, 10 notes, 29 q | **1.000** / 0.897 | 0.966 / 0.885 | 0.966 / **0.905** | cognee llm | +0.008, one rank swap; memorycastle +0.034 recall@3 vs both |

#### Absent topic (11 questions per corpus that should return nothing)

| | memorycastle | cognee, every mode | grep | winner |
| --- | --- | --- | --- | --- |
| answered anyway | 0 to 1 of 11 | 11 of 11 | 11 of 11 | memorycastle |

#### Speed and size

| | memorycastle | cognee local | cognee llm | winner | by |
| --- | --- | --- | --- | --- | --- |
| query p50 | 52 ms warm, 0.23 s cold, 2.5 ms keyword | 30 to 190 ms after model load | 40 ms lexical, 0.6 to 1.0 s CHUNKS | memorycastle | tie with cognee lexical on small corpora; 12 to 20x vs llm CHUNKS |
| resident size of the answering process | 9 MB keyword, 300 MB meaning | 730 to 760 MB | 520 to 610 MB | memorycastle | 2.4x vs local, 1.7x vs llm |
| ingest, 10 notes, from empty | 21 s, 535 MB peak, 0 tokens | 193 s, 8.8 GB | 135 s, 511 MB, 74k prompt + 53k completion + 56k embedding tokens | memorycastle | 9x faster and 16x less memory vs local; 6x faster vs llm, no tokens |
| ingest, 40 notes | 55 s, 471 MB | 164 s, 4.7 GB | 82 s, 604 MB, 275k tokens | memorycastle | 3x vs local, 1.5x vs llm |
| one note edited, re-ingest | 0.19 s | 13.1 s, 3.0 GB | not measured | memorycastle | 68x |
| store on disk, 10 notes | under 1 MB | 25 MB | 70 MB | memorycastle | 25x |
| left running after a session | one `serve` process, 270 MB, 66 MB after 5 min idle, exits after 30 min | none (the agent starts and stops the MCP server) | none | tie | |

#### Agents end to end

Codex 0.163, `codex exec`, a fresh session per run, three homes that differ
only in the memory: grep (no memory section, no tools), memorycastle (MCP
server, hooks, the installed instruction section) and cognee (its MCP
server with `recall`, an instruction section of the same length). Same
clone of Project 3's repository, four pre-registered tasks, one run each.
All three arms passed all four rubrics: found the recorded rejection,
reported the measured numbers and their caveat, respected the recorded
API-level constraint, and added a flag without citing an invented
decision. The difference is cost.

| | memorycastle | grep | cognee | winner | by |
| --- | --- | --- | --- | --- | --- |
| total tokens, four tasks | **875k** | 1,208k | 1,401k | memorycastle | 28% under grep, 38% under cognee |
| uncached input tokens | **91k** | 106k | 126k | memorycastle | 14% under grep, 28% under cognee |
| retrieval bytes read | **190 KB** | 261 KB | 376 KB | memorycastle | 27% under grep, 49% under cognee |

Per task (total / uncached input / retrieval bytes): rejected approach
411k/37k/111 KB grep, 379k/32k/90 KB memorycastle, 483k/39k/133 KB cognee;
measured number 79k/15k/13 KB, 77k/18k/15 KB, 155k/29k/90 KB; function with
a note 170k/26k/78 KB, 151k/17k/34 KB, 185k/25k/82 KB; no note 548k/29k/58
KB, 268k/24k/52 KB, 578k/33k/71 KB. One run per cell is a direction, not a
distribution; the "no note" task varies most for reasons unrelated to
memory (build attempts).

#### Honesty

| | memorycastle | cognee | winner |
| --- | --- | --- | --- |
| superseded note | labelled `superseded`, ranked below the note that replaced it | served first, unlabelled, in both modes | memorycastle |
| nothing on the topic | "no relevant note" | the nearest wrong note | memorycastle |
| the function a note describes has changed | labelled `code changed since` | no equivalent | not applicable |

Score: memorycastle wins 12 rows, cognee 1, by a single rank swap on the
smallest corpus with a paid key.

#### Why each winner matters

- **Absent topic.** An agent asks memory about something nobody wrote down
  several times per session. cognee returns the nearest chunk every time,
  with a score and no floor, so the agent reads it, weighs it, and often
  acts on it: a wrong constraint applied, a rejected approach revived. The
  "no relevant note" answer is the one that reduces work. It is also what
  makes the workflow deterministic: the same question gives the same
  nothing, not a different plausible neighbour per embedding run.
- **Superseded note.** The one place a memory can actively mislead. A
  tool that serves the reversed decision first, unlabelled, is worse than
  no tool: the agent re-implements what the team already undid. The label
  and the rank come from a declared `Supersedes:` line, which is why it
  cannot be wrong.
- **Ranking on a growing journal.** The gap widens with corpus size (0.008
  behind at 10 notes, 0.011 ahead at 22, 0.064 ahead at 40, against the
  paid configuration). A journal grows every day; the result that matters
  is the one at 40 notes and beyond. Rank 1 versus rank 2 is the
  difference between one section read and two.
- **Ingest and re-ingest.** A note is edited or added at the end of every
  task. 0.19 s means `save` runs in a hook without anyone noticing; 13 s
  and 3 GB means it runs in batches, and the memory is stale between them.
  Zero tokens means no bill, no key, no network, and a laptop on a train.
- **Process size.** 9 MB for a keyword answer is the difference between a
  hook that can run before every edit and one that cannot. 300 MB for a
  meaning answer, released after 5 minutes idle, fits beside an IDE and a
  build; 750 MB per answering process does not.
- **Agent tokens.** Retrieval bytes are the part the memory controls:
  what the agent reads to find the fact. Half the bytes of cognee for the
  same four correct answers is half the context spent on memory and the
  rest kept for the task.
- **The one cognee win.** On a 10-note journal with wording close to the
  questions, a paid embedding model orders two related notes slightly
  better. It needs a key, a provider call per ingest, and still answers
  every absent question. We report it; it does not change the choice.

#### Note shape

Project 3's notes predate the fixed shape (free headings, no dates, a bug
tracker carrying a dated work log); the pre-registered run was on those
and scored memorycastle 0.856 to cognee llm 0.925. A copy reshaped by hand
(24 lines of headers and section names, the tracker's closed entries moved
to an archive as `lint` advises) gives the Project 3 row above:
memorycastle 0.877 to 0.897, cognee 0.925 to 0.905. The shape is the lever
on small journals, not the ranker; tracker weighting, section weighting
and finer confidence buckets were measured the same day and changed no
rank. The reshaped copy was not written back: notes are never edited.
Project 3's figures are from an isolated store without the code index, so
they differ slightly from the main store's.

#### What the run found in memorycastle

The session-start hook started one embedding server per session: a server
that exited on idle removed the socket file that by then belonged to a
newer server, so the next session found nothing and started another. Ten
were resident at once during the agent runs. Fixed in 0.11.2: the server
claims the socket before loading the model, stands down if another answers,
and removes the file only if it is still its own. A second defect: the
code-changed pass re-sorted hits by fused score alone, undoing the
confidence order; `eval` bypasses that pass, so the tables are unaffected,
the agent runs were. Fixed the same day.

The harness is `memcastle eval --gold <file>`; any system that implements
the `Memory` trait in `src/search.rs` can be scored by it.

The honest summary: if your memory is "what did we decide, try, measure, and
why", written by the agent in a fixed shape, retrieval over those notes is
the whole problem, and memorycastle solves it with one binary and no
network. If you need a knowledge graph built from unstructured documents,
use a tool that builds one.
