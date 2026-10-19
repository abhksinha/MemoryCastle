//! The gold-set harness: recall@k, MRR, latency percentiles and peak RSS for
//! any [`Memory`]. Built first, because it is what turns "better" into a number.

use crate::project::{self, Scope, ScopeFlags};
use crate::search::{Memory, Mode, SearchOptions};
use crate::{err, sys, Result};
use rusqlite::Connection;
use std::time::Instant;

#[derive(Debug, serde::Deserialize)]
pub struct GoldQuestion {
    pub project: String,
    pub question: String,
    /// Note path that answers it, relative to its repo root.
    pub answer: String,
    #[serde(default)]
    pub related: bool,
}

#[derive(Debug, serde::Deserialize)]
pub struct GoldSet {
    pub questions: Vec<GoldQuestion>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize, Clone)]
pub struct Report {
    pub label: String,
    pub questions: usize,
    pub skipped: usize,
    pub recall_at_3: f64,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub first_ms: f64,
    pub peak_rss_mb: f64,
    pub at: i64,
    /// Questions whose answer was not in the top 3: (question, expected, top paths).
    pub misses: Vec<(String, String, Vec<String>)>,
    /// Model-ranked samples: (questions with a right-note cosine, with a best wrong-note cosine).
    pub cosine_samples: (usize, usize),
    /// Quartiles and the 90th percentile of those cosines: right p25/p50/p75,
    /// best-wrong p50/p75/p90. Measured 2026-10-19 on both gold sets: right
    /// median 0.75, best-wrong p90 0.76 to 0.79. The raw cosine does not
    /// separate the right note from a related one; fusion, the graph and the
    /// near-miss gate do. So no per-store rescale is derived from this: the
    /// default floor and ceiling measure "on topic at all", which is what
    /// confidence means.
    pub cosine_quantiles: Vec<f32>,
}

pub fn load_gold(path: &std::path::Path) -> Result<GoldSet> {
    let text = std::fs::read_to_string(path).map_err(|e| err!("{}: {e}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

/// Runs every question through `memory` with k = 10. A question whose
/// project is unknown is skipped and counted.
pub fn run(conn: &Connection, gold: &GoldSet, memory: &mut dyn Memory, mode: Mode, label: &str) -> Result<Report> {
    let opts = SearchOptions { k: 10, mode };
    let mut ranks: Vec<Option<usize>> = Vec::with_capacity(gold.questions.len());
    let mut misses: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut times: Vec<f64> = Vec::with_capacity(gold.questions.len());
    let mut skipped = 0;
    // Per question, the right note's best cosine and the best cosine of any
    // other note returned: evidence for what confidence can and cannot mean.
    let mut right: Vec<f32> = Vec::new();
    let mut wrong: Vec<f32> = Vec::new();
    for q in &gold.questions {
        let Some(p) = project::by_name(conn, &q.project)? else {
            skipped += 1;
            continue;
        };
        let flags = ScopeFlags { related: q.related, ..Default::default() };
        let scope: Scope = project::resolve_scope(conn, Some(&p), &flags)?;
        let t0 = Instant::now();
        let hits = memory.search(&scope, &q.question, &opts)?;
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
        if let Some(c) = hits.iter().filter(|h| h.path == q.answer).filter_map(|h| h.trace.cosine).reduce(f32::max) {
            right.push(c);
        }
        if let Some(c) = hits.iter().filter(|h| h.path != q.answer).filter_map(|h| h.trace.cosine).reduce(f32::max) {
            wrong.push(c);
        }
        // Rank of the first hit from the answer note (notes, not chunks, are judged).
        // An empty answer means "no note covers this": correct when nothing comes back.
        let mut seen: Vec<&str> = Vec::new();
        let mut rank = None;
        if q.answer.is_empty() {
            ranks.push(if hits.is_empty() { Some(1) } else { None });
            if !hits.is_empty() {
                misses.push((q.question.clone(), String::new(), hits.iter().take(3).map(|h| format!("{} {:.2}", h.path, h.trace.confidence)).collect()));
            }
            continue;
        }
        for h in &hits {
            if !seen.contains(&h.path.as_str()) {
                seen.push(&h.path);
                if h.path == q.answer {
                    rank = Some(seen.len());
                    break;
                }
            }
        }
        if rank.is_none_or(|r| r > 3) {
            misses.push((q.question.clone(), q.answer.clone(), hits.iter().take(3).map(|h| format!("{} {:.2}", h.path, h.trace.confidence)).collect()));
        }
        ranks.push(rank);
    }
    let n = ranks.len().max(1) as f64;
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    let pct = |p: f64| -> f64 {
        if sorted.is_empty() {
            return 0.0;
        }
        let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
        sorted[i]
    };
    Ok(Report {
        cosine_samples: (right.len(), wrong.len()),
        cosine_quantiles: quantiles(&right, &[0.25, 0.5, 0.75]).into_iter().chain(quantiles(&wrong, &[0.5, 0.75, 0.9])).collect(),
        misses,
        label: label.into(),
        questions: ranks.len(),
        skipped,
        recall_at_3: ranks.iter().filter(|r| r.is_some_and(|r| r <= 3)).count() as f64 / n,
        recall_at_10: ranks.iter().filter(|r| r.is_some_and(|r| r <= 10)).count() as f64 / n,
        mrr: ranks.iter().map(|r| r.map(|r| 1.0 / r as f64).unwrap_or(0.0)).sum::<f64>() / n,
        p50_ms: pct(0.5),
        p95_ms: pct(0.95),
        first_ms: times.first().copied().unwrap_or(0.0),
        peak_rss_mb: sys::peak_rss_bytes() as f64 / 1_048_576.0,
        at: sys::now(),
    })
}

fn quantiles(v: &[f32], ps: &[f64]) -> Vec<f32> {
    if v.is_empty() {
        return Vec::new();
    }
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    ps.iter().map(|p| s[((s.len() as f64 - 1.0) * p).round() as usize]).collect()
}

/// The gates from the design: fails when recall@10 drops more than one point
/// against `previous`, when keyword p50 is over 10 ms, or when peak RSS on a
/// keyword run is over 30 MB.
pub fn gate(report: &Report, previous: Option<&Report>, keyword_run: bool) -> Vec<String> {
    let mut fails = Vec::new();
    if let Some(prev) = previous {
        if report.recall_at_10 < prev.recall_at_10 - 0.01 {
            fails.push(format!("recall@10 fell from {:.3} to {:.3}", prev.recall_at_10, report.recall_at_10));
        }
    }
    if keyword_run {
        if report.p50_ms > 10.0 {
            fails.push(format!("keyword p50 {:.1} ms is over 10 ms", report.p50_ms));
        }
        if report.peak_rss_mb > 30.0 {
            fails.push(format!("keyword peak RSS {:.1} MB is over 30 MB", report.peak_rss_mb));
        }
    }
    fails
}
