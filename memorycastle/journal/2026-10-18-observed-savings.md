# Observed savings from a silent read ledger, beside the estimate
Author: abhi+claude | Date: 2026-10-18
Files: plugin/hooks/after-read.py, src/store.rs, src/main.rs, src/db.rs, src/limits.rs

## Context
The estimate assumed every file a span pointed into would otherwise be read whole. True for notes and most code reads in the logs, not all; `links` rows overstated. The user asked for a number that measures rather than guesses, at no token cost.

## Decision
A `PostToolUse` hook on Read and Bash parses the tool input and response and calls `memorycastle read <path> --bytes N`, printing nothing. Schema v11 adds `served_spans` (every path a search or links printed a span for) and `reads` (path, bytes returned, whole size, guided flag). A read is guided when a span for that file was printed within `SPAN_FOLLOW_SECS` (600). `status` prints `observed` (saving over guided reads, exact on bytes read, one assumption: whole file otherwise) above `estimated` (the old counter, renamed). Status line prefers observed.

## Rejected
Hooking Grep and Glob: their output is lines, not file bytes, and they are what memorycastle replaces, not what it guides. Counting unguided range reads as savings: no evidence memorycastle caused them. Putting the hook body in a shell heredoc: the heredoc took stdin and the hook JSON never arrived; the body lives in `after-read.py`.

## Measured
Hook run 78 ms end to end including python3 start; memorycastle part under 1 ms. First replay: a guided `sed -n` of 23 lines returned 948 bytes of a 27,221-byte file (97% saved); a whole Read of `src/limits.rs` recorded as unguided, saving zero.

## Open
Only Claude Code reports reads. `tail -n` and `head -c` on files the agent later edits still count as reads, which is right. A read of the same range twice counts twice, also right: both cost tokens.

## Decision (same day, later)
Observed is reported as a range, not a point. Floor: the agent would have grepped and read a 150-line range (`RANGE_READ_LINES`) at the file's average line length. Ceiling: the whole file. Asked by the user after I admitted I read whole files often but not always. Status line shows `1k-8k (90-98%) tokens saved`.
