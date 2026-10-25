# Table files are opened through a bounded handle cache
Author: fixture+claude | Date: 2026-01-25
Files: `src/files.rs`

## Decision
`TableHandles` keeps at most 500 open files and closes the least recently used, because the default ulimit is 1,024.

## Rejected
- Opening every table at start: crashed with 'too many open files' at 1,100 tables.
