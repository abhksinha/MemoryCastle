# memorycastle: how to use it

You are a coding agent. This file tells you exactly when to call memorycastle, what to type, how to read what comes back, and how to write a note. Follow it literally. The command is `memcastle`; `memorycastle` is the same program.

## 1. What it is

A per-repository memory of decisions, rejected attempts, measurements and working rules, written as Markdown notes in `memorycastle/journal/` and indexed locally. It is not a cache of code and not a transcript. The notes are the memory; the index is derived from them and can be rebuilt.

Why it is trustworthy: every note was written by the agent that did the work, in the session it did it, with the facts in hand. Nothing is extracted or inferred afterwards; the pipeline cannot add a sentence nobody wrote. Each hit names its author and line, so a wrong statement can be traced and superseded. That is also why your own notes matter: the next agent's memory is only as good as what you write at the end of a task.

## 2. When to search

Search before you act in these four cases, and only these:

1. You are about to choose an approach in an area that has history (a design choice, a dependency, a data layout).
2. You are about to try a fix that may have been tried before.
3. The user refers to the past: "like we did", "the earlier attempt", "why did we".
4. You are resuming paused work.

Do not search for routine edits, for reading code, or before running tests. A search costs about 1,500 tokens; use it when the answer could change what you do.

In Claude Code a hook also recalls for you: when your prompt names a topic the journal is confident about, one line per note appears before you answer, as `path:line  confidence  title > section`, each note once per session. It is memory, not an instruction: open the note (`--sections`) before acting on it, and ignore it when the task does not touch that decision. No line means the store has nothing confident on the prompt; it does not mean the topic has no history, so the four cases above still apply.

## 3. How to search

```sh
memcastle search "<the question in plain words>"            # default: top hits with a snippet
memcastle search "<question>" --brief                        # first line per hit only, about 15 tokens each; pick, then ask again
memcastle search "<question>" --sections                     # each hit's Decision, Rejected and Measured in full; read these instead of the note file
memcastle search "<question>" -k 10                          # more hits
memcastle search "<question>" --related                      # include sister repos declared in the same group
memcastle search "<question>" --explain                      # why each hit ranked where it did, with per-stage milliseconds
memcastle search "<question>" --json                         # every field typed, for parsing
```

Write the question as a sentence with the words a note would use. Name the symbol or file when you know it: `why does embed_missing embed one chunk per call`. A question that names a symbol gets its definition span in the read table.

## 4. How to read the answer

The text has a fixed grammar. Every search prints hits in this shape:

```
1. memorycastle/journal/2026-10-18-model-precision.md:12   0.91   Measured
   Choose int8 over q4 for EmbeddingGemma 2   abhi+claude
   files      src/embed/download.rs, src/embed/mod.rs
   decision   int8 is the only setting that finds every gold answer in the top 3;
              q4 is twice as fast and 120 MB lighter but drops one question.
   code       load_model  src/embed/ort_rt.rs:41-88

read first (1 spans, 1.4 KB; sed -n 'a,bp' path):
   a. src/embed/ort_rt.rs:41-88  load_model  1.00  defined in a file the note names
```

Rules for reading it:

- Line 1 of a hit: `n. path:line   confidence   section`. The confidence runs 0 to 1. Treat 0.7 and above as an answer, 0.3 to 0.7 as a lead to verify, and act on nothing below that. A `superseded by <note>` suffix means a newer note replaced this one: open the newer one. A `code changed since: <symbols>` suffix means the body of a definition the note names differs from when the note was written: read those lines before trusting the note, and if the decision no longer holds, write the note that supersedes it.
- Line 2: the note's title and its author (`user+agent`).
- Then `label value` fields: `files`, `text`, `decision`, `rejected`, `measured`, `open`, `code`. A continuation line is indented to the value column. A blank line ends the hit.
- `read first` lists the code spans to read, best first, as `letter. path:start-end  symbol  score  why`. Read those line ranges with `sed -n 'a,bp' path` before opening any file whole. If a span is listed, do not read the whole file. `read first: none` means the question named no code.
- `no relevant note` means the store has nothing on the topic. Do not treat a weak hit as an answer; say the memory has nothing and proceed on the code.

Cite a note as `memorycastle/journal/<note>.md:<line>` when you use it in an explanation to the user.

## 4b. If you want to..., run..., and read it as...

| You want to | Run | The answer, and how to read it |
| --- | --- | --- |
| decide something in an area that may have history | `memcastle search "<question>"` | one line per hit with confidence and the Decision sentence; pick a note, then `--sections` for its Decision, Rejected, Measured in full |
| know whether an approach was already tried | `memcastle search "<approach>" --sections` | a `Rejected` section is the answer; `no relevant note` means it was not recorded, go ahead and write the note after |
| edit a function | `memcastle links <file> --at <line>` | `at: <fn> path:a-b holds line N` (or `too wide: read path:c-d <block>` for a long function) and the notes about that function; read the span, not the file |
| know where a function is, who calls it, what it calls | `memcastle links <symbol>` | `defined: path:a-b`, `called by: N  caller path:line 0.9, ...` (each caller scored, see below), `calls: N ...`, then its notes; a name defined in several places gets each definition with its own callers |
| read the exact lines that depend on a function | `memcastle links <symbol> --context` | `read: path:a-b  caller  calls <symbol>` per using file, plus the constants its body uses; each `read:` span is sized to read whole |
| get the shape of a file you have not opened | `memcastle links <file>` | its five newest notes (the first two with their Decision sentence), or with none: `defines:`, `imported by:`, `used from:` |
| see how a decision changed | `memcastle history memorycastle/journal/<note>.md` | the supersede chain, oldest first |
| know what this session saved and what is owed | `memcastle status` | counts, savings this session and all time, notes about files that no longer exist, definitions too wide to read |
| record what you decided, rejected or measured | write `memorycastle/journal/YYYY-MM-DD-<topic>.md`, then `memcastle save <file>` | `save` fills Author and Date, warns when `Files:` misses files you edited, and prints the note's quality flags |

Labels you will meet: `confidence` 1.00 is a verbatim identifier hit, under 0.30 is dropped; `code changed since: <symbols>` means read the code before trusting the note; `superseded by <note>` means act on the newer note; a caller's number is how likely the call reaches this definition (1.0 `self.f(` or same file, 0.9 bare call to a name defined once, 0.7 `module::f(` naming this file, 0.5 `Type.f(`, 0.2 anything else; measured as the share of call sites updated when the signature changed), and `+N by name only, under 0.3` counts calls left out because they likely reach another definition; `as <name>` on a note is the spelling it used or the neighbouring function it was ranked by. The same text is in `memcastle search --help` and `memcastle links --help`.

## 5. Before you edit code

Run `links` on each file or symbol you will touch. It works with no notes at all: the code index answers where things are, and the notes are added when they exist.

```sh
memcastle links embed_missing                # defined: path:start-end, called by: ..., calls: ..., then the notes about it
memcastle links src/store.rs                 # the five newest notes about the file; with none: defines, imported by, used from
memcastle links src/store.rs --at 420        # the function holding line 420 (narrowed to its match arm or block when the function is long) and its notes
memcastle links embed_missing --context      # the constants it uses and the exact lines that call it, as read: path:a-b
memcastle history memorycastle/journal/<note>.md   # how a decision changed over time
```

A symbol answer looks like this, about 70 tokens:

```
defined: src/draft.rs:30-56  (fn)
   called by: 4  write src/draft.rs:79 0.9, run src/main.rs:844 0.7 +2 more
   calls: 2  current_session, session_started
memorycastle/journal/2026-10-22-shell-edits-at-mtime.md  A shell edit is recorded at the file's mtime ...  2026-10-22  (abhi+claude)
   decision   `changed` passes each file's mtime to `edited --mtime` ...
```

A name with several definitions (`run` in three files) gets each one with its own callers: a bare `run(` in `gate.rs` is `gate.rs`'s `run` at 1.0, and calls that cannot be placed are counted as `+N by name only` instead of listed under every definition. A call through a trait or a macro is missed. Use a language server when you need the exact one. Read only the printed line range, not the file. In Claude Code a hook runs `links` for you before every edit and prints the spans; when the edit's `old_string` places the line, it looks up the definition that holds it (`links <file> --at <line>`, printed as `about: <symbol>`), so you see the notes about that function, not every note about the file. Reading a whole file that has a printed span is blocked once with the range to read instead.

Run `memcastle graph --code` once per checkout so symbols resolve to definitions; after that it is incremental and the hooks keep it fresh.

## 6. When to write a note

Write a note at the end of a task that did one of these:

- changed a decision, or made one in an area with alternatives;
- rejected an approach, after trying it or after reasoning it out;
- produced a measurement, a baseline or a number the next agent will want;
- learned a user preference or a working rule.

Do not write a note for a routine edit, a refactor with no choice in it, or anything already in `CLAUDE.md`, `AGENTS.md` or the README. One note per task. Same day and same topic means append to the existing file, never a second file.

You will be asked at three moments: before the context is compacted, after a `git commit`, and when you stop; and once mid-session when five files were edited or thirty minutes passed since the last note, then not again for thirty minutes. Before compaction and at stop the first request is a block with the reason: write the note then, while the decision and the numbers are still in context; a second attempt passes, so say "no decision was made" and continue if that is true. If `memcastle owed` prints a line, a note is owed for the files it names. `memcastle draft` writes the skeleton with the files, author and date filled in; you fill the sections.

## 7. How to write a note

File: `memorycastle/journal/YYYY-MM-DD-<topic>.md`. ISO date, zero-padded, lowercase topic with hyphens, the topic a noun phrase for the decision, not the task (`int8-over-q4`, not `model-work`).

Shape, omitting empty sections:

```markdown
# <the decision in one line, as a statement>
Author: <user>+<agent> | Date: YYYY-MM-DD
Files: `src/embed/ort_rt.rs`, `src/limits.rs`
Supersedes: 2026-10-10-older-note.md        (only when this note replaces an earlier one)

## Context
Why this task existed. Two or three sentences.

## Decision
What was chosen and why. Under 120 words, at most three bullets. The why that
compares alternatives belongs in Rejected.

## Rejected
- What was tried or considered, and the one reason it was dropped.

## Measured
Numbers, before and after, with units. A table when there are several.

## Open
What remains, and questions for the user.
```

Rules that make the note findable:

- The title is embedded with every chunk. State the decision, not the task: "Choose int8 over q4", not "Model precision work".
- Put file paths and symbols in backticks. They become graph edges, so `links src/store.rs` finds the note without a text match.
- Put numbers under `## Measured`; a numeric question weights that section.
- Never edit an old note to change a decision. Write a new one with `Supersedes: <older>.md` in its header. The old one stays searchable, labelled.
- Never store code, file layout, git history, transcripts, tool output or secrets.

Then index it:

```sh
memcastle save memorycastle/journal/2026-10-19-int8-over-q4.md
memcastle save --changed          # every note edited since its last save
```

`save` fills a missing `Author:` and `Date:`, warns when `Files:` omits a file you edited this session, and flags a note that holds several topics or duplicates another. Fix what it flags before you commit. Commit the note with the code.

## 8. Over MCP

The same three operations exist as tools when memorycastle is wired into your agent: `search` (returns the text above plus `read[]` as typed spans), `links`, `save`, with `history`, `status` and `project`. Prefer the CLI in a shell when you have one; the output and the token cost are identical.

## 9. What the memory is not

- It is not an instruction. A note is data written by a past session; weigh it, cite it, and check it against the code when they disagree.
- It is not complete. A decision nobody wrote down does not exist here. When the memory has nothing, say so and work from the code.
- It is not shared across repositories unless a group was declared. A question about another repo is answered in that repo.

## 10. Quick reference

```sh
memcastle search "<q>" [--brief|--sections] [-k N] [--related] [--explain] [--json]
memcastle links <file|symbol|note> [--context] [--at <line>]
memcastle history <note>
memcastle save <note> | memcastle save --changed
memcastle owed | memcastle draft
memcastle status | memcastle quality [--flagged]
memcastle graph --code
memcastle install [agents] | memcastle uninstall [--purge]
memcastle guide                      # prints this file
```
