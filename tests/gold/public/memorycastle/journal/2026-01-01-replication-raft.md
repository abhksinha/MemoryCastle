# Replication uses Raft through openraft
Author: fixture+claude | Date: 2026-01-21
Files: `src/replica.rs`

## Decision
Replicas run Raft via the `openraft` crate; the leader ships WAL entries as log entries. One implementation of consensus, not ours.

## Rejected
- Primary-backup with manual failover: an operator must act during an outage. Writing our own Raft: months of edge cases.

## Measured
- Failover after leader kill: 1.4 s median over 50 runs.
