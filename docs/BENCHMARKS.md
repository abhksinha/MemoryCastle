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

### Compared with Mem0 and cognee

[Mem0](https://github.com/mem0ai/mem0) is the most widely adopted
open-source agent memory: an LLM extracts facts from each batch of
messages, a second pass decides whether each fact adds to, updates or
deletes a stored memory, and a vector store serves them by similarity;
with extraction off it is a vector store over the given text.
[cognee](https://github.com/topoteretes/cognee) is a well-built memory layer
and a fair representative of the LLM-extraction approach: `add` stores
documents, `cognify` runs an LLM over every chunk to extract entities and
relations into a graph store plus a vector store, and graph-aware search
modes call the LLM again at query time. That design can answer questions a
plain retriever cannot, such as multi-hop questions across documents. It
pays for that in moving parts and in per-token cost.

| | memorycastle | Mem0 | cognee |
| --- | --- | --- | --- |
| Language, runtime | Rust, one binary | Python package over a vector database, or the hosted API | Python package, async service |
| Stores | one SQLite file | vector store (Qdrant by default) + SQLite history log | vector store (LanceDB by default) + graph store (Kuzu by default) + relational metadata |
| Ingest | chunk, embed locally | chunk, LLM extraction of facts, embed, LLM decides add/update/delete against stored memories; or raw text with `infer=False` | chunk, embed, LLM extraction of entities and relations per chunk |
| Needs an LLM API key | no | yes in its intended mode (extraction and update); no for the raw vector mode | yes, for cognify and for graph search modes |
| Ingest cost per note | milliseconds of CPU, 0 tokens | two LLM calls per batch plus one embedding per fact (measured: a million tokens for ten notes) | one or more LLM calls per chunk |
| Query | BM25 + local embedding, under 0.3 s cold | one provider embedding plus a vector search, 0.4 to 0.7 s; 5 ms with the local small model | embedding plus optional LLM completion over retrieved graph |
| Memory footprint | 5 MB keyword, 285 MB with model loaded | Python process plus the vector database, 200 to 320 MB measured | Python process plus two database engines |
| Where the memory lives | markdown in your repo, under git | the vector store, as the LLM's paraphrase of the text | the framework's stores |
| Multi-hop reasoning over a graph | no, by design | in the paid graph tier | yes |

### Measured against Mem0, cognee and grep (2026-10-19)

A pre-registered, single-session run on an M5 MacBook Air. The plan, with
the questions, tasks and pass criteria, was written before any competitor
number was seen (cognee in the morning, Mem0 as an addendum in the
evening); the only changes after were setup fixes, logged with a time. The
notes and questions are private; the aggregates are below.

**Systems.** grep: term-presence count over whole notes (the `GrepMemory`
baseline in the harness). memorycastle 0.11.3: keyword plus meaning
(EmbeddingGemma 2 int8, 300M parameters, local, no key). Mem0 2.2.1, the
open-source `Memory` class over a Qdrant store on disk, in two
configurations: *raw*, `add(infer=False)` with its fastembed embedder
(`bge-small-en-v1.5`, 33M parameters), no key and no LLM, so each chunk is
stored verbatim, the apples-to-apples comparison; and *llm*, Mem0's
intended mode, `infer=True`, an LLM extracting facts from each chunk and
updating the memories already stored (`gpt-4o-mini`, because Mem0's default
model rejects Mem0's own sampling parameters at the OpenAI API, and
`text-embedding-3-small`). cognee 1.6.5 in two configurations: *local*,
GLiNER extractor and fastembed embeddings, no key; and *llm*, cognee's
defaults with a key; its best search type per cell is shown
(CHUNKS_LEXICAL throughout). Every system got the same chunks (heading
split, 200-word pieces) tagged with the note they came from; a result
counts when that note is the gold note.

**Corpora.** Three real journals, written by agents on three repositories:
Project 1, 45 notes, 215 chunks, 57 questions (the journal grew from 40 to
45 during the day; every system was re-scored on the 45-note store so the
row is one store for all); Project 2, 22 notes, 36 questions; Project 3,
10 notes, 29 questions. Each corpus also gets 11 absent-topic questions
whose right answer is nothing.

#### Retrieval (recall@3 / MRR; winner by MRR)

| corpus | memorycastle | Mem0 raw | Mem0 llm (key) | cognee local | cognee llm (key) | winner | by |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Project 1, 45 notes, 57 q | 0.941 / 0.838 | 0.912 / **0.866** | **0.965** / 0.813 | 0.825 / 0.747 | 0.842 / 0.778 | Mem0 raw | +0.028 MRR over memorycastle; memorycastle +0.025 over Mem0 llm, +0.060 over cognee llm |
| Project 2, 22 notes, 36 q | **1.000 / 0.976** | 1.000 / 0.954 | 1.000 / 0.903 | 0.972 / 0.917 | 0.972 / 0.965 | memorycastle | +0.011 over cognee llm, +0.022 over Mem0 raw, +0.073 over Mem0 llm |
| Project 3, 10 notes, 29 q | **1.000** / 0.897 | **1.000** / 0.902 | 0.966 / **0.909** | 0.966 / 0.885 | 0.966 / 0.905 | Mem0 llm | +0.012, one rank swap; memorycastle +0.034 recall@3 over both paid modes |

**Latest, 2026-10-20, memorycastle 0.11.5 against Mem0 raw on the same
stores, no key** (the journals as they stand today; the key-using modes
keep their pre-registered numbers above):

| today's stores, recall@3 / MRR | memorycastle 0.11.5 | Mem0 raw | winner |
| --- | --- | --- | --- |
| Project 1, 48 notes, 57 q | **0.968 / 0.853** | 0.895 / 0.829 | memorycastle, +0.024 MRR, +0.073 recall@3 |
| Project 2, 26 notes, 36 q | **1.000 / 0.975** | 0.972 / 0.964 | memorycastle, +0.011 |
| Project 3, 10 notes, 29 q, one label corrected | **1.000 / 0.938** | 1.000 / 0.925 | memorycastle, +0.013 |
| absent topic answered, of 11 | **0, 0, 2** | 11, 11, 11 | memorycastle |

Every system puts the right note in the top three on almost every
question; the MRR differences are one or two questions where two related
notes swap rank 1 and rank 2. Within noise on these sizes, in both
directions.

#### Absent topic (11 questions per corpus that should return nothing)

| | memorycastle | Mem0, raw and llm | cognee, every mode | grep | winner |
| --- | --- | --- | --- | --- | --- |
| answered anyway | **0 to 2 of 11** | 11 of 11 | 11 of 11 | 11 of 11 | memorycastle |

Mem0's `search` has a `threshold` (default 0.1, which nothing falls
under). Set at the lowest score any right answer got, so that recall is
untouched, it would still answer 1 to 2 of 11 absent questions on the two
small corpora in raw mode and 11 of 11 on Project 1 (a right answer there
scored 0.335); in llm mode 0, 5 and 7 of 11. The scores do not separate
"nothing on this" from "a right answer phrased differently"; a floor has to
be calibrated, which is what memorycastle's confidence is.

#### Speed and size

| | memorycastle | Mem0 raw | Mem0 llm | cognee local | cognee llm | winner | by |
| --- | --- | --- | --- | --- | --- | --- | --- |
| query p50 | 52 ms warm, 0.23 s cold, **2.5 ms keyword** | **4.5 to 6 ms** in process, model loaded | 0.4 to 0.7 s, one provider call per query | 30 to 190 ms | 40 ms lexical | Mem0 raw on meaning queries | 10x over memorycastle's warm meaning query; memorycastle's keyword path is the fastest of all |
| resident size of the answering process | **9 MB keyword, 300 MB meaning** | 300 to 316 MB, no keyword path | 200 to 235 MB plus the network | 730 to 760 MB | 520 to 610 MB | memorycastle | 33x on keyword answers; tie with Mem0 on meaning |
| ingest, 10 notes, from empty | 21 s, 535 MB, 0 tokens | **2.9 s, 300 MB, 0 tokens** | 685 s, 213 MB, 970k prompt + 30k completion + 43k embedding tokens | 193 s, 8.8 GB | 135 s, 511 MB, 183k tokens | Mem0 raw | 7x over memorycastle; memorycastle 33x over Mem0 llm and 6 to 9x over cognee |
| ingest, 45 notes, from empty | 68 s, 472 MB, 0 tokens | **3.3 s, 308 MB, 0 tokens** | 1,393 s, 235 MB, 2.35M prompt + 39k + 48k tokens | 72 s, 4.9 GB | 86 s, 586 MB | Mem0 raw | 20x over memorycastle; memorycastle 20x over Mem0 llm |
| one note edited, re-ingest | **0.19 s, old chunks replaced** | 0.15 s, but no content hash: the old chunks stay, every edit adds copies | not measured; about 6.5 s per chunk from the ingest rate, so about 30 s | 13.1 s, 3.0 GB | not measured | memorycastle | the only one that is both fast and correct |
| store on disk, 10 / 45 notes | **under 1 MB / 1.1 MB** | 0.9 MB / 1.5 MB | 7.7 MB / 8.9 MB | 25 MB / 28 MB | 70 MB / 118 MB | tie, memorycastle and Mem0 raw | |
| left running after a session | one `serve` process, 270 MB, 66 MB after 5 min idle, exits after 30 min | none (the MCP server is per session, 300 MB while it runs) | none | none | none | tie | |

Mem0 raw's speed is its embedding model: 33M parameters against 300M.
The same model was then measured inside memorycastle's own fusion (its
vectors and the gold queries injected into isolated stores as a new
space, BM25, graph rerank and the confidence floor unchanged), with and
without bge's recommended query instruction, recall@3 / MRR against
EmbeddingGemma 2 int8 on the same stores: Project 1 0.825 / 0.742 (0.825
/ 0.756 with the instruction) against 0.941 / 0.838; Project 2 (26 notes
by then) 0.936 / 0.920 against 0.979 / 0.963; Project 3 0.971 / 0.897
(0.941 / 0.853) against 1.000 / 0.897; absent questions answered 2 and 4
of 11 against 2 and 0 to 1. The small model loses the paraphrase
questions (the neural engine, the reindex timing, batching) that the
larger one was chosen for, on every journal, and some right answers fall
under a floor calibrated for the larger model. The 300M model stays; the
7x on ingest is not worth 0.04 to 0.10 MRR on a journal that is read far
more often than it is written.

#### Agents end to end

Codex 0.163, `codex exec`, a fresh session per run, four homes that differ
only in the memory: grep (no memory section, no tools), memorycastle (MCP
server, hooks, the installed instruction section), cognee (its MCP server
with `recall`) and Mem0 (a 40-line stdio MCP server over the open-source
`Memory.search`, tool `search_memory`, on the llm store of Project 3's
original journal), the last two with an instruction section of the same
length as memorycastle's. Same clone of Project 3's repository, four
pre-registered tasks, one run each. All four arms passed all four
rubrics: found the recorded rejection, reported the measured numbers and
their caveat, respected the recorded API-level constraint, and added a
flag without citing an invented decision. The difference is cost.

| | memorycastle 0.11.4 | memorycastle 0.11.3 | grep | cognee | Mem0 | winner | by |
| --- | --- | --- | --- | --- | --- | --- | --- |
| total tokens, four tasks | **700k** | 875k | 1,208k | 1,401k | 1,772k | memorycastle | 42% under grep, 50% under cognee, 61% under Mem0 |
| uncached input tokens | **71k** | 91k | 106k | 126k | 105k | memorycastle | 34% under grep, 44% under cognee, 33% under Mem0 |
| retrieval bytes read | **92 KB** | 190 KB | 261 KB | 376 KB | 194 KB | memorycastle | 65% under grep, 76% under cognee, 53% under Mem0 |

The 0.11.3 column is the pre-registered run. The 0.11.4 column is the
same four tasks rerun after the Mem0 result changed the default answer
shape (one line and the note's Decision sentence per hit, sections on
request, the change Mem0's short memories argued for); all four rubrics
passed again. The memory's own output is the part that halved; the rest
of the saving is the agent reading less around it.

Per task (total / uncached input / retrieval bytes; memorycastle is the
0.11.3 run, with 0.11.4 in brackets): rejected approach
411k/37k/111 KB grep, 379k/32k/90 KB memorycastle (220k/20k/34 KB),
483k/39k/133 KB cognee, 451k/36k/102 KB Mem0; measured number 79k/15k/13 KB, 77k/18k/15 KB (123k/17k/6 KB),
155k/29k/90 KB, 124k/24k/4 KB; function with a note 170k/26k/78 KB,
151k/17k/34 KB (198k/17k/33 KB), 185k/25k/82 KB, 173k/18k/48 KB; no note
548k/29k/58 KB, 268k/24k/52 KB (159k/16k/18 KB), 578k/33k/71 KB,
1,024k/28k/40 KB. One run per cell is a
direction, not a distribution; the "no note" task varies most for reasons
unrelated to memory (build attempts: the Mem0 run made 33 calls and took
244 s there). Mem0's extracted memories are short, which is why its
retrieval bytes on the measured-number task are the lowest of any arm.

#### Honesty

| | memorycastle | Mem0 | cognee | winner |
| --- | --- | --- | --- | --- |
| superseded note | labelled `superseded`, ranked below the note that replaced it | unlabelled; raw mode served the superseded note first (0.911 to 0.904), llm mode the successor first (0.819 to 0.796): the order is the score, which is chance | served first, unlabelled, in both modes | memorycastle |
| nothing on the topic | "no relevant note" | the nearest memories, at whatever score | the nearest wrong note | memorycastle |
| the function a note describes has changed | labelled `code changed since` | no equivalent | no equivalent | not applicable |

Score over the 16 rows above: memorycastle 9, Mem0 raw 4, Mem0 llm 1,
cognee 0, ties 2 (the retrieval-bytes tie went to memorycastle on the
0.11.4 rerun). Mem0's four are speed rows that belong to a model a
tenth the size; its one ranking win and cognee's are single rank swaps on
the smallest journal. memorycastle's eight are the rows about being
right: nothing when there is nothing, the reversed decision kept reversed,
an edit re-indexed in place, the smallest answering process, and the
cheapest agent session.

#### What the comparison changed in memorycastle

- **Brief default (0.11.4).** Mem0 serves one sentence per memory; on the
  measured-number task its agent read 4 KB where memorycastle's read 15
  KB, and the label log (22 notes served, 1 opened) said agents skim
  titles and skip sections. A hit is now one line and the note's first
  Decision sentence, about 50 tokens; `--sections` opens the note picked,
  `--full` is the older snippet block. The four tasks rerun: retrieval 190
  KB to 92 KB, tokens 875k to 700k, every rubric passed.
- **The 33M model measured inside the fusion and rejected.** Mem0 raw's
  speed is `bge-small-en-v1.5`. Its chunk and query vectors were injected
  into isolated memorycastle stores as a new space (no Rust change: the
  query cache is checked before the model opens) and scored with BM25,
  graph rerank and the floor unchanged:

  | recall@3 / MRR | bge-small 33M | with bge's query instruction | EmbeddingGemma 2 int8, 300M |
  | --- | --- | --- | --- |
  | Project 1, 45 notes | 0.825 / 0.742 | 0.825 / 0.756 | **0.941 / 0.838** |
  | Project 2, 26 notes | 0.936 / 0.920 | 0.936 / 0.920 | **0.979 / 0.963** |
  | Project 3, 10 notes | 0.971 / 0.897 | 0.941 / 0.853 | **1.000 / 0.897** |
  | absent answered | 2 and 4 of 11 | | 2 and 0 to 1 of 11 |

  It loses the paraphrase questions the larger model was chosen for (the
  neural engine, the reindex timing, batching, thread count) on every
  journal, and some right answers fall under a floor calibrated for the
  larger model's cosines. The same small model bare in Mem0 scored 0.866
  on Project 1: the fusion and the floor are tuned to one model's
  distribution, so a model swap needs per-space calibration first, which
  is now an open item. The 300M model stays.
- **The model's verdict cannot be overruled by words (0.11.5).** Looking
  at the absent questions memorycastle still answered showed the cause:
  confidence was the larger of the model's gated cosine and the share of
  the question's words present, so four everyday words ("state",
  "library", "pick") carried a chunk the model had put under the floor to
  0.50. Coverage is now capped at the model's own rescaled cosine when the
  model ran. On the current stores (the journals grew to 48 and 26 notes
  during the day), recall@3 / MRR / absent answered, before and after:
  Project 1 0.905 / 0.772 / 3 of 11 to 0.968 / 0.853 / 0; Project 2
  1.000 / 0.989 / 0 to 1.000 / 0.975 / 0; Project 3 1.000 / 0.912 / 4 to
  1.000 / 0.938 / 2 (the one question that first read as lost was a
  closed bug the reshaped copy had archived; its gold answer pointed at
  the old tracker, which word overlap alone had been lifting into the top
  three). The false hits had been filling the top ten on the
  largest journal, which is why recall rose as noise fell; one rank swap
  was lost on Project 2. The fused score is also scaled by the cosine, so
  a chunk BM25 ranked first on everyday words with the model near the
  floor scores at most half of one the model agrees with. A rule that
  muted BM25 when the question had no rare word was measured the same
  day and fired on 1 of 68 questions: in a small store almost every
  content word is rare. Mem0 raw was then re-scored, no key, on the same grown
  stores: Project 1 0.895 / 0.829, Project 2 0.972 / 0.964, Project 3
  (corrected label) 1.000 / 0.925, so on today's stores memorycastle
  0.11.5 leads it on all three journals, by 0.024, 0.011 and 0.013 MRR.
  The pre-registered tables above are unchanged; they are the 0.11.3
  numbers.
- **Why the 300M model is kept.** On ten notes any embedder finds the
  right one: few candidates, overlapping words. At forty-eight the
  misses are paraphrases ("was the neural engine tried" against a note
  that says CoreML and ANE; "how long did reindexing take" against one
  titled one chunk per call), and that is where the 33M model lost 0.10
  MRR inside the same fusion and the 300M one did not. A journal gains a
  note per task, questions drift from the wording that was written, and
  identifiers sit inside sentences; EmbeddingGemma 2 is trained on code
  and prose together, uses separate query and document prefixes, and
  holds 1,024 tokens of a dense chunk where the small model truncates at
  512. The cost is paid once per chunk at save, 0.2 s in a hook; the
  recall is collected on every search for the life of the journal, and
  the gap widens as the journal grows. Ingest speed is the wrong thing
  to optimise for something written once and read for months.
- **One embedding server (0.11.2).** Ten were resident during the agent
  runs; the fix is described under "What the run found".
- **Mem0's threshold tested.** Reported under "Absent topic" above, so the
  "no usable floor" claim rests on a number, not an assumption.

#### Why each winner matters

- **Absent topic.** An agent asks memory about something nobody wrote down
  several times per session. Mem0 and cognee return the nearest chunk
  every time, with a score and no usable floor, so the agent reads it,
  weighs it, and often acts on it: a wrong constraint applied, a rejected
  approach revived. The "no relevant note" answer is the one that reduces
  work. It is also what makes the workflow deterministic: the same
  question gives the same nothing, not a different plausible neighbour per
  embedding run.
- **Superseded note.** The one place a memory can actively mislead. A
  tool that serves the reversed decision first, unlabelled, is worse than
  no tool: the agent re-implements what the team already undid. Mem0 and
  cognee order the two by similarity, which is a coin toss between a
  decision and its reversal. The label and the rank in memorycastle come
  from a declared `Supersedes:` line, which is why they cannot be wrong.
- **Ranking.** A tie, and a tie is the honest result: every system finds
  the right note in the top three, and the rank-1 differences are one
  question each way. Rank 1 against rank 2 is one section read instead of
  two. Sessions are not won here; they are won on the rows above and
  below.
- **Ingest and re-ingest.** A note is edited or added at the end of every
  task. 0.19 s means `save` runs in a hook without anyone noticing and
  replaces what was there; Mem0 raw is as fast but keeps the old copies,
  so a journal edited daily fills with stale duplicates; Mem0 llm and
  cognee need seconds to minutes and a provider, so re-ingest runs in
  batches and the memory is stale between them. Zero tokens means no bill,
  no key, no network, and a laptop on a train.
- **Process size.** 9 MB for a keyword answer is the difference between a
  hook that can run before every edit and one that cannot. Mem0 has no
  keyword path: every answer loads a model. 300 MB for a meaning answer,
  released after 5 minutes idle, fits beside an IDE and a build; 750 MB
  per answering process does not.
- **Agent tokens.** Retrieval bytes are the part the memory controls:
  what the agent reads to find the fact. Mem0's short extracted memories
  matched memorycastle's sections, which is what made the brief default
  with the Decision sentence worth shipping: after it, memorycastle's
  agent reads half of Mem0's bytes and a quarter of cognee's for the same
  four correct answers, and the whole run is 42% to 61% cheaper, with one
  run per cell.
- **Where Mem0 wins.** Bare vectors over raw chunks with a 33M model are
  very fast to build and to query, and rank as well as anything on a
  journal this size. That mode is Mem0 as a vector store: no extraction,
  no dedupe, no floor, no supersession, duplicates on edit, a Python
  runtime and a vector database. Its memory features need the llm mode,
  which costs 685 s and a million tokens for ten notes and ranks below
  memorycastle on two of three journals. We report both; neither changes
  the choice for a repository's decisions.

#### Note shape

Project 3's notes predate the fixed shape (free headings, no dates, a bug
tracker carrying a dated work log); the pre-registered cognee run was on
those and scored memorycastle 0.856 to cognee llm 0.925. A copy reshaped
by hand (24 lines of headers and section names, the tracker's closed
entries moved to an archive as `lint` advises) gives the Project 3 row
above: memorycastle 0.877 to 0.897, cognee 0.925 to 0.905; Mem0 was run
on the reshaped copy only (and on the original for the agent arm, where
it scored 1.000 / 0.902 in llm mode). The shape is the lever on small
journals, not the ranker; tracker weighting, section weighting and finer
confidence buckets were measured the same day and changed no rank. The
reshaped copy was not written back: notes are never edited. Project 3's
figures are from an isolated store without the code index, so they differ
slightly from the main store's.

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

The Mem0 run found nothing new in memorycastle and one thing about the
corpus: Project 1 grew by five notes during the day, and two of the eleven
absent questions now land on a note at confidence 0.30 to 0.50 (a handoff
note that mentions many topics in passing). The floor holds; a note that
lists everything is the kind `quality` flags as low spread.

The harness is `memcastle eval --gold <file>`; any system that implements
the `Memory` trait in `src/search.rs` can be scored by it; the Mem0 and
cognee drivers live beside the private gold sets.

The honest summary: if your memory is "what did we decide, try, measure, and
why", written by the agent in a fixed shape, retrieval over those notes is
the whole problem, and memorycastle solves it with one binary and no
network. If you need a knowledge graph built from unstructured documents,
use a tool that builds one. If you want a fast vector store over raw
chunks, Mem0's raw mode is one, and it is not a memory.
