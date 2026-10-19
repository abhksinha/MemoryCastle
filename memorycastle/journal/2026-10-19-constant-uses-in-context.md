# links --context lists where a constant is used
Author: abhi+claude | Date: 2026-10-19
Files: src/spans.rs

## Context
Asked for a real use of the graph, I ran `links WHOLE_READ_LINES --context`
and got only the definition. The context rule ran from functions outward
(`is_call`: `name(`, `.name(`, `name::`), and a constant is never called,
so the question that matters for a limit, "where is it used", went
unanswered. This morning I had found the two uses by grep.

## Decision
For a `const` or `static` definition, `spans::context` lists the first
value use per mentioning file, the defining file included (a limit is used
where it is set): `spans::is_identifier_use` matches the identifier with
boundaries on both sides, outside strings and comments, skipping import
lines and the definition line. Printed as `uses NAME` beside the function
case's `calls NAME`. Version 0.10.7.

## Rejected
- Every use line, not the first per file: `CONTEXT_ITEMS` bounds the
  output, and the agent greps within a file it has been pointed at.

## Measured
`WHOLE_READ_LINES`: definition plus its use in `src/store.rs`.
`SPAN_FOLLOW_SECS`: definition, two notes, and uses in `src/store.rs` and
`src/quality.rs`; the `use crate::limits::{…}` import line no longer counts.
