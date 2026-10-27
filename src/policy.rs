//! A calibrated show-or-hide policy learned from outcomes (reinforcement
//! learning for calibrated decisions): p(useful) of a candidate note is a
//! logistic over 17 features of one search's trace, its weights a diagonal
//! Gaussian posterior per project. Search shows a note when p >= `POLICY_TAU`
//! (above the floor, which stays) and orders notes by p; the agent-facing
//! search explores by Thompson sampling, showing a note under the bar when a
//! draw from the posterior lifts it over, labelled `explore`. When a logged
//! search settles (`learned::derive`: the note or code the session read
//! next), every shown note is a reward, 1 for the credited one and 0 for the
//! rest, and the posterior is updated by assumed-density filtering (one
//! Laplace step on the batch, diagonal precision). No model call: the
//! features are what search computed already.
//!
//! The prior was fitted offline on 356 questions (gold sets and learned
//! question-note pairs), held-out calibration error 0.007
//! (`2026-10-27-calibrated-policy-replay.md`).

use rusqlite::Connection;

use crate::error::Result;

pub const NF: usize = 17;

/// One candidate note as search saw it: its best chunk's trace and parts.
#[derive(Debug, Clone, Default)]
pub struct Cand {
    pub note_id: i64,
    pub conf: f32,
    pub cosine: Option<f32>,
    pub coverage: f32,
    pub reader: f32,
    pub graph: bool,
    pub bm25_rank: Option<u16>,
    pub vec_rank: Option<u16>,
    pub decision: bool,
    pub decision_part: f32,
    pub parts: usize,
    pub superseded: bool,
    pub files: usize,
    /// 1-based position among the candidate notes.
    pub rank: usize,
}

/// The features of every candidate, in order: confidence, cosine, whether
/// the model scored it, coverage, reading, graph link, log BM25 and vector
/// ranks, Decision chunk, Decision part confidence, parts, superseded, log
/// files named, rank, confidence and reading relative to the best, log rank.
pub fn features(cands: &[Cand]) -> Vec<[f32; NF]> {
    let best_conf = cands.iter().map(|c| c.conf).fold(0.0, f32::max);
    let best_read = cands.iter().map(|c| c.reader).fold(0.0, f32::max);
    let ln = |r: Option<u16>| (1.0 + r.map(f32::from).unwrap_or(150.0)).ln();
    cands
        .iter()
        .map(|c| {
            [
                c.conf,
                c.cosine.unwrap_or(0.0),
                f32::from(c.cosine.is_some() as u8),
                c.coverage,
                c.reader,
                f32::from(c.graph as u8),
                ln(c.bm25_rank),
                ln(c.vec_rank),
                f32::from(c.decision as u8),
                c.decision_part,
                c.parts as f32,
                f32::from(c.superseded as u8),
                (1.0 + c.files as f32).ln(),
                c.rank as f32,
                if best_conf > 0.0 { c.conf / best_conf } else { 0.0 },
                if best_read > 0.0 { c.reader / best_read } else { 0.0 },
                (c.rank.max(1) as f32).ln(),
            ]
        })
        .collect()
}

/// Feature means and spreads of the prior's training set (`examples/policy_replay.py`).
#[allow(clippy::excessive_precision)]
const MU: [f32; NF] = [0.450454, 0.683977, 0.939786, 0.411608, 0.290056, 0.068341, 3.031043, 2.652224, 0.336904, 0.244208, 2.562985, 0.009235, 2.306624, 4.936830, 0.673955, 0.606676, 1.369021];
#[allow(clippy::excessive_precision)]
const SD: [f32; NF] = [0.255549, 0.180073, 0.237884, 0.252878, 0.205874, 0.252332, 1.392116, 1.218630, 0.472653, 0.290580, 0.727835, 0.095657, 0.708960, 3.010768, 0.259077, 0.299706, 0.728519];
/// The prior: posterior mean (17 weights, then the bias) and diagonal
/// precision of a Gaussian-prior logistic fitted on 2707 candidate notes
/// (254 useful) of 356 questions.
#[allow(clippy::excessive_precision)]
const PRIOR_M: [f32; NF + 1] = [-0.043161, 0.714624, -0.960409, -0.133464, 0.135035, 0.101033, -0.530733, -0.525186, 0.081670, -0.030670, 0.220607, 0.009467, -0.238293, -0.213732, 0.612105, 0.475413, -0.116892, -3.482816];
#[allow(clippy::excessive_precision)]
const PRIOR_Q: [f32; NF + 1] = [260.939082, 265.227123, 255.766288, 254.276898, 296.793170, 293.381984, 231.328522, 244.410797, 180.828768, 307.599194, 123.812476, 274.652448, 185.772599, 186.343443, 184.154127, 176.639210, 303.509755, 170.176704];

/// The weights in force for a project: posterior mean `m` and precision `q`
/// (17 weights, then the bias), with the standardisation they apply to.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Posterior {
    pub mu: Vec<f32>,
    pub sd: Vec<f32>,
    pub m: Vec<f32>,
    pub q: Vec<f32>,
    /// Labelled notes learned from since the prior.
    #[serde(default)]
    pub n: u64,
}

impl Posterior {
    /// The shipped prior, its precision scaled by `POLICY_PRIOR_SCALE` so a
    /// project's own outcomes can move it.
    pub fn prior() -> Posterior {
        let s = crate::limits::POLICY_PRIOR_SCALE;
        Posterior { mu: MU.to_vec(), sd: SD.to_vec(), m: PRIOR_M.to_vec(), q: PRIOR_Q.iter().map(|q| q * s).collect(), n: 0 }
    }

    fn z(&self, x: &[f32; NF]) -> Vec<f32> {
        x.iter().zip(&self.mu).zip(&self.sd).map(|((x, mu), sd)| (x - mu) / sd).chain(std::iter::once(1.0)).collect()
    }

    /// p(useful) under the posterior mean.
    pub fn prob(&self, x: &[f32; NF]) -> f32 {
        sigmoid(dot(&self.m, &self.z(x)))
    }

    /// p(useful) under weights `w` drawn from the posterior.
    pub fn prob_with(&self, w: &[f32], x: &[f32; NF]) -> f32 {
        sigmoid(dot(w, &self.z(x)))
    }

    /// One draw of the weights: each N(m, 1/q).
    pub fn sample(&self, rng: &mut Rng) -> Vec<f32> {
        self.m.iter().zip(&self.q).map(|(m, q)| m + rng.normal() / q.max(1e-6).sqrt()).collect()
    }

    /// Assumed-density filtering on one batch of rewards: the MAP under the
    /// current Gaussian (a few diagonal Newton steps), then each precision
    /// grows by the batch's curvature along it.
    pub fn update(&mut self, batch: &[([f32; NF], bool)]) {
        if batch.is_empty() {
            return;
        }
        let zs: Vec<(Vec<f32>, f32)> = batch.iter().map(|(x, y)| (self.z(x), if *y { 1.0 } else { 0.0 })).collect();
        let mut w = self.m.clone();
        for _ in 0..20 {
            let ps: Vec<f32> = zs.iter().map(|(z, _)| sigmoid(dot(&w, z))).collect();
            let mut step = 0.0f32;
            for i in 0..w.len() {
                let mut g = self.q[i] * (w[i] - self.m[i]);
                let mut h = self.q[i];
                for ((z, y), p) in zs.iter().zip(&ps) {
                    g += (p - y) * z[i];
                    h += p * (1.0 - p) * z[i] * z[i];
                }
                let d = g / h;
                w[i] -= d;
                step = step.max(d.abs());
            }
            if step < 1e-5 {
                break;
            }
        }
        for (z, _) in &zs {
            let p = sigmoid(dot(&w, z));
            for (q, zi) in self.q.iter_mut().zip(z) {
                *q += p * (1.0 - p) * zi * zi;
            }
        }
        self.m = w;
        self.n += batch.len() as u64;
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}

fn sigmoid(t: f32) -> f32 {
    1.0 / (1.0 + (-t.clamp(-30.0, 30.0)).exp())
}

/// Whether the policy decides (`MEMORYCASTLE_RL=0` turns it off; then the
/// reader's cut decides, as before).
pub fn enabled() -> bool {
    std::env::var("MEMORYCASTLE_RL").map(|v| v != "0").unwrap_or(true)
}

/// Whether the policy learns from reads and explores (`MEMORYCASTLE_RL_LEARN=1`).
/// Off by default: the shipped weights alone decide, so the same question
/// over the same notes always gives the same answer.
pub fn learning() -> bool {
    std::env::var("MEMORYCASTLE_RL_LEARN").map(|v| v == "1").unwrap_or(false)
}

/// The bar a note's p must reach to be shown (`MEMORYCASTLE_POLICY_TAU`
/// overrides it while measuring).
pub fn tau() -> f32 {
    std::env::var("MEMORYCASTLE_POLICY_TAU").ok().and_then(|v| v.parse().ok()).unwrap_or(crate::limits::POLICY_TAU)
}

fn key(project_id: i64) -> String {
    format!("policy:{project_id}")
}

/// The project's posterior, else the prior. `MEMORYCASTLE_POLICY_WEIGHTS`
/// (a JSON file with `mu`, `sd`, `m`, `q`) replaces both, for held-out
/// measurement.
pub fn load(conn: &Connection, project_id: i64) -> Result<Posterior> {
    if let Some(path) = std::env::var_os("MEMORYCASTLE_POLICY_WEIGHTS") {
        let text = std::fs::read_to_string(&path).map_err(|e| crate::err!("{}: {e}", path.to_string_lossy()))?;
        let mut p: Posterior = serde_json::from_str(&text)?;
        let s = crate::limits::POLICY_PRIOR_SCALE;
        p.q.iter_mut().for_each(|q| *q *= s);
        return Ok(p);
    }
    if !learning() {
        return Ok(Posterior::prior());
    }
    Ok(match crate::db::meta_get(conn, &key(project_id))? {
        Some(v) => serde_json::from_str::<Posterior>(&v).ok().filter(|p| p.m.len() == NF + 1 && p.mu.len() == NF).unwrap_or_else(Posterior::prior),
        None => Posterior::prior(),
    })
}

/// Learns from one settled search: `shown` are the logged candidates the
/// agent saw (and the credited note when search had it as a candidate),
/// `credited` the note the session read next.
pub fn learn(conn: &Connection, project_id: i64, shown: &[(i64, [f32; NF])], credited: i64) -> Result<()> {
    if !enabled() || !learning() || shown.is_empty() || crate::db::is_readonly(conn) || std::env::var_os("MEMORYCASTLE_POLICY_WEIGHTS").is_some() {
        return Ok(());
    }
    let mut post = load(conn, project_id)?;
    let batch: Vec<([f32; NF], bool)> = shown.iter().map(|(id, x)| (*x, *id == credited)).collect();
    post.update(&batch);
    crate::db::meta_set(conn, &key(project_id), &serde_json::to_string(&post)?)
}

/// Forgets what a project learned: back to the prior.
pub fn reset(conn: &Connection, project_id: i64) -> Result<()> {
    conn.execute("DELETE FROM meta WHERE key = ?1", [key(project_id)])?;
    Ok(())
}

/// xorshift64*, seeded from the clock and process: exploration needs no
/// more than an even spread.
pub struct Rng(u64);

impl Rng {
    pub fn seeded() -> Rng {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        Rng((t ^ (u64::from(std::process::id()) << 32)) | 1)
    }

    pub fn with_seed(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn uniform(&mut self) -> f32 {
        ((self.next() >> 40) as f32 + 0.5) / (1u64 << 24) as f32
    }

    /// True once in `n` calls on average.
    pub fn one_in(&mut self, n: u32) -> bool {
        self.next() % u64::from(n.max(1)) == 0
    }

    /// A standard normal draw (Box-Muller).
    pub fn normal(&mut self) -> f32 {
        let (a, b) = (self.uniform(), self.uniform());
        (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(note_id: i64, conf: f32, reader: f32, rank: usize) -> Cand {
        Cand { note_id, conf, cosine: Some(0.6 + conf * 0.2), coverage: reader, reader, bm25_rank: Some(rank as u16), vec_rank: Some(rank as u16), rank, decision: true, decision_part: conf, parts: 2, files: 3, ..Default::default() }
    }

    #[test]
    fn the_prior_ranks_a_strong_candidate_over_a_weak_one() {
        let x = features(&[cand(1, 0.9, 0.9, 1), cand(2, 0.4, 0.1, 5)]);
        let p = Posterior::prior();
        assert!(p.prob(&x[0]) > p.prob(&x[1]));
        assert!((0.0..=1.0).contains(&p.prob(&x[0])));
    }

    #[test]
    fn rewards_move_the_posterior_toward_what_was_read() {
        let x = features(&[cand(1, 0.5, 0.2, 1), cand(2, 0.45, 0.8, 2)]);
        let mut p = Posterior::prior();
        let before = (p.prob(&x[0]), p.prob(&x[1]));
        for _ in 0..30 {
            p.update(&[(x[0], false), (x[1], true)]);
        }
        assert!(p.prob(&x[1]) > before.1 && p.prob(&x[0]) < before.0);
        assert!(p.q.iter().zip(Posterior::prior().q.iter()).all(|(a, b)| a >= b), "precision only grows");
        assert_eq!(p.n, 60);
    }

    #[test]
    fn thompson_draws_spread_around_the_mean() {
        let p = Posterior::prior();
        let mut rng = Rng::with_seed(7);
        let draws: Vec<f32> = (0..2000).map(|_| p.sample(&mut rng)[0]).collect();
        let mean = draws.iter().sum::<f32>() / draws.len() as f32;
        let sd = (draws.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / draws.len() as f32).sqrt();
        assert!((mean - p.m[0]).abs() < 0.05, "{mean}");
        assert!((sd - 1.0 / p.q[0].sqrt()).abs() < 0.05, "{sd}");
    }
}
