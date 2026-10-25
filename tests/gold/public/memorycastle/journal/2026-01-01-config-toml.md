# Configuration is one TOML file with defaults in code
Author: fixture+claude | Date: 2026-01-24
Files: `src/config.rs`

## Decision
`Config::load` reads `tidepool.toml`; every field has a default in `Config::default`, so an empty file is valid.

## Rejected
- YAML: indentation errors reported far from their cause. Environment variables only: no place to comment why a value was chosen.
