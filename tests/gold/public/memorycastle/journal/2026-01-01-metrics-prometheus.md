# Metrics are exported in Prometheus text format
Author: fixture+claude | Date: 2026-01-02
Files: `src/metrics.rs`

## Decision
`/metrics` serves Prometheus text: counters for reads, writes, compactions, and histograms for latency.

## Rejected
- StatsD push: needs an agent on every host. OpenTelemetry SDK: 2 MB more binary for the same counters.

## Measured
- Scrape takes 0.4 ms with 120 series.
