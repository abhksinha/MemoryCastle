# Keys compare bytewise; no custom comparators
Author: fixture+claude | Date: 2026-01-23
Files: `src/codec.rs`

## Decision
Ordering is plain byte order (`compare_keys`). Users encode keys so byte order is the order they want.

## Rejected
- Pluggable comparators: a mismatched comparator on reopen silently corrupted ordering in a test.
