# The table footer carries a format version
Author: fixture+claude | Date: 2026-01-27
Files: `src/table.rs`

## Decision
The footer's last byte is `FORMAT_VERSION`; a reader refuses tables newer than it knows instead of misreading them.

## Rejected
- Guessing the format from the file size: broke on the first change.
