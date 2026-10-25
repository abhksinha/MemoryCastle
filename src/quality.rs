//! What the store can say about its own notes without a model call: two
//! scores from vectors already stored, and one from the label log.
//!
//! - spread: the lowest cosine between two chunks of the same note. Low
//!   means the note holds several topics (the batch-note smell).
//! - overlap: the highest cosine between a chunk of this note and a chunk
//!   of another. High means a duplicate or a note that should supersede.
//! - served, opened: how often a search printed the note and how often
//!   something followed (its sections, or the file itself) within the
//!   follow window. Served often and never opened is the one signal the
//!   users of the memory produce themselves.
//!
//! Verdicts are percentiles of the store's own notes (`QUALITY_LOW_PCT`,
//! `QUALITY_HIGH_PCT`), so the bar moves with the journal; under
//! `QUALITY_MIN_NOTES` the numbers print without verdicts.

use crate::limits::{OPEN_MIN_SECS, RECALL_IGNORED_CONFIDENCE, RECALL_MIN_CONFIDENCE, RECALL_REPEAT_SECS, QUALITY_HIGH_PCT, QUALITY_LOW_PCT, QUALITY_MIN_NOTES, SERVED_NEVER_FOLLOWED, SPAN_FOLLOW_SECS};
use crate::{embed, Result};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteQuality {
    pub note_id: i64,
    pub path: String,
    pub chunks: usize,
    /// Lowest within-note cosine; None with fewer than two embedded chunks.
    pub spread: Option<f32>,
    /// Highest cosine to a chunk of another note, and that note's path.
    pub overlap: Option<(f32, String)>,
    pub served: i64,
    pub opened: i64,
    /// Verdict words, empty when nothing stands out or the store is small.
    pub flags: Vec<String>,
}

/// (chunk id, note id, vector).
type ChunkVec = (i64, i64, Vec<f32>);

/// Chunk vectors of a project in the active space, with their note id.
fn vectors(conn: &Connection, project_id: i64) -> Result<(usize, Vec<ChunkVec>)> {
    let Some(space) = embed::active_space(conn)? else { return Ok((0, Vec::new())) };
    let mut stmt = conn.prepare_cached(
        "SELECT c.id, c.note_id, v.vec FROM vectors v JOIN chunks c ON c.id = v.chunk_id
         WHERE v.project_id = ?1 AND v.space_id = ?2 AND length(v.vec) = ?3",
    )?;
    let dims = space.def.dims;
    let mut out = Vec::new();
    let mut floats = Vec::with_capacity(dims);
    for row in stmt.query_map(params![project_id, space.id, (dims * 2) as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?)))? {
        let (cid, nid, blob) = row?;
        embed::decode_f16(&blob, &mut floats);
        out.push((cid, nid, floats.clone()));
    }
    Ok((dims, out))
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Scores for every live note of the project, worst first. Up to
/// `QUALITY_MAX_CHUNKS` every chunk pair is compared; above it, the
/// `QUALITY_RECENT_NOTES` newest notes are the subjects and the rest only
/// serve as neighbours, so the pass stays linear in the store.
pub fn notes(conn: &Connection, project_id: i64) -> Result<Vec<NoteQuality>> {
    let (_, vecs) = vectors(conn, project_id)?;
    let subjects: Option<HashSet<i64>> = if vecs.len() > crate::limits::QUALITY_MAX_CHUNKS {
        let mut stmt = conn.prepare_cached("SELECT id FROM notes WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY saved_at DESC LIMIT ?2")?;
        let ids: HashSet<i64> = stmt.query_map(params![project_id, crate::limits::QUALITY_RECENT_NOTES as i64], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Some(ids)
    } else {
        None
    };
    let mut out = score(conn, project_id, &vecs, subjects.as_ref())?;
    flag(&mut out);
    out.sort_by(|a, b| b.flags.len().cmp(&a.flags.len()).then(a.spread.unwrap_or(1.0).partial_cmp(&b.spread.unwrap_or(1.0)).unwrap_or(std::cmp::Ordering::Equal)));
    Ok(out)
}

/// Spread and overlap for the subject notes (all when None) against every
/// chunk: pairs with no subject on either side are skipped.
fn score(conn: &Connection, project_id: i64, vecs: &[ChunkVec], subjects: Option<&HashSet<i64>>) -> Result<Vec<NoteQuality>> {
    let mut paths: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn.prepare_cached("SELECT id, path FROM notes WHERE project_id = ?1 AND deleted_at IS NULL")?;
        for row in stmt.query_map([project_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, p) = row?;
            paths.insert(id, p);
        }
    }
    let is_subject = |n: i64| subjects.is_none_or(|s| s.contains(&n));
    let mut spread: HashMap<i64, f32> = HashMap::new();
    let mut overlap: HashMap<i64, (f32, i64)> = HashMap::new();
    let mut chunks: HashMap<i64, usize> = HashMap::new();
    for (_, nid, _) in vecs {
        *chunks.entry(*nid).or_insert(0) += 1;
    }
    for i in 0..vecs.len() {
        let (_, ni, vi) = &vecs[i];
        let si = is_subject(*ni);
        for (_, nj, vj) in &vecs[i + 1..] {
            let sj = is_subject(*nj);
            if !si && !sj {
                continue;
            }
            let c = dot(vi, vj);
            if ni == nj {
                let e = spread.entry(*ni).or_insert(1.0);
                if c < *e {
                    *e = c;
                }
            } else {
                for (a, b, sa) in [(*ni, *nj, si), (*nj, *ni, sj)] {
                    if !sa {
                        continue;
                    }
                    let e = overlap.entry(a).or_insert((-1.0, b));
                    if c > e.0 {
                        *e = (c, b);
                    }
                }
            }
        }
    }
    let (served, opened) = served_opened(conn, project_id)?;
    Ok(paths
        .iter()
        .filter(|(id, _)| is_subject(**id))
        .map(|(id, path)| NoteQuality {
            note_id: *id,
            path: path.clone(),
            chunks: chunks.get(id).copied().unwrap_or(0),
            spread: spread.get(id).copied(),
            overlap: overlap.get(id).and_then(|(c, other)| paths.get(other).map(|p| (*c, p.clone()))),
            served: served.get(id).copied().unwrap_or(0),
            opened: opened.get(id).copied().unwrap_or(0),
            flags: Vec::new(),
        })
        .collect())
}

/// Percentile verdicts over the store's own notes.
fn flag(notes: &mut [NoteQuality]) {
    if notes.len() < QUALITY_MIN_NOTES {
        for n in notes.iter_mut() {
            if n.served >= SERVED_NEVER_FOLLOWED && n.opened == 0 {
                n.flags.push(format!("served {} times, never opened", n.served));
            }
        }
        return;
    }
    let mut spreads: Vec<f32> = notes.iter().filter_map(|n| n.spread).collect();
    let mut overlaps: Vec<f32> = notes.iter().filter_map(|n| n.overlap.as_ref().map(|o| o.0)).collect();
    spreads.sort_by(f32::total_cmp);
    overlaps.sort_by(f32::total_cmp);
    let pct = |v: &[f32], p: f64| v.get(((v.len() as f64 - 1.0) * p).round() as usize).copied();
    let low = pct(&spreads, QUALITY_LOW_PCT);
    let high = pct(&overlaps, QUALITY_HIGH_PCT);
    for n in notes.iter_mut() {
        if let (Some(s), Some(l)) = (n.spread, low) {
            if s <= l && n.chunks >= 3 {
                n.flags.push(format!("spread {s:.2}, weakest {:.0}%: several topics in one note, split it", QUALITY_LOW_PCT * 100.0));
            }
        }
        if let (Some((o, other)), Some(h)) = (&n.overlap, high) {
            if *o >= h {
                n.flags.push(format!("overlap {o:.2} with {other}, top {:.0}%: duplicate, or add `Supersedes:`", 100.0 - QUALITY_HIGH_PCT * 100.0));
            }
        }
        if n.served >= SERVED_NEVER_FOLLOWED && n.opened == 0 {
            n.flags.push(format!("served {} times, never opened", n.served));
        }
    }
}

/// Per note: times a search printed it, and times something followed
/// within the window (its sections printed, or the note file read), no
/// sooner than `OPEN_MIN_SECS` after: a loop that asks for sections a
/// few hundred milliseconds later is a script, not a reader.
fn served_opened(conn: &Connection, project_id: i64) -> Result<(HashMap<i64, i64>, HashMap<i64, i64>)> {
    let mut served = HashMap::new();
    let mut opened = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT s.note_id, count(*),
                sum(EXISTS (SELECT 1 FROM served_notes t WHERE t.project_id = s.project_id AND t.note_id = s.note_id AND t.mode IN ('sections', 'read') AND t.at >= s.at + ?3 AND t.at <= s.at + ?2)
                    OR EXISTS (SELECT 1 FROM reads r JOIN notes n ON n.id = s.note_id WHERE r.project_id = s.project_id AND r.path = n.path AND r.at >= s.at + ?3 AND r.at <= s.at + ?2))
         FROM served_notes s WHERE s.project_id = ?1 AND s.mode NOT IN ('sections', 'read') GROUP BY s.note_id",
    )?;
    for row in stmt.query_map(params![project_id, SPAN_FOLLOW_SECS, OPEN_MIN_SECS], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))? {
        let (id, n, f) = row?;
        served.insert(id, n);
        opened.insert(id, f);
    }
    Ok((served, opened))
}

/// Store-wide label-log totals: (notes served, of which opened after).
pub fn label_totals(conn: &Connection, project_id: i64) -> Result<(i64, i64)> {
    let (s, o) = served_opened(conn, project_id)?;
    Ok((s.values().sum(), o.values().sum()))
}

/// The scores of one note against the rest of the store, for `save`: the
/// note's chunks against every vector, linear in the store. Verdicts use
/// the store-wide percentiles only while the full table is cheap.
pub fn of_note(conn: &Connection, project_id: i64, note_id: i64) -> Result<Option<NoteQuality>> {
    let (_, vecs) = vectors(conn, project_id)?;
    if vecs.len() <= crate::limits::QUALITY_MAX_CHUNKS {
        let mut all = score(conn, project_id, &vecs, None)?;
        flag(&mut all);
        return Ok(all.into_iter().find(|n| n.note_id == note_id));
    }
    let one: HashSet<i64> = [note_id].into_iter().collect();
    let mut out = score(conn, project_id, &vecs, Some(&one))?;
    flag(&mut out);
    Ok(out.pop())
}

/// One line per note for the CLI: path, chunks, spread, overlap, served/opened, flags.
pub fn render(notes: &[NoteQuality], only_flagged: bool) -> String {
    let mut out = String::new();
    let w = notes.iter().map(|n| n.path.len()).max().unwrap_or(0);
    for n in notes {
        if only_flagged && n.flags.is_empty() {
            continue;
        }
        let spread = n.spread.map(|s| format!("{s:.2}")).unwrap_or_else(|| "-".into());
        let overlap = n.overlap.as_ref().map(|(c, _)| format!("{c:.2}")).unwrap_or_else(|| "-".into());
        out.push_str(&format!("{:w$}  chunks {:2}  spread {spread}  overlap {overlap}  served {:3} opened {:3}", n.path, n.chunks, n.served, n.opened));
        if !n.flags.is_empty() {
            out.push_str("  <- ");
            out.push_str(&n.flags.join("; "));
        }
        out.push('\n');
    }
    out
}

/// The confidence a note must reach to be recalled, and why when it is
/// raised. A note the agent keeps ignoring costs a line on every prompt it
/// matches: after `SERVED_NEVER_FOLLOWED` recalls with no open since the
/// last one (a read of the note, its sections, or its file), or a recall in
/// the last `RECALL_REPEAT_SECS` with no open since, the bar is
/// `RECALL_IGNORED_CONFIDENCE`. Any open resets it. Two indexed queries.
pub fn recall_bar(conn: &Connection, project_id: i64, note_id: i64, now: i64) -> Result<(f32, Option<String>)> {
    let last_open: i64 = conn.query_row(
        "SELECT max(at) FROM (
             SELECT max(at) AS at FROM served_notes WHERE project_id = ?1 AND note_id = ?2 AND mode IN ('sections', 'read')
             UNION ALL
             SELECT max(r.at) FROM reads r JOIN notes n ON n.id = ?2 WHERE r.project_id = ?1 AND r.path = n.path)",
        params![project_id, note_id],
        |r| r.get::<_, Option<i64>>(0),
    )?.unwrap_or(0);
    let (since, last): (i64, Option<i64>) = conn.query_row(
        "SELECT count(*), max(at) FROM served_notes WHERE project_id = ?1 AND note_id = ?2 AND mode = 'recall' AND at > ?3",
        params![project_id, note_id, last_open],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if since >= SERVED_NEVER_FOLLOWED {
        return Ok((RECALL_IGNORED_CONFIDENCE, Some(format!("recalled {since} times, not opened since"))));
    }
    if let Some(at) = last.filter(|at| now - at < RECALL_REPEAT_SECS) {
        return Ok((RECALL_IGNORED_CONFIDENCE, Some(format!("recalled {} min ago, not opened since", (now - at) / 60))));
    }
    Ok((RECALL_MIN_CONFIDENCE, None))
}
