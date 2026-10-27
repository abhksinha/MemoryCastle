//! The reader: what a note's own text says about the question, by counting,
//! no model. Each candidate chunk is read for which of the question's words
//! it holds, weighted by how rare each word is in the scope (IDF), and how
//! close together they sit (the smallest window holding them all). A note's
//! reading is its best chunk's. Noise notes match a word here and another
//! there; the answer holds the rare words together in one bullet.
//!
//! Search uses the reading two ways: it multiplies the fused score, so the
//! note that reads best rises inside its confidence quarter, and it cuts a
//! note whose reading is far under the best note's (`READER_CUT`), a cut
//! relative to this question, under the absolute floor, never instead of it.

use std::collections::HashMap;

use rusqlite::Connection;

use crate::error::Result;
use crate::project::Scope;
use crate::search::Hit;

/// One question word with its weight in this scope.
#[derive(Debug, Clone)]
pub struct Term {
    pub stem: String,
    pub idf: f32,
}

/// The question's content words, each with its IDF over the scope's chunks
/// (`ln(1 + (N - df + 0.5) / (df + 0.5))`, df counted by FTS on the stem as
/// a prefix, as `fts::build` matches it).
pub fn terms(conn: &Connection, scope: &Scope, query: &str) -> Result<Vec<Term>> {
    // A hyphenated or snake word also reads as its parts: "sign-bit" is
    // "sign bit" in the note.
    let mut words = crate::search::content_words(query);
    let parts: Vec<String> = words.iter().filter(|w| w.contains(['-', '_'])).flat_map(|w| w.split(['-', '_']).filter(|p| p.len() >= 3).map(str::to_string).collect::<Vec<_>>()).collect();
    words.retain(|w| !w.contains(['-', '_']) || parts.is_empty());
    words.extend(parts);
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let total: f32 = conn.query_row(
        &format!("SELECT count(*) FROM chunks WHERE project_id IN {}", scope.placeholders(1)),
        rusqlite::params_from_iter(scope.ids()),
        |r| r.get::<_, i64>(0),
    )? as f32;
    let sql = format!("SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1 AND project_id IN {}", scope.placeholders(2));
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut out = Vec::with_capacity(words.len());
    for w in words {
        let stem = crate::search::stem(&w).to_string();
        let m = format!("\"{}\"*", stem.replace('"', ""));
        let binds = std::iter::once(rusqlite::types::Value::Text(m)).chain(scope.bind());
        let df = stmt.query_row(rusqlite::params_from_iter(binds), |r| r.get::<_, i64>(0)).unwrap_or(0) as f32;
        let idf = (1.0 + (total - df + 0.5) / (df + 0.5)).ln();
        if !out.iter().any(|t: &Term| t.stem == stem) {
            out.push(Term { stem, idf });
        }
    }
    Ok(out)
}

/// How one chunk reads against the question.
#[derive(Debug, Clone, Copy, Default)]
pub struct Reading {
    /// Share of the question's IDF mass present.
    pub cover: f32,
    /// Distinct words present over the width of the smallest window holding
    /// them all (1.0 when they sit side by side, or one word).
    pub tight: f32,
    pub score: f32,
}

/// Reads `text` (the heading and the chunk) for `terms`.
pub fn read(text: &str, terms: &[Term]) -> Reading {
    let total: f32 = terms.iter().map(|t| t.idf).sum();
    if total <= 0.0 {
        return Reading::default();
    }
    let lower = text.to_ascii_lowercase();
    // (position, term index) of every match, in text order.
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for (pos, w) in crate::fts::terms(&lower).into_iter().enumerate() {
        let parts = w.split(['_', '-']).filter(|p| !p.is_empty());
        for (i, t) in terms.iter().enumerate() {
            if w.starts_with(t.stem.as_str()) || parts.clone().any(|p| p.starts_with(t.stem.as_str())) {
                hits.push((pos, i));
            }
        }
    }
    let mut present = vec![false; terms.len()];
    for &(_, i) in &hits {
        present[i] = true;
    }
    let distinct = present.iter().filter(|p| **p).count();
    if distinct == 0 {
        return Reading::default();
    }
    let cover = terms.iter().zip(&present).filter(|(_, p)| **p).map(|(t, _)| t.idf).sum::<f32>() / total;
    // Smallest window holding every distinct term present.
    let mut count = vec![0usize; terms.len()];
    let (mut have, mut lo, mut best) = (0usize, 0usize, usize::MAX);
    for hi in 0..hits.len() {
        let i = hits[hi].1;
        if count[i] == 0 {
            have += 1;
        }
        count[i] += 1;
        while have == distinct {
            best = best.min(hits[hi].0 - hits[lo].0 + 1);
            let j = hits[lo].1;
            count[j] -= 1;
            if count[j] == 0 {
                have -= 1;
            }
            lo += 1;
        }
    }
    let tight = (distinct as f32 / best.max(1) as f32).min(1.0);
    let a = knob("TIGHT_BASE", crate::limits::READER_TIGHT_BASE);
    Reading { cover, tight, score: cover * (a + (1.0 - a) * tight) }
}

/// Reads every hit and returns each note's reading (its best chunk's score).
/// Sets `trace.reader` on each hit to its note's reading.
pub fn read_hits(terms: &[Term], hits: &mut [Hit]) -> HashMap<i64, f32> {
    let mut notes: HashMap<i64, f32> = HashMap::new();
    for h in hits.iter() {
        let r = read(&format!("{}\n{}", h.heading, h.text), terms);
        let e = notes.entry(h.note_id).or_insert(0.0);
        *e = e.max(r.score);
    }
    for h in hits.iter_mut() {
        h.trace.reader = notes.get(&h.note_id).copied().unwrap_or(0.0);
    }
    notes
}

/// A bound from `limits`, overridden by `MEMORYCASTLE_READER_<NAME>` while
/// it is being measured.
pub fn knob(name: &str, default: f32) -> f32 {
    std::env::var(format!("MEMORYCASTLE_READER_{name}")).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Whether the reader runs (`MEMORYCASTLE_READER=0` turns it off, for
/// measuring against the build without it).
pub fn enabled() -> bool {
    std::env::var("MEMORYCASTLE_READER").map(|v| v != "0").unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str, idf: f32) -> Term {
        Term { stem: s.into(), idf }
    }

    #[test]
    fn words_together_read_better_than_words_apart() {
        let terms = [t("drive", 2.0), t("overwrit", 3.0), t("guard", 2.5)];
        let together = read("The drive overwrite guard keeps the newest backup.", &terms);
        let apart = read("Drive sync runs hourly. Many unrelated words sit here between them all along. A guard on the queue. Later an overwrite of the cache.", &terms);
        assert!((together.cover - 1.0).abs() < 1e-6 && (apart.cover - 1.0).abs() < 1e-6);
        assert!(together.score > apart.score);
        let one = read("A guard on the queue.", &terms);
        assert!(one.score < apart.score);
    }

    #[test]
    fn rare_words_weigh_more() {
        let terms = [t("note", 0.2), t("mailbox", 4.0)];
        assert!(read("the mailbox", &terms).score > read("the note", &terms).score);
    }
}
