# The network server speaks gRPC
Author: fixture+claude | Date: 2026-01-13
Files: `src/server.rs`

## Decision
`tidepool serve` exposes gRPC (tonic) with streaming scans; clients in any language get generated stubs.

## Rejected
- A custom TCP protocol: every client library written by hand. REST: no streaming scans.

## Measured
- Single-key get over loopback: 85 microseconds p50.
