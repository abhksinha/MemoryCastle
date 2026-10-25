# Windows is not supported
Author: fixture+claude | Date: 2026-01-19
Files: `README.md`

## Decision
Tidepool runs on Linux and macOS. Windows file locking and rename semantics break the atomic manifest replace.

## Rejected
- Emulating rename with copy and delete: not atomic, so a crash can lose the manifest.
