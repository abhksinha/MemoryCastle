# Interlinkage graph from declared lines, bit-packed vector prefilter, limits module
Author: abhi+claude | Date: 2026-10-18
Files: src/graph.rs, src/graph.html, src/limits.rs, src/search.rs, src/db.rs, src/store.rs

## Context
Asked to become a hybrid of graphify (graph of a codebase) and cognee (LLM-extracted memory graph) without their cost, with bit-packed structures and Bloom or XOR filters where they help, and with every memory bound defined.

## Decision
`links` table (schema v5) filled at save from `Files:`, `Supersedes:`, backticked paths and symbols, and mentions of other notes; commands `links`, `history`, `graph`; `status` lists notes about files that no longer exist. `graph` writes `memorycastle/graph/{nodes,edges}.tsv` and a self-contained `graph.html` (inline JSON, canvas force layout, union-find communities). Vector channel gained a bit-packed prefilter (schema v6, `vector_bits`): one sign bit per dimension, 72 B per vector in one blob per project, XOR + popcount, exact rerank of 300 candidates; engaged above 512 vectors in scope. `src/limits.rs` holds every bound with the bytes it controls and compile-time relations. Journal moved to `memorycastle/journal/`, graph to `memorycastle/graph/`.

## Rejected
LLM entity extraction (cognee style): on template notes it re-emits the headings and adds a second source of truth. Code AST graph inside memorycastle (graphify style): the agent has grep, LSP and graphify for structure; the memory tool only needs file and symbol names, which the note already states. Bloom filter for term presence: FTS5 answers in under 1 ms. XOR filter on the query cache: 256-row primary-key lookup. JSON as canonical graph export: TSV is cheaper to parse for agents and shell, JSON only inside the HTML where the browser parses it natively. Symbol edges in community detection: `id`, `Result` join everything.

## Measured
3,619 chunks: exact scan 3.0 ms, prefilter plus rerank 1.6 ms, top-1 identical on 5 of 5 queries, top-10 overlap 9 of 10 average, pack 260 KB, rebuild 16 ms. Gold 34 questions: recall@3 1.000, recall@10 1.000 unchanged. `graph` at 700 notes: 9 ms warm, 59 ms including link build, 638 KB. `links <file>`: under 1 ms, 7 MB RSS. Keyword search at scale 4.8 MB RSS, meaning 288 MB. Token cost of "what do we know about file X" on the AbhiGram journal: `links` answer 136 to 238 bytes, the linked notes in full 3.8 to 45.7 KB, the whole journal 79 KB.

## Open
Prefilter recall is measured on five queries and the gold set; a larger gold set with near-duplicate notes would stress it. Communities are connected components, which merge into one at 700 synthetic notes sharing few files; label propagation is the next step if real journals show the same. CoreML stays off.
