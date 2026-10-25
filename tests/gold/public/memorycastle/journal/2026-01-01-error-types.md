# Errors are one enum with a kind and a source
Author: fixture+claude | Date: 2026-01-26
Files: `src/error.rs`

## Decision
`Error` is an enum (`Io`, `Corruption`, `NotFound`, `Busy`) wrapping the source error; callers match on the kind.

## Rejected
- Boxed dyn errors everywhere: callers could not tell corruption from a missing key.
