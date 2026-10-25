# Benchmarks use YCSB workloads A to F
Author: fixture+claude | Date: 2026-01-19
Files: `bench/ycsb.rs`

## Decision
The benchmark harness runs YCSB workloads A to F against a fresh store; results are recorded per release.

## Rejected
- Hand-written microbenchmarks only: they missed the scan regression in 0.4.

## Measured
- Workload A: 112,000 operations per second; workload E (scans): 9,800.
