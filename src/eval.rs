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
    /// Files the session is taken to be working on, weight 1 each.
    #[serde(default)]
    pub working: Vec<String>,
    /// Other wordings searched with it (`search --also`), merged per note.
    #[serde(default)]
    pub also: Vec<String>,
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
    /// Questions with an empty answer (no note covers them), and how many of
    /// those got a hit anyway: a false answer.
    #[serde(default)]
    pub absent: usize,
    #[serde(default)]
    pub absent_answered: usize,
    /// Every question with the rank of its answer note (None: not in the
    /// top 10; for an absent question, Some(1) when nothing came back), so
    /// `eval --compare` can show which questions moved.
    #[serde(default)]
    pub ranks: Vec<(String, Option<usize>)>,
    /// Answerable questions: mean count of other notes among the first five
    /// notes returned (what a default `search` prints besides the answer).
    #[serde(default)]
    pub noise_at_5: f64,
    /// The policy's calibration on the notes returned: expected calibration
    /// error over ten bins of p (0 when the policy did not run), and how many
    /// notes it covers.
    #[serde(default)]
    pub policy_ece: f64,
    #[serde(default)]
    pub policy_notes: usize,
}

pub fn load_gold(path: &std::path::Path) -> Result<GoldSet> {
    let text = std::fs::read_to_string(path).map_err(|e| err!("{}: {e}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

/// Runs every question through `memory` with k = 10. A question whose
/// project is unknown is skipped and counted.
pub fn run(conn: &Connection, gold: &GoldSet, memory: &mut dyn Memory, mode: Mode, label: &str) -> Result<Report> {
    let opts = SearchOptions { k: 10, mode, working: Vec::new() };
    let mut ranks: Vec<Option<usize>> = Vec::with_capacity(gold.questions.len());
    let mut per: Vec<(String, Option<usize>)> = Vec::with_capacity(gold.questions.len());
    let (mut absent, mut absent_answered) = (0, 0);
    let mut misses: Vec<(String, String, Vec<String>)> = Vec::new();
    let mut times: Vec<f64> = Vec::with_capacity(gold.questions.len());
    let mut skipped = 0;
    let mut noise: Vec<usize> = Vec::new();
    let mut calib: Vec<(f32, bool)> = Vec::new();
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
        let opts = SearchOptions { working: q.working.iter().map(|f| (f.clone(), 1.0)).collect(), ..opts.clone() };
        let mut hits = memory.search(&scope, &q.question, &opts)?;
        if !q.also.is_empty() {
            let mut lists = vec![hits];
            for a in q.also.iter().take(crate::limits::SEARCH_ALSO_MAX) {
                lists.push(memory.search(&scope, a, &opts)?);
            }
            hits = crate::search::merge_queries(lists, opts.k);
        }
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
            absent += 1;
            absent_answered += usize::from(!hits.is_empty());
            ranks.push(if hits.is_empty() { Some(1) } else { None });
            per.push((format!("[{}] {}", q.project, q.question), *ranks.last().unwrap()));
            if !hits.is_empty() {
                misses.push((q.question.clone(), String::new(), hits.iter().take(3).map(|h| format!("{} {:.2}", h.path, h.trace.confidence)).collect()));
            }
            continue;
        }
        let mut noted: Vec<i64> = Vec::new();
        for h in &hits {
            if h.trace.policy > 0.0 && !noted.contains(&h.note_id) {
                noted.push(h.note_id);
                calib.push((h.trace.policy, h.path == q.answer));
            }
        }
        let mut first5: Vec<&str> = Vec::new();
        for h in &hits {
            if first5.len() < 5 && !first5.contains(&h.path.as_str()) {
                first5.push(&h.path);
            }
        }
        noise.push(first5.iter().filter(|p| **p != q.answer).count());
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
        per.push((format!("[{}] {}", q.project, q.question), rank));
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
        absent,
        absent_answered,
        ranks: per,
        noise_at_5: noise.iter().sum::<usize>() as f64 / noise.len().max(1) as f64,
        policy_ece: ece(&calib),
        policy_notes: calib.len(),
    })
}

/// Expected calibration error: per tenth of p, the gap between the mean p
/// and the share that was the answer, weighted by the bin's size.
fn ece(v: &[(f32, bool)]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut bins = [(0.0f64, 0.0f64, 0usize); 10];
    for &(p, y) in v {
        let b = ((p * 10.0) as usize).min(9);
        bins[b].0 += f64::from(p);
        bins[b].1 += f64::from(u8::from(y));
        bins[b].2 += 1;
    }
    bins.iter().filter(|b| b.2 > 0).map(|b| (b.0 / b.2 as f64 - b.1 / b.2 as f64).abs() * b.2 as f64).sum::<f64>() / v.len() as f64
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

/// Fails when recall@3 is under `min_recall` or more than `max_absent`
/// absent questions were answered: the retrieval gate CI runs on the public
/// gold set.
pub fn retrieval_gate(report: &Report, min_recall: Option<f64>, max_absent: Option<usize>) -> Vec<String> {
    let mut fails = Vec::new();
    if let Some(m) = min_recall {
        if report.recall_at_3 < m {
            fails.push(format!("recall@3 {:.3} under {m:.3}", report.recall_at_3));
        }
    }
    if let Some(m) = max_absent {
        if report.absent_answered > m {
            fails.push(format!("{} of {} absent questions answered, at most {m} allowed", report.absent_answered, report.absent));
        }
    }
    fails
}

/// Two reports of one gold set side by side: the totals, then every
/// question whose rank moved, worst first.
pub fn compare(base: &Report, new: &Report) -> String {
    let mut out = format!(
        "{:<12} {:>8} {:>8} {:>8} {:>8}\n{:<12} {:>8.3} {:>8.3} {:>8.3} {:>5}/{:<2}\n{:<12} {:>8.3} {:>8.3} {:>8.3} {:>5}/{:<2}\n",
        "", "r@3", "r@10", "MRR", "absent",
        "base", base.recall_at_3, base.recall_at_10, base.mrr, base.absent_answered, base.absent,
        "new", new.recall_at_3, new.recall_at_10, new.mrr, new.absent_answered, new.absent,
    );
    if base.ranks.is_empty() {
        out.push_str("base has no per-question ranks (built before 0.11.47): totals only\n");
        return out;
    }
    let before: std::collections::HashMap<&str, Option<usize>> = base.ranks.iter().map(|(q, r)| (q.as_str(), *r)).collect();
    let score = |r: Option<usize>| r.map(|r| 1.0 / r as f64).unwrap_or(0.0);
    let mut moved: Vec<(f64, String)> = new
        .ranks
        .iter()
        .filter_map(|(q, r)| {
            let b = *before.get(q.as_str())?;
            (b != *r).then(|| {
                let show = |r: Option<usize>| r.map(|r| r.to_string()).unwrap_or_else(|| "-".into());
                (score(*r) - score(b), format!("{:>3} -> {:<3} {q}", show(b), show(*r)))
            })
        })
        .collect();
    moved.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (lost, won) = (moved.iter().filter(|m| m.0 < 0.0).count(), moved.iter().filter(|m| m.0 > 0.0).count());
    out.push_str(&format!("{} questions moved: {won} up, {lost} down\n", moved.len()));
    for (_, l) in moved {
        out.push_str(&l);
        out.push('\n');
    }
    out
}

/// `eval --compare <ref>`: the same gold set through another build and this
/// one, each on its own copy of a snapshot of the store, since the live
/// store drifts as notes and learned questions arrive (Courier recall@3
/// moved 0.955 to 0.932 in minutes with no code change) and an older build
/// may not read a newer schema. `reference` is a binary path, or a git ref
/// built in a temporary worktree (`git` and `cargo` processes: a developer
/// tool, never on a hook path). `mode` is passed through (`--keyword`).
pub fn compare_run(conn: &Connection, gold: &std::path::Path, reference: &str, mode: &[&str]) -> Result<String> {
    use std::process::Command;
    let data = sys::data_dir();
    let tmp = std::env::temp_dir().join(format!("memorycastle-compare-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let other = if std::path::Path::new(reference).is_file() {
        std::path::PathBuf::from(reference)
    } else {
        let wt = tmp.join("worktree");
        let ok = Command::new("git").args(["worktree", "add", "--detach", "-q"]).arg(&wt).arg(reference).status()?.success();
        if !ok {
            return Err(err!("git worktree add {reference} failed"));
        }
        let target = data.join("compare-target");
        let built = Command::new("cargo").args(["build", "--release", "-q", "--bin", "memcastle"]).current_dir(&wt).env("CARGO_TARGET_DIR", &target).status()?.success();
        let _ = Command::new("git").args(["worktree", "remove", "--force"]).arg(&wt).status();
        if !built {
            return Err(err!("cargo build of {reference} failed"));
        }
        let bin = tmp.join("memcastle-base");
        std::fs::copy(target.join("release/memcastle"), &bin)?;
        bin
    };
    let me = std::env::current_exe()?;
    let mut reports = Vec::new();
    for (name, bin) in [("base", &other), ("new", &me)] {
        let home = tmp.join(name);
        std::fs::create_dir_all(&home)?;
        let db = home.join("memory.db");
        conn.execute("VACUUM INTO ?1", [db.to_string_lossy()])?;
        for shared in ["models", "runtime"] {
            #[cfg(unix)]
            let _ = std::os::unix::fs::symlink(data.join(shared), home.join(shared));
        }
        let out = Command::new(bin)
            .arg("eval")
            .arg("--gold")
            .arg(gold)
            .args(mode)
            .arg("-q")
            .env("MEMORYCASTLE_HOME", &home)
            .env("MEMORYCASTLE_NO_LEDGER", "1")
            .output()?;
        if !out.status.success() {
            return Err(err!("{name} eval failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        // The report is the JSON object; anything printed before it is not.
        let json = text.find('{').map(|i| &text[i..]).unwrap_or("");
        let json = json.rfind('}').map(|j| &json[..=j]).unwrap_or(json);
        reports.push(serde_json::from_str::<Report>(json).map_err(|e| err!("{name} report: {e}"))?);
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(compare(&reports[0], &reports[1]))
}
