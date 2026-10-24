<img src="docs/logo/mark.svg" width="56" align="left" alt="">

# memorycastle

Shared memory for coding agents. One 6 MB Rust binary, one SQLite file, no LLM, no key, no server.

**Smaller, honest, and it knows when to say nothing.** Measured on
2026-10-19 against Mem0 2.2.1 and cognee 1.6.5, the two
best-known open-source agent memories, each without a key and with a paid
OpenAI configuration, on three real journals: memorycastle wins 12 of 17
rows with no key, no competitor more than 3, and ranks first on all three journals. It is the only one that says
"no relevant note" when there is none and the only one that labels and
demotes a superseded decision; it indexes an edited note in place in
0.19 s, answers keyword questions in 2.5 ms from a 9 MB process, and ran
four agent tasks on 50% fewer tokens than cognee and 61% fewer than Mem0.
Mem0's bare vector mode, a 33M-parameter model over raw chunks, ingests
and queries faster and ranks as well; it has no floor, no supersession
and keeps old copies on every edit.

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

## For the agent

`memcastle guide` prints [docs/GUIDE.md](docs/GUIDE.md): when to search,
what to type, how to read the answer's grammar, when and how to write a
note. It is written as instructions an LLM can follow literally, and it is
the file to paste into any agent that has no plugin or instruction file.

## Uninstall

```sh
memcastle uninstall            # memory sections, MCP entries, Claude plugin and status line
memcastle uninstall --purge    # also the store, models and runtime under ~/.local/share/memorycastle
```

`uninstall` reverses `install` for every agent it detects (or the ones you
name) and, inside a repo, removes the section from that repo's
`CLAUDE.md`/`AGENTS.md`/`GEMINI.md`. Your journal notes stay in the repo;
they are plain files you wrote. It ends by printing the one command it
cannot run, removing its own binary. Restart the agents.

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

The rules in `CLAUDE.md` say when to write one, and the hooks ask at the
three moments a note is due: before the context is compacted (the why is
about to be summarised away), after a `git commit` (a task just closed),
and when the agent stops. Before compaction and at stop the ask is a hold,
once: the agent gets the reason and writes the note while the facts are
still in context; the second attempt passes, so nothing can wedge. If code was edited since the last note, a draft
is written from what was edited; the agent fills in the decision. `save` fills a missing author and date, warns when
the note skips a file the session touched, and flags a note that holds
several topics or duplicates another.

**2. The store indexes them.** Each note is split by heading into chunks.
Every chunk goes into a full-text index and, once embedded by the local
model, a 512-dimension vector in the same SQLite file. The header lines
agents already write (`Files:`, `Supersedes:`, backticked names) become
graph edges, so "what do we know about this file" needs no text match. A
newer note that supersedes an older one is declared, never edited; the old
note stays searchable and labelled. The checkout's own code is indexed too, by
scanning lines with no parser: definitions with their spans, imports and
call edges, for Rust, Kotlin, Java, Python, TypeScript and JavaScript, Go,
Swift, Ruby, PHP, C# and C/C++. C is read by its own grammar (a C function
has no keyword before its name): on six real C and C++ trees it finds 98%
of what `ctags` finds; Go, Java, TypeScript and Python were checked against
their own parsers and find 99% to 100% of functions and types.

**3. An agent asks.** Search runs keyword and meaning retrieval, fuses them,
reranks by the graph, and drops anything off topic rather than returning
the nearest unrelated page. The answer is shaped for the agent's budget:

```
$ memcastle search "did we try batching embeddings"
1. memorycastle/journal/2026-10-18-one-chunk-per-call.md:13   0.77   Rejected
   decision   Embed one chunk per model call, longest first; batching bought no throughput on the CPU.
read first (2 spans, 1.1 KB; sed -n 'a,bp' path):
   a. src/store.rs:161-232   embed_missing   1.00  named in question
   b. src/limits.rs:130-132  EMBED_BATCH     0.90  named by note #1
```

The text has a fixed grammar, so a human and a parser read the same bytes:
a hit line is `n. path:line   confidence   section`, the next line is the
title and author, every field after that is `   label      value` with
continuations indented to the value column, a blank line ends the hit, and
the read table rows are lettered so they never look like hits. `--json`
gives the same answer with every field typed, including the ranking trace.

The default is one line per hit (confidence, note, line, section) and the
note's first Decision sentence, about 50 tokens: enough to pick a note.
`--sections` prints that note's Decision, Rejected and Measured; `--full`
the older title-and-snippet block. Every answer ends with the code
spans to read, re-found by their anchor line after edits, so the agent
reads 60 lines instead of a 110 KB file.

The agent can pass a second wording with `--also` ("the same question in
the repo's own words"), which found the right note in the top 3 for 6 more
of 187 test questions. And the index learns from use, with no model
retrained: when an agent searches and then reads a note (or the code a note
covers) that the search did not rank first, the question is kept for that
note, so the next wording of it finds the note first. One search and one
read is enough; on 187 questions in new wordings the right note in the top 3
went from 163 to 173, and none was lost. `memcastle learned` lists what it
learned and `learned forget` drops one.

**4. Hooks keep it honest.** In Claude Code, Codex, Gemini CLI and Cursor
(`install` wires each one; Codex asks you to trust them once with `/hooks`),
a hook recalls on every
prompt: when the journal is confident about the topic, one line per note
(about 15 tokens, each note once per session) and nothing otherwise, so the
session that would not have searched still meets the note that rejected
its approach. Another shows what the journal says about the function being
edited, before it is edited; one blocks a whole-file read once when a
lookup already printed the range; one logs every read silently. A note
that names a function whose body changed since the note was written is
labelled `code changed since` and ranked under one whose code still
matches: memory that says "check me" instead of being trusted blindly.
The note itself is asked for at the moments it is due, without the user:
once mid-session when five files were edited or thirty minutes passed
since the last note, after a `git commit`, before compaction and at stop
(the last two hold once). Copilot, OpenCode and Antigravity have no hook
surface; they get the instruction text and the tools. From that
ledger `status` reports savings two ways: the controlled A/B on fixed
questions, and the observed figure (bytes of files a printed span stood in
for, minus what was read and the lookup output; an upper bound, so the
status line writes it with a tilde). Codex transcripts are imported after
the fact. Codex, Gemini CLI and Cursor have no status line setting of their
own; `memcastle status --line` prints the same short line for any bar, for
example tmux: `set -g status-right '#(cd #{pane_current_path} && memcastle status --line)'`.

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
The A/B is the figure to quote; the observed one is a bound you can
inspect read by read.

## Commands you will actually type

```sh
memcastle search "<question>" [--sections | --full] [--related]
memcastle links <symbol>                 # defined at path:a-b, called by, calls, and the notes about it; works with no notes
memcastle links <file> [--at <line>]     # notes about the file, or the function holding the line; with none, what it defines and who uses it
memcastle status                          # counts, savings, notes about files that no longer exist
memcastle quality --flagged               # notes the store can see are weak
memcastle save memorycastle/journal/<note>.md
memcastle init --into CLAUDE.md          # the memory section into one file, plus a one-line lead under its first heading
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
measure.

## License

MIT OR Apache-2.0. The embedding model is used under Google's
[Gemma Terms of Use](https://ai.google.dev/gemma/terms).
