# Lightweight Agent Memory Architecture, v3.1

Oct 18, 2026 · @Abhi

v3 keeps v2's narrow scope (curated notes, search that usually needs no model, links without an LLM) and changes three things: one database with a project filter instead of a file per repo, EmbeddingGemma 2 text-only as the embedding model, and a C++ inference runtime accepted behind the `Embedder` trait. The review fixes from v2 (tokenizer, latency targets, query prefix, FTS triggers, write lock) are folded in. v3.1 keeps the single store and fixes the v3 review: ONNX Runtime is the default runtime, the model lifecycle matches it, FTS5 queries are quoted, the project filter lives inside the FTS index, and project ids survive remote URL changes. The changes are listed at the end.

## Goal

A single Rust binary, `memorycastle`, that gives Claude Code, Codex, agy and people a per-project memory of decisions, rejected attempts, measurements and preferences, across all projects from one store. It should match or beat Cognee on your own questions at a small fraction of its RAM, CPU and complexity.

Numbers in this document are targets until the evaluation harness has measured them. Cognee's column is what you measured on 2026-10-18.

| Measure | Cognee as used (measured) | Target here |
| --- | --- | --- |
| Idle RAM | about 1.0 GB after start, 2.5 GB after one search, 3.9 GB after a day of capture | 0 MB in CLI mode (no resident process); under 40 MB for an idle MCP server with the model unloaded |
| Idle CPU | 0% with capture off; about 480% while a capture backlog processed | 0% |
| Search, keyword hit | about 2 s through the plugin | under 10 ms |
| Search, needs the model | as above | under 1 s with a cold model load; under 30 ms warm on Metal, under 150 ms warm on CPU |
| Save one note | seconds to minutes (LLM extraction in the pipeline) | under 1 s cold, under 100 ms warm |
| RAM with model loaded | n/a | under 300 MB with the int8 text-only model |
| Install | Python runtime, server, several databases | one binary, the ONNX Runtime library (about 20 to 30 MB) and one model file of about 190 MB, all but the binary downloaded on first use |

## What goes into memory, and when agents use it

The biggest accuracy and cost lever is not the engine but what is stored and when it is read.

**Stored:** decisions and their reasons, things tried and rejected, measurements and baselines, user preferences and working rules.

**Not stored:** code, file layout and call graphs (the repo and graphify answer these exactly), git history, transcripts, tool output, secrets, and anything already in CLAUDE.md, AGENTS.md or the README.

**Agents search** before a design choice in an area with history, before trying a fix that may have been tried, when the user refers to the past ("like we did"), and when resuming paused work. They add `--related` when the user refers to a sister project. They do not search for routine edits, reading code or running tests.

**Agents save** only at the end of a task that changed a decision, and they save the curated file (a journal entry, a decisions note), never the conversation.

These rules ship as a short snippet for CLAUDE.md, AGENTS.md and GEMINI.md, printed by `memorycastle init`. Nothing is captured automatically.

## Design principles

When two conflict, the earlier one wins.

1. **One store, one filter.** All projects live in one SQLite file. Every query carries a project filter applied inside each search channel through a single `Scope` type, and one test proves a search in project A never returns a note that exists only in project B.
2. **The note text is the only source of truth.** Chunks, vectors and links are derived and can be rebuilt from the notes at any time. Changing the model is a reindex, not a migration.
3. **No resident process by default.** Every CLI command opens the file, does its work and exits. The model is loaded only when an embedding is needed. The MCP server is the one long-lived mode, and it unloads the model after five idle minutes.
4. **Search never needs an LLM.** Keyword search, vector arithmetic and links only. Reasoning over the results is the calling agent's job.
5. **Every derived row knows its source.** Each chunk, vector and link points to its note and records the model or rule version that produced it. Forgetting a note removes everything derived from it.
6. **The runtime is a dependency, not a principle.** A C++ inference library behind the `Embedder` trait is acceptable. Low RAM, fast search and few moving parts are the goals; pure Rust was a preference and is dropped where it costs accuracy or time.
7. **Nothing ships without a number.** Every change is gated by the harness: recall@k on the gold set, latency and RAM.

## System overview

&#91;embedded content: system overview · CLI and MCP, memorycastle core, one SQLite file, the on-demand embedding runtime\]

Agents call the CLI or the MCP server; the core is the only reader and writer of the one file, filters every access by project, and links the embedding runtime only on the path that needs a vector.

## Projects and isolation

One database at `~/.local/share/memorycastle/memory.db`. Every note and chunk carries a `project_id`, and every search filters on it exactly. Nothing lives inside a repo, so there is no `.memorycastle/` directory and no question of committing a database.

**Project identity.** The id is derived from the git remote URL when there is one, otherwise from the main repo path, so worktrees and clones resolve to the same project. The remote URL is normalised to `host/owner/repo` before hashing (scheme, user, `.git` suffix and case removed), so `git@github.com:abhi/x.git` and `https://github.com/Abhi/x` are one project. A repo with no remote uses its main repo path. When a remote changes or a repo moves, the project is found again through `projects.paths`, and `memorycastle project relink` updates the stored id, so memory never silently forks. A `projects` table maps id to a display name, a list of known paths and a `group`. Outside a git repo there is no project and every command refuses, unless `--project <name>` names one.

| Table `projects` | Example |
| --- | --- |
| `id` | sha of `git@github.com:abhi/abhigram.git` |
| `name` | `abhigram` |
| `paths` | `["~/code/abhigram", "~/code/abhigram-wt2"]` |
| `group` | `android`; `physics` for `physics-website`, `Physics_ISP`, `Abhiphysics` |

**Search scope, chosen per query.**

| Flag | Projects searched | Weight in fusion | When |
| --- | --- | --- | --- |
| none | the current project | 1.0 | default |
| `--related` | the current project and every project in its group | own 1.0, siblings 0.6 | "like we did on the website" |
| `--project 'physics*'` | the glob is expanded against `projects.name`, then filtered by id | each 1.0 | an explicit set |
| `--all` | every project | each 1.0 | "have I solved this before?" |

The glob is only ever matched against the `projects` table, never against note text, so relatedness is a fact you record once, not an accident of naming. Every result carries its project name so the agent can show where a memory came from.

**What this costs against one file per repo.** Isolation is a filter, not a wall; the single `Scope` path and its test are the wall. BM25 term weights are computed over all projects, which at a few thousand chunks does not change a top-10. All agents share one write lock; `busy_timeout` of 5 s makes a second `save` wait instead of fail. Forgetting a project is `memorycastle forget --project <name>`, a `DELETE` through lineage in one transaction.

**What it gains.** One file to back up, one embedding space for every project, cross-project search with no `ATTACH`, and no per-repo state.

## Storage

One SQLite file in WAL mode, compiled in through `rusqlite` with the `bundled` feature, opened with `busy_timeout = 5000` and `synchronous = NORMAL`. Six tables.

| Table | One row is | Key columns |
| --- | --- | --- |
| `projects` | A known project | `id`, `name`, `paths`, `group`, `added_at` |
| `notes` | One saved file or text, the source of truth | `id`, `project_id`, `path`, `title`, `content`, `content_hash`, `source` (user, agent, import), `saved_at`, `deleted_at` |
| `chunks` | A section of a note, split on headings, then by size (200 to 400 tokens; overlap of 1 sentence only when a section is split by size) | `id`, `note_id`, `project_id`, `ordinal`, `heading_path`, `text`, `text_indexed` |
| `chunks_fts` | FTS5 over `heading_path` and `text_indexed`, external content (`content=chunks`), BM25 ranking, plus `project_id UNINDEXED` | `unicode61` with `tokenchars '_-'`; see the tokenizer note. The project filter is a condition on this column inside the same query as `MATCH`, not a later join |
| `vectors` | One embedding of one chunk in one space | `chunk_id`, `space_id`, `vec` (f16 blob, 512 dims) |
| `links` | One mechanical reference found in a chunk (Layer 2) | `chunk_id`, `kind` (bug\_id, path, symbol, supersedes), `value` |

Plus `spaces` (model id, dimensions, precision, runtime, status: building, active, retired), `query_cache` (space id, prefixed query text, vector, last used) and `meta` (schema version, last eval).

**Tokenizer.** `text_indexed` is `text` with trailing punctuation stripped from every token (`rotated.` becomes `rotated`, `B-42,` becomes `B-42`) and with `.` left inside identifiers (`StatusGl.kt`). `tokenchars '_-'` keeps `B-42`, `snake_case` and `2026-10-18` whole; the price is that `read-only` is one token and "read only" does not match it. `.` is not a token character, so file names index as `StatusGl` and `kt`, which a phrase query `"StatusGl kt"` still finds. Trigram indexing is off by default and added only if the gold set shows identifiers missed.

**Queries are quoted.** In FTS5 query syntax an unquoted term cannot contain `-` or `.`, so `B-42` would fail or be misread. The query builder splits the user's text the same way `text_indexed` is built and wraps every term as a phrase string (`"B-42" "StatusGl" "kt"`), escaping embedded quotes. User text is never passed to `MATCH` raw. A test covers `B-42`, `snake_case`, `2026-10-18`, `StatusGl.kt` and a query containing a double quote.

**FTS5 stays in sync** through the standard three triggers on `chunks` (after insert, after delete, after update), so `save`, `forget` and `reindex` cannot drift the index. Soft-deleted notes are excluded by a join on `notes.deleted_at IS NULL`, and `forget --purge` hard-deletes through the triggers.

**Vector search is a plain scan.** A few thousand chunks at 512 dimensions in f16 is 1 KB each, a few MB in total, scanned in about a millisecond. The scan is restricted to the scope's `project_id` set before any arithmetic. An index is added only if the store passes about 100k chunks.

**Sizes at 5,000 chunks:** text about 3 MB, FTS about 2 MB, vectors 5 MB. The file stays under 30 MB.

## Embedding: EmbeddingGemma 2, text only

**Model.** `google/embeddinggemma-2`, released 2026-10-06 under Apache 2.0, loaded in text-only mode: the 270M-parameter text component, without the 170M vision and 300M audio encoders. 768 dimensions by default, truncated to 512 with Matryoshka and L2-normalised after truncation. Context 8,192 tokens, 100+ languages, which covers Hindi notes.

|  | EmbeddingGemma 2 text-only | Qwen3-Embedding-0.6B (fallback) |
| --- | --- | --- |
| Text parameters | 270M | 595M |
| MTEB Code (vendor-reported, to confirm against the model cards) | 78.7 | 74.6 |
| MTEB Multilingual v2 (to confirm) | 61.4 | 64.3 |
| MTEB English v2 (to confirm) | 68.5 | 70.7 |
| Quantised weights | about 190 MB | about 600 MB at Q8 |
| Licence | Apache 2.0 | Apache 2.0 |

It is chosen for the code-retrieval score, which is the benchmark closest to notes full of identifiers, paths and bug ids, and for the footprint. Qwen3 leads on general English and multilingual retrieval, so stage 3 runs both on the gold set before the choice is final. All scores are Google's own, published two weeks ago; no independent run exists yet.

**Precision.** The model card warns that float16 inference can produce NaNs or silently degraded vectors. The model runs in bfloat16, float32, or Google's int8 quantised checkpoint, never float16. Storing the output vectors as f16 is fine and is what `vectors.vec` does. The harness compares int8 against bfloat16 once; int8 stays if it loses less than one point of recall@10.

**Runtime.** ONNX Runtime by default, behind the `Embedder` trait and selected by a Cargo feature. Gemma 2 is built on the Gemma 4 architecture and is two weeks old, so stage 3 confirms that an ONNX export exists and is accurate before the choice is final; `llama` is the fallback if it does not.

| Feature | Crate | Runs | Notes |
| --- | --- | --- | --- |
| `ort` (default) | `ort` with `load-dynamic`, plus `tokenizers` | ONNX exports; CPU, CoreML execution provider measured on macOS | the ONNX Runtime library is a separate file loaded with `dlopen` only when an embedding is needed, so keyword-only commands never map it; model download, checksum and tokenisation are `memorycastle`'s own code. `fastembed-rs` is used only if it already supports EmbeddingGemma 2, since it adds dependencies |
| `llama` (fallback) | `llama-cpp-2` | any GGUF, Metal native | C++ built by `cmake` during `cargo build` and linked statically; used if Gemma 2 has no good ONNX export |
| `candle` | `candle` | Qwen3-Embedding today; Gemma 2 when ported | pure Rust, Metal; the fallback for the pure-Rust path |

With `ort`, keyword search, links, status and forget never load the runtime library. With the static `llama` fallback the runtime's code is in the binary but is never called on those paths, so it costs disk, not RAM.

**Query prefix.** EmbeddingGemma uses task prompts (the format below is EmbeddingGemma 1's; stage 3 confirms it is unchanged for version 2): queries are embedded as `task: search result | query: <text>` and documents as `title: <heading path> | text: <chunk>`. The prefix is part of the `query_cache` key and of the space's definition, so a prefix change is a new space.

**Model lifecycle.**

- ONNX Runtime reads the model file into its own session memory, so every process that loads the model holds its own copy, about 190 to 300 MB. Pages are not shared between agents. This is why the model is loaded only when an embedding is needed and why warm mode is opt-in. After first use the file sits in the OS cache, so a reload costs session creation, not disk reads; stage 3 measures it.
- With the `llama` fallback, GGUF weights are memory-mapped and CPU inference shares the cached pages between processes; on Metal they are copied into GPU buffers that are not shared.
- Stage 3 measures CPU against CoreML (or Metal on the fallback) for single queries and picks the cheaper; a 270M model on the CPU is expected to be fast enough for queries, and batch embedding on `save` and `reindex` may use the GPU.
- Recent query embeddings are cached in `query_cache` keyed by space and prefixed text, so a repeated question needs no model.
- Optional warm mode: `memorycastle serve --idle 5m` keeps one process with the model loaded for back-to-back searches and exits after five idle minutes. It is never started automatically.

**Spaces.** `spaces` records model id, dimensions, precision, runtime and prefix. `reindex --space new` embeds every chunk into a new space in the background while the old one stays active, then swaps. A model change is never a migration.

## Search pipeline

&#91;embedded content: search pipeline · BM25 always, warm-or-cold decision, meaning search, fusion\]

The warm path always fuses keyword and meaning results. Only the cold path may return keyword results alone, and only on an exact identifier hit.

1. **Resolve the scope.** The current project, widened by `--related`, `--project` or `--all`, becomes a set of project ids with weights. Every channel below filters on that set before ranking.
2. **BM25 over `chunks_fts`.** Always runs; under 10 ms.
3. **Decide whether to load the model.** If the model is already warm (`serve` mode, the MCP server, or a second query in a run), skip to step 4: a warm meaning search costs a few milliseconds, so there is no reason to return keyword-only. If the model is cold, return the keyword results now when the best hit is an exact identifier match (a bug id, a path or a symbol present verbatim in the query); otherwise load the model. `--meaning` forces the model, `--keyword` forbids it.
4. **Meaning search.** Embed the query with the search prefix (or take it from `query_cache`), scan `vectors` in the active space over the scope's chunks, keep the top 100.
5. **Fuse** both lists with reciprocal rank fusion (k = 60), then multiply by the project weight (own 1.0, siblings 0.6 under `--related`).
6. **Drop** deleted and superseded chunks. Return the top k with note path, project, heading, date and a trace.

**Every result carries a trace:** which channel found it, its rank in each, its project and its note. A miss is debugged with one command, `memorycastle search --explain`.

**Keyword-first threshold.** Exact identifier match only, to start. A BM25 score margin is a second knob added only if the gold set shows the exact-match rule returning too early or too late.

## Layer 2: links without an LLM

Your notes already carry their own references. On save, rules (not a model) extract them into `links`.

| Kind | Rule | Example |
| --- | --- | --- |
| `bug_id` | project pattern, configurable per project (`B-\d+`, `H-\d+`) | `B-42` |
| `path` | tokens that exist as files in the repo at save time | `ui/status/StatusGl.kt` |
| `symbol` | CamelCase and snake\_case tokens of three or more characters; no parser. A small stop-list drops product and platform names (`macOS`, `iPhone`, `GitHub`, `JavaScript`); a tree-sitter check against the repo is a later option if false positives still hurt | `CropRect`, `exportStatusJpeg` |
| `supersedes` | "superseded by", "replaced by", "fixed in" followed by a bug id or a note path | `Superseded by B-57` |

Dates are not links; `notes.saved_at` already orders chunks in time.

**Uses.**

- `memorycastle links B-38` lists every chunk that mentions B-38, oldest first, across the scope.
- A search can follow one hop: the best chunk plus the chunks that share its strongest link.
- `supersedes` down-ranks the older chunk only when its target resolves to a note or a bug id in the store. A target that is a commit hash is recorded but ranks nothing, because there is no note to point at.

Layer 2 is built only if the gold set shows link-type questions failing with Layer 1 alone. Well cross-referenced journals may not need it.

## Commands

| Command | What it does | Needs the model |
| --- | --- | --- |
| `memorycastle save <file>...` | Add or update curated notes in the current project; unchanged files are skipped by hash | Yes (new chunks only) |
| `memorycastle search "<query>" [-k 5] [--related] [--project <glob>] [--all] [--meaning] [--keyword] [--explain]` | Keyword first, meaning search if needed, scope as flagged | Only when cold and no exact identifier hit |
| `memorycastle links <value> [--related] [--all]` | Chunks that share a bug id, path or symbol (Layer 2) | No |
| `memorycastle forget <path or id> [--purge]` | Remove a note and everything derived from it; `--purge` hard-deletes | No |
| `memorycastle forget --project <name>` | Remove a whole project's memory in one transaction | No |
| `memorycastle project add`, `memorycastle project list`, `memorycastle project group <name> <group>`, `memorycastle project relink` | Register a project, list projects, set a project's group, re-attach memory after a remote or path change | No |
| `memorycastle reindex [--space new]` | Rebuild chunks, vectors and links from the notes, into a new space if the model changed | Yes |
| `memorycastle status` | Current project, note and chunk counts, active space, file size, notes changed in git since the last save, last eval | No |
| `memorycastle serve --idle 5m` | Keep the model warm for a session; exits after idle | Yes |
| `memorycastle eval` | Run the gold set and record recall@k, latency and peak RAM | Yes |
| `memorycastle init` | Print the agent snippet for CLAUDE.md, AGENTS.md or GEMINI.md | No |

**MCP server.** `memorycastle mcp` serves `search`, `save`, `links`, `status` and `project` over stdio for clients that prefer MCP. It resolves the project from the client's working directory, loads the model lazily like the CLI and unloads it after five idle minutes.

**Agent snippet.** When to search, when to add `--related`, when to save, and never to save transcripts, tool output or secrets.

## Safety

- **Memory is data, not instruction.** Every result is labelled with its note path, project and `source`, so text inside a note that reads like an instruction is shown as quoted content.
- **Isolation is tested.** The test suite saves a sentinel string into project B and asserts that no search from project A, with any flag except `--all` or a glob naming B, ever returns it.
- **Secrets.** `save` refuses files matching common secret patterns (keys, tokens, private-key blocks, `.env`) unless forced, and never follows symlinks out of the repo.
- **Forgetting** removes the note's chunks, vectors and links in one transaction. `memorycastle forget --verify` confirms nothing derived remains.
- **Backup** is a file copy, or `VACUUM INTO`. Everything except the notes can be rebuilt with `reindex`.

## Resource budget

Measured on the M5 Air with the harness. A target is a target until the Measured column has a value.

| Resource | Target | Measured |
| --- | --- | --- |
| Binary size | under 20 MB with `ort` (the ONNX Runtime library is a separate on-demand file); under 40 MB with the static `llama` fallback |  |
| RAM, idle, CLI mode | 0 MB (no process) |  |
| RAM, idle MCP server, model unloaded | under 40 MB |  |
| RAM, keyword search | under 30 MB peak |  |
| RAM, model loaded | under 300 MB per process with int8 text-only weights in ONNX Runtime |  |
| CPU, idle | 0% |  |
| Keyword search | p50 under 10 ms |  |
| Meaning search, model cold | under 1 s including load |  |
| Meaning search, model warm, Metal | p50 under 30 ms |  |
| Meaning search, model warm, CPU | p50 under 150 ms |  |
| Save one 300-token note, model warm | under 100 ms |  |
| Reindex 5,000 chunks | under 2 minutes on Metal |  |
| Disk, 5,000 chunks | under 30 MB plus the 190 MB model file |  |

**Why the fanless Air is fine.** The foreground path is one short model call or none. The only sustained load is `reindex`, which runs in batches and can be throttled without affecting a search.

## Evaluation harness

Built first, because it is the only thing that turns "better than Cognee" into a result.

- **Gold set:** 20 questions per project to start (AbhiGram, OCAPro, physics-website, Physics\_ISP, Abhiphysics), each with the note that answers it, written by hand from real work. At least five are cross-project questions that need `--related`. It grows from real sessions, and questions are never edited after being added.
- **Metrics:** recall@3 and recall@10, MRR, p50 and p95 latency for keyword hits and meaning searches (cold and warm, CPU and Metal), peak RAM, disk size.
- **Baselines:** Cognee (as configured on 2026-10-18, per-project datasets, curated notes) and plain `rg` over the same notes. Beating `rg` is the first bar; beating Cognee is the second.
- **Model comparison:** EmbeddingGemma 2 text-only against Qwen3-Embedding-0.6B, both at their int8 and full-precision checkpoints, on the same gold set. This decides the default model.
- **Ablations:** keyword only, meaning only, both fused, with and without Layer 2, cold-path early return on and off.
- **Gates, each of which can fail:** no change may lower recall@10 by more than one point, raise keyword-search peak RAM above 30 MB, raise keyword p50 above 10 ms, or make the isolation test fail.
- Public long-conversation benchmarks (LongMemEval, LoCoMo) are not used: they test memory of chat transcripts, which this tool does not store.

## Build plan

Each stage has an exit test, and the next stage starts only when it passes.

1. **Harness and gold set.** `memorycastle eval` against a stub; 100 questions across five projects; Cognee and `rg` baselines recorded. Exit: the harness reports recall@k, latency and RAM for any implementation of the `Memory` trait.
2. **Keyword store.** Schema with `project_id`, the `projects` table and groups, `save` (heading chunking, hashing, punctuation stripping), FTS5 with triggers, `search` with `--related`, `--project`, `--all`, the isolation test, `forget`, `status`, `init`. Exit: recall@10 recorded; beats `rg`; keyword p50 under 10 ms; keyword peak RAM under 30 MB; isolation test passes.
3. **Meaning search.** EmbeddingGemma 2 text-only through the `ort` feature with `load-dynamic` (or `llama` if no good ONNX export exists), int8, 512 dimensions, query prefix confirmed, lazy load, `query_cache`, RRF, the warm-or-cold rule. CPU against CoreML measured; int8 against bfloat16 measured; Gemma 2 against Qwen3 measured. Exit: recall@10 improves over stage 2; cold and warm latency within budget; model and precision chosen by the numbers.
4. **Daily use.** Replace Cognee in `memorycastle`'s existing commands for all five projects; MCP server with project resolution. Exit: two weeks of use, 50 more gold questions from real sessions, no data loss, no cross-project leak observed.
5. **Layer 2, only if needed.** Link extraction, `links`, one-hop expansion, supersede ranking. Exit: link-type gold questions improve without slowing keyword search.

Stages 1 to 3 are the commitment. If stage 3 does not match Cognee on the gold set, the result is still a measured answer, and Cognee stays.

## Open source

Once stage 4 passes, publish as its own repository.

- **Licence:** dual MIT and Apache-2.0 for the code. EmbeddingGemma 2 is Apache 2.0, so there is no separate model licence to explain; the weights are downloaded on first use with a checksum, never committed.
- **Dependencies:** `rusqlite` (bundled), `serde`, `serde_json`, `half`, `clap`, `tokenizers` with the `onig` feature off, and one runtime crate behind a feature (`ort` by default, `llama-cpp-2` or `candle`). With `ort` and `load-dynamic` no C++ toolchain is needed to build; the ONNX Runtime library (MIT) is downloaded at first use with a pinned checksum. The `llama` fallback needs `cmake` and a C++ toolchain, stated in the README. `cargo deny` for licences and advisories, as in AbhiGram.
- **No personal data:** the published benchmark uses synthetic notes; your gold set stays private.
- **Interop:** optional `memorycastle import --cognee <dataset>` and `memorycastle export --cognee`, so it works next to Cognee instead of replacing it by force. No Cognee code is copied unless its licence allows it and it is credited.

## Risks and open questions

| Risk | Likelihood | Mitigation |
| --- | --- | --- |
| EmbeddingGemma 2 has no ONNX export yet, or the quantised export loses accuracy | Medium: the model is two weeks old | Export it once with Optimum and pin the checksum; then the `llama` fallback; EmbeddingGemma 1 and Qwen3-0.6B already have ONNX exports and are the model fallbacks |
| ONNX Runtime library missing or the wrong version on a user's machine | Medium | Download a pinned build next to the model on first use; a clear error, never a crash; keyword search keeps working |
| Two agents each load the model and hold separate copies | Medium | Model loaded only when needed and unloaded after idle; `memorycastle serve` as one shared warm process for heavy sessions |
| A remote URL change forks a project's memory | Low after normalisation | `projects.paths` match and `memorycastle project relink` |
| Google's benchmark numbers do not hold on your notes | Medium | The gold-set comparison with Qwen3 decides, not the vendor table |
| A missed `project_id` filter leaks across projects | Low with the single `Scope` path | The isolation test is a CI gate |
| Cold model load is slower than 1 s | Low: 190 MB of weights | mmap and OS cache; `memorycastle serve --idle`; keyword-first avoids most loads |
| Keyword-first threshold returns a weak hit too early on the cold path | Medium | Exact identifier match only; `--meaning` forces vector search; warm path never skips |
| Gold set too small or too easy | High at first | Grow it from real sessions; keep `rg` as a sanity baseline |
| Agents forget to save after tasks | Medium | The agent snippet; `memorycastle status` shows notes changed in git since the last save |
| C++ toolchain makes the open-source build harder for others | Low with `ort` as default | Only the `llama` fallback needs `cmake`; prebuilt release binaries for macOS and Linux |

**Open questions.**

- [ ] Does EmbeddingGemma 2 have an accurate ONNX export, or does stage 3 fall back to `llama`?
- [ ] Are the task prompts unchanged from EmbeddingGemma 1?
- [ ] CPU or Metal for single query embeddings? A 270M model may be fast enough on the CPU to avoid the GPU buffer copy. Measured in stage 3.
- [ ] Does the 0.6 sibling weight under `--related` need tuning? Only the gold set's cross-project questions can say.
- [ ] Is Layer 2 needed at all after stage 3?

## Changes from v2

| v2 | v3 | Why |
| --- | --- | --- |
| One SQLite file per repo at `<repo>/.memorycastle/memory.db` | One file at `~/.local/share/memorycastle/memory.db` with a `project_id` filter and a `projects` table | One backup, one embedding space, no per-repo state, cross-project search without `ATTACH`; isolation becomes a tested filter |
| Cross-project search through `ATTACH` | `--related` (group), `--project <glob>` (expanded against the `projects` table) and `--all` | Relatedness is recorded once instead of guessed from names; sibling results are down-weighted and labelled |
| Qwen3-Embedding-0.6B Q8\_0, about 600 MB | EmbeddingGemma 2 text-only, int8, about 190 MB, Apache 2.0 | Higher vendor-reported code retrieval, a third of the RAM, simpler licence; Qwen3 kept as the measured fallback |
| Pure Rust preferred, `llama.cpp` as a fallback | C++ runtime accepted behind the `Embedder` trait, chosen by Cargo feature | Gemma 2 runs on `llama.cpp` and ONNX first; pure Rust was a preference, not a goal |
| `tokenchars '_-.'` | `tokenchars '_-'` plus trailing-punctuation stripping into `text_indexed` | `.` as a token character glued sentence-final periods onto words |
| Warm meaning search under 30 ms | 30 ms on Metal, 150 ms on CPU | A 0.6B model on the CPU was unlikely to hit 30 ms; 270M may |
| Idle RAM gate "above 0 MB" | Keyword-search peak RAM under 30 MB, plus the isolation test | The 0 MB gate could never fail |
| Keyword-first early return whenever the best hit is strong | Early return only on the cold path with an exact identifier match; the warm path always fuses | Warm vector search is nearly free, so skipping it only lost recall |
| `supersedes` marks the older chunk | Only when the target is a note or bug id in the store | A commit-hash target has nothing to down-rank |
| `symbol` links via tree-sitter | CamelCase and snake\_case tokens, no parser | Keeps the dependency list short; tree-sitter is a later option |
| `date` links | Removed | `saved_at` already orders chunks in time |
| No query prefix | Task prefixes on queries and documents, part of the space definition | The model is trained with them; omitting them costs accuracy |
| FTS5 external content, sync unspecified | Three triggers on `chunks` | Prevents index drift on `forget` and `reindex` |
| No write-lock handling | `busy_timeout = 5000` | Two agents saving at once wait instead of failing |

## Changes from v3

| v3 | v3.1 | Why |
| --- | --- | --- |
| `llama` as the default runtime candidate, `ort` second | `ort` with `load-dynamic` as the default, `llama` as the fallback | ONNX was the chosen runtime; no C++ toolchain needed to build; keyword-only commands never map the runtime |
| `ort` through `fastembed-rs` | Plain `ort` plus `tokenizers`; `fastembed-rs` only if it already supports Gemma 2 | Fewer dependencies; the new model may not be in `fastembed-rs` yet |
| Lifecycle said weights are memory-mapped and shared between agents | Each process holds its own ONNX Runtime copy; sharing applies only to the `llama` CPU fallback | The v3 text described GGUF, not ONNX Runtime |
| "The runtime is only linked into the embedding path" | Dynamic loading for `ort`; static code costs disk, not RAM, for `llama` | Static linking puts the code in the binary regardless |
| Project filter applied by a join after `MATCH` | `project_id UNINDEXED` inside `chunks_fts`, filtered in the same query | Isolation and speed at the index, one place to test |
| User query passed to FTS5 as written | Every term quoted as a phrase string, quotes escaped, tested | Unquoted `B-42` and `StatusGl.kt` break FTS5 query syntax |
| Project id from the raw remote URL | Normalised `host/owner/repo`, path fallback, `memorycastle project relink` | SSH and HTTPS remotes of one repo hashed to two projects |
| MTEB table stated as fact | Marked "to confirm against the model cards" | Vendor numbers two weeks old, not yet cross-checked |
| Task prompts assumed | Marked as EmbeddingGemma 1's format, confirmed in stage 3 | Version 2 may differ |
| `symbol` rule without a stop-list | Stop-list for product and platform names | `macOS` and `iPhone` matched the CamelCase rule |
| Broken `memorycastle project` row in the commands table | Split into named subcommands, including `relink` | Pipes inside backticks split the table |
| Install: binary plus model | Binary, ONNX Runtime library and model, the last two downloaded on first use | Matches the `ort` runtime |
