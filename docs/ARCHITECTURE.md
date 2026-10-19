# Architecture

- **Storage**: one SQLite file. `notes` (full text), `chunks` (by heading,
  200 words), `chunks_fts` (FTS5, BM25), `vectors` (512-d f16, 1 KB per
  chunk), `spaces` (which model produced the vectors). Every row carries a
  `project_id`; every query filters on it before ranking.
- **Project identity**: read from `.git` on disk, remote normalised to
  `host/owner/repo`, worktrees resolve to the main repo. No `git` process.
- **Search**: BM25 always. The model loads only when the query needs it
  (warm server, `--meaning`, or no exact identifier hit). The vector channel
  is a plain scan over the project's rows: 3 ms at 3,600 chunks. The two
  channels fuse with reciprocal rank fusion. Every hit carries a trace.
- **Embedding**: EmbeddingGemma 2 through ONNX Runtime, loaded dynamically.
  The tokenizer is a 300-line BPE that reads a 12 MB binary built once from
  `tokenizer.json`; the HuggingFace crate cost 376 MB resident for the same job.
- **No index structures** until measured need: no ANN, no graph, no Bloom
  filters. Under about 100k chunks the scan is cheaper than any index.

The design document is `Lightweight_Agent_Memory_Architecture_v3.1.md`. The
engineering rule applied to every change is
`Resource_Conscious_Software_Engineering.md`: do less work, allocate less,
move less, and measure before and after.

## Graph nodes

`memcastle graph` writes `nodes.tsv` with, per node: id, kind (note, file,
symbol), label, key, date, superseded, author, community, degree, sections
(bit-packed: Context 1, Decision 2, Rejected 4, Measured 8, Open 16), exists,
loc, summary. For a note, `summary` is the first sentence of its Decision
section and `loc` its path. For a file, `exists` says whether the checkout
still has it; a bare name such as `ChatScreen.kt` is resolved through a bounded
walk of the checkout and `loc` holds the resolved path. For a symbol, `loc` is
`path:line` of its definition (or first mention) in the files its notes name,
and `exists` is false when it was found in none of them; an `in` edge links a
located symbol to its file. `edges.tsv` holds src, dst, kind (file, note,
symbol, in). The viewer colours missing nodes red and offers `file://` links.

## Code layer (`graph --code`)

`src/code.rs` walks the checkout (bounded, skipping VCS and build output),
hashes each source file, and for changed files records definitions
(keyword-before-name: `fn`, `def`, `fun`, `class`, `struct`, `val`, `const`
and so on), imports resolved against the checkout by file name (`mod`,
`use crate::`, Kotlin and Java `import`, Python `from … import`, relative
JavaScript imports, `#include "…"`), and mentions (identifier tokens that
match a definition elsewhere, top 200 per file). Names defined in more than
20 files or used from more than 100 are dropped as vocabulary. Tables
`code_files`, `code_defs`, `code_imports`, `code_mentions` (schema v7), all
derived, never on the search path. `graph` merges this layer under the note
layer: a note's symbol lands on the definition node, areas become
directories, and the HTML shows the navigation subset by default.

## Precision (what "relevant" means)

Every hit carries a confidence in 0 to 1. With the model: the cosine between
the question and the chunk, rescaled so that 0.60 is 0 and 0.80 is 1 (the
gold set shows off-topic questions peak at 0.65 and right answers start at
0.67). Without the model: the share of the question's content words in the
chunk. An identifier from the question found verbatim is 1.0. Under 0.30 the
hit is dropped and the search says "no relevant note"; the gold set keeps
five off-topic questions that must return nothing. The wording of a question
also picks a section: numbers want Measured, "why" wants Decision, "tried"
wants Rejected (1.3x on that section's chunks). Duplicate chunks collapse to
one, the dated note preferred over a tracker. Hits print `note.md:line` from
`chunks.line`, and the default snippet is the sentence with most question
words plus one each side; `--sections` prints the whole chunk and sections.

## Graph rerank

The question's files and symbols (backticked or bare tokens that look like a
path or an identifier) are matched against each candidate note's declared
links: a direct link weights the note 1.5x, and a symbol the code index
defines in a file the note links weights it 1.2x. Everything is declared or
indexed; nothing is inferred. `--explain` shows the factor per hit.

## Code spans

Each definition in the code index carries its end line and its anchor (the
definition line's text). `links <symbol>` and `search --sections` print
`path:start-end`, so an agent reads the lines that matter instead of the
file. When the file's content hash differs from the index, the span is
re-found at output time by the anchor line, then by name, and the end is
recomputed; nothing stale is printed. Brace depth with strings and comments
blanked for brace languages, indentation for Python, expression bodies for
Kotlin. Measured 38 of 40 random spans correct on a Rust and Kotlin
checkout; the misses are blocks whose closing brace sits at another indent.

## Savings counter

Each `search` and `links` call stores two numbers: bytes it printed, and the
byte size of every distinct note and file it drew from, read whole (the
notes it returned blocks from; for `links`, also the file it located, at
most 32 KB). `status`
reports tokens saved, `(stood_for − served) / 3.2`, and the share
`100 × (stood_for − served) / stood_for` rounded, cumulative and for the
last 24 hours. The divisor is measured once with the built-in tokenizer and
fixed (`limits::BYTES_PER_TOKEN`), so the number is local, deterministic,
and needs no model. Each file counts at most 32 KB (`limits::USAGE_FILE_CAP`),
since agents do not read a huge file whole. It counts what the agent would
have read if it had opened those notes and files, which is what grep-only agents did in the
benchmark logs; it does not count the agent's prompt, thinking or answer.

### Observed reads

A `PostToolUse` hook on Read and Bash (`plugin/hooks/after-read.py`) reports
each file read to `memcastle read <path> --bytes N`, with N the bytes the
tool returned. The hook prints nothing, so it costs no tokens; one keyword
call, under 1 ms. Every span `search` or `links` prints goes to
`served_spans`; a read of that path within `limits::SPAN_FOLLOW_SECS` (600)
is `guided`. The observed saving is a range over guided reads: the floor
counts against a grep-then-range read of `limits::RANGE_READ_LINES` (150)
lines at the file's average line length, the ceiling against the whole
file (schema v12, `reads.range_bytes`). Bytes read are exact; the truth of
what would have happened sits between the two baselines. Reads of the journal are not counted. Codex and
Antigravity have no hooks and get only the estimate.

## Ranked lines to read (`src/spans.rs`)

After the hits, `search` prints up to `limits::READ_SPANS` (5) spans in
reading order. Candidates come from four graph walks, each with a fixed
score: a question symbol defined in the code index (1.0); symbols the
top-ranked notes link, decaying by note rank (0.9, 0.77, ...); the first
line where a file in `code_mentions` uses a question symbol, with
`USE_CONTEXT_LINES` (3) either side (0.6); definitions whose name contains a
content word of the question (0.5). Sort by score, then by bytes, so a
tie goes to the cheaper read. Definition spans are re-anchored by
`code::current_span`; use spans were read from the file moments ago. Paths
are repo-relative, absolute with `--abs`, when run outside the root, and in
the MCP JSON (`read[].abs`). Every printed span goes to `served_spans`, so
a following read counts as guided. Keyword path only: under 20 ms.

### Per-session savings

The session-start hook passes Claude Code's `session_id` to
`memcastle session <id>`, stored in `meta` per project; every read and
search/links row carries it (schema v13). `status` prints observed
savings for this session and all time; the status line shows both.
