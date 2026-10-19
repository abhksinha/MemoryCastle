# memorycastle

Shared memory for coding agents. One binary, one SQLite file, no LLM, no server.

Claude Code, Codex, Gemini CLI, Antigravity, Cursor, GitHub Copilot and
OpenCode forget everything between sessions. memorycastle gives them one memory per repository: what was
decided, what was tried and rejected, what was measured, and where in the
code it lives. The next agent asks a question and gets the right paragraph
and the exact lines to read, in milliseconds, instead of re-reading the
journal or repeating the experiment.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/abhksinha/MemoryCastle/main/scripts/get.sh | sh
memcastle install
```

That is all, from any directory. `install` finds the agents on your
machine and wires each one: the tool server in its config, the Claude Code
plugin and status line, and the memory rules in the user-level file every
agent reads in every repo (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`,
`~/.gemini/GEMINI.md`, `~/.config/opencode/AGENTS.md`; Cursor, Copilot and
Antigravity keep user rules in the app and read the repo's `AGENTS.md`). From then on memory works in whatever repo you
open. Run it inside a repo to add the same section to that repo's own
`CLAUDE.md`/`AGENTS.md`/`GEMINI.md` for teammates, build its code index,
import your past transcripts into the savings ledger and start the
embedding server. Run it again any time; it changes only what is missing.
Restart the agents.

### Prerequisites

- **Binaries** (macOS arm64, Linux x86_64): `curl`, `tar`, `shasum`. Nothing
  else; the binary is static apart from libc.
- **Building from source** (any other platform, or `cargo install --path .`):
  Rust 1.85 or newer from [rustup.rs](https://rustup.rs), a C compiler for
  the bundled SQLite (Xcode command line tools on macOS, `build-essential`
  on Debian and Ubuntu). No other system libraries.
- **Meaning search**: downloaded on first use with pinned SHA-256 checksums,
  about 390 MB in all: ONNX Runtime 1.31 (43 MB), EmbeddingGemma 2 int8
  (314 MB, from `onnx-community/embeddinggemma-2-ONNX`) and its tokenizer
  (32 MB). Keyword search never needs any of it. Air-gapped machines place
  the same files under the data directory and set `MEMORYCASTLE_ORT_DYLIB`.
- **Status line and hooks** (Claude Code): `jq` and `python3`, both present
  on macOS and most Linux installs.

### Where things go

| What | macOS and Linux |
| --- | --- |
| Binaries `memorycastle`, `memcastle` | `~/.local/bin/` (installer), `~/.cargo/bin/` (cargo build); `MEMORYCASTLE_BIN` overrides |
| Store, model, runtime, your gold sets | `~/.local/share/memorycastle/` (`memory.db`, `models/`, `runtime/`); `MEMORYCASTLE_HOME` overrides |
| Notes and the derived graph | `memorycastle/journal/`, `memorycastle/graph/` in each repo, under git |
| Agent config written by `install` | `~/.codex/config.toml`, `~/.gemini/settings.json`, `~/.gemini/antigravity/mcp_config.json`, `~/.cursor/mcp.json`, `~/.copilot/mcp-config.json`, `~/.config/opencode/opencode.json`, `~/.claude/statusline.sh` and the plugin under `~/.claude/plugins/` |
| Embedding server socket | the system temp dir, `memorycastle-<hash>.sock`; exits after 30 minutes idle |

Nothing is written outside those paths. Uninstall: delete the two binaries and
`~/.local/share/memorycastle/`; the notes stay in your repos as plain Markdown.

## How it works

**1. Agents write notes.** At the end of a task the agent saves a short
Markdown file in `memorycastle/journal/`, under git with the code:

```markdown
# Choose int8 over q4 for EmbeddingGemma 2
Author: abhi+claude | Date: 2026-10-18
Files: src/embed/download.rs, src/embed/mod.rs

## Decision
int8 stays the default: it is the only setting that finds every answer in the top 3.
## Rejected
q4 is twice as fast and 120 MB lighter but drops one question.
## Measured
int8 recall@3 1.000, p50 124 ms. q4 0.958, 58 ms.
```

The rules in `CLAUDE.md` say when to write one. If a session edits code and
saves nothing, the stop hook drafts the note from what was edited; the agent
fills in the decision. `save` fills a missing author and date, warns when
the note skips a file the session touched, and flags a note that holds
several topics or duplicates another.

**2. The store indexes them.** Each note is split by heading into chunks.
Every chunk goes into a full-text index and, once embedded by the local
model, a 512-dimension vector in the same SQLite file. The header lines
agents already write (`Files:`, `Supersedes:`, backticked names) become
graph edges, so "what do we know about this file" needs no text match. A
newer note that supersedes an older one is declared, never edited; the old
note stays searchable and labelled.

**3. An agent asks.** Search runs keyword and meaning retrieval, fuses them,
reranks by the graph, and drops anything off topic rather than returning
the nearest unrelated page. The answer is shaped for the agent's budget:

```
$ memcastle search "did we try batching embeddings" --brief
1. 0.77  memorycastle/journal/2026-10-18-one-chunk-per-call.md:13 > Rejected
read first (2 spans, 1.1 KB in all; sed -n 'a,bp' path):
   1. src/store.rs:161-232   embed_missing   1.00  named in question
   2. src/limits.rs:130-132  EMBED_BATCH     0.90  named by note #1
```

One line per hit: confidence, note, line, section. `--sections` prints the
note's Decision, Rejected and Measured. Every answer ends with the code
spans to read, re-found by their anchor line after edits, so the agent
reads 60 lines instead of a 110 KB file.

**4. Hooks keep it honest.** In Claude Code, a hook shows what the journal
says about a file before it is edited, blocks a whole-file read once when a
lookup already printed the range, and logs every read silently. From that
ledger `status` reports savings two ways: the controlled A/B on fixed
questions, and the observed figure (bytes of files a printed span stood in
for, minus what was read and the lookup output; an upper bound, so the
status line writes it with a tilde). Codex transcripts are imported after
the fact.

## What it costs

| Path | Latency | Memory |
| --- | --- | --- |
| Keyword search | under 1 ms | 5 MB |
| Meaning search, server warm | 90 ms | 5 MB client, 260 MB server |
| Meaning search, cold | 230 ms | 274 MB, released when idle |
| 700 notes, 3,619 chunks: search | 3 ms scan | 19 MB on disk |

## What it is measured against

Two private gold sets, 97 questions over real journals: recall@3 1.000 and
0.962, against 0.958 for grep with MRR 0.73 versus 0.92. A controlled A/B
with Codex on a real project: 32 to 53% fewer tokens for the whole run.
Method and raw numbers in [docs/BENCHMARKS.md](docs/BENCHMARKS.md); the
design in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). The A/B is the
figure to quote; the observed one is a bound you can inspect read by read.

## Commands you will actually type

```sh
memcastle search "<question>" [--brief | --sections] [--related]
memcastle links <file | symbol | note>   # notes about it, and where the code is now
memcastle status                          # counts, savings, notes about files that no longer exist
memcastle quality --flagged               # notes the store can see are weak
memcastle save memorycastle/journal/<note>.md
```

`memcastle` is the command; `memorycastle` is the same binary under the
project's name, for scripts that already use it. Everything else (`history`, `graph`, `eval`,
`import-log`, `forget`, `export`, `reindex`, `serve`, `mcp`) is in `--help`.

## Development

```sh
cargo test --release && cargo clippy --all-targets
MEMORYCASTLE_HOME=/tmp/mc memcastle search "q" --explain   # isolated store, per-stage timings
```

Environment: `MEMORYCASTLE_HOME` (data dir), `MEMORYCASTLE_MODEL` (`int8`,
`q4`, `q4f16`), `MEMORYCASTLE_NO_LEDGER` (for benchmark loops),
`MEMORYCASTLE_ORT_DYLIB` (an existing ONNX Runtime; the air-gapped path).
The engineering rule applied throughout: do less, allocate less, move less,
measure. See `Resource_Conscious_Software_Engineering.md`.

## License

MIT OR Apache-2.0. The embedding model is used under Google's
[Gemma Terms of Use](https://ai.google.dev/gemma/terms).
