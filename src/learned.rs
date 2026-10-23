//! Learned questions: the index learns from what agents do, not from a
//! model change. Every agent search is logged (`searches`); when the agent
//! then reads the file of a note the search did not rank first (same session, within `LEARN_WINDOW_SECS`), the question
//! and its vector are kept for that note (`learned`). A later question
//! close to a learned one (cosine at least `LEARNED_MIN_COSINE`, which on
//! the gold sets is the same question in other words, never a question
//! about another note) brings that note in with a confidence from the
//! match. Notes, not instructions: the rows are what agents asked.

use crate::search::Explain;
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};

/// The learned channel runs unless `MEMORYCASTLE_LEARNED=0` (for measuring it).
pub fn enabled() -> bool {
    std::env::var("MEMORYCASTLE_LEARNED").map(|v| v != "0").unwrap_or(true)
}

fn to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// A question's content words, sorted, space-joined: what `word_matches`
/// compares, computed once at insert.
pub fn words_of(query: &str) -> String {
    crate::search::content_words(query).join(" ")
}

/// Sign bits of a vector, one per dimension, set when positive.
fn sign_bits(v: impl Iterator<Item = f32>) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, x) in v.enumerate() {
        if i % 8 == 0 {
            out.push(0);
        }
        if x > 0.0 {
            *out.last_mut().unwrap() |= 1 << (i % 8);
        }
    }
    out
}

/// Sign bits of a stored little-endian f32 vector.
pub fn bits_of_blob(blob: &[u8]) -> Vec<u8> {
    sign_bits(blob.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])))
}

fn hamming(a: &[u8], b: &[u8]) -> u32 {
    a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum()
}

fn dot(blob: &[u8], q: &[f32]) -> f32 {
    blob.chunks_exact(4).zip(q).map(|(b, x)| f32::from_le_bytes([b[0], b[1], b[2], b[3]]) * x).sum()
}

/// Confidence a learned match gives its note: `LEARNED_MIN_COSINE` is 0.5
/// (verify), an identical question 1.0.
pub fn confidence(cos: f32) -> f32 {
    let lo = crate::limits::LEARNED_MIN_COSINE;
    0.5 + 0.5 * ((cos - lo) / (1.0 - lo)).clamp(0.0, 1.0)
}

/// Logs an agent's search: the question, its other wordings, the notes
/// printed in order and the question's vector when the model ran. Nothing
/// for benchmarks (`MEMORYCASTLE_NO_LEDGER`), the hooks' own lookups
/// (`MEMORYCASTLE_VIA`) or a read-only store.
pub fn record_search(conn: &Connection, project_id: i64, query: &str, also: &[String], printed: &[i64], ex: &Explain) -> Result<()> {
    if crate::db::is_readonly(conn) || std::env::var_os("MEMORYCASTLE_NO_LEDGER").is_some() || std::env::var_os("MEMORYCASTLE_VIA").is_some() {
        return Ok(());
    }
    let session = crate::store::current_session(conn, project_id)?;
    let printed: Vec<String> = printed.iter().map(|i| i.to_string()).collect();
    let vec = (!ex.qvec.is_empty()).then(|| to_bytes(&ex.qvec));
    conn.execute(
        "INSERT INTO searches(at, project_id, session, query, also, printed, space_id, vec) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![crate::sys::now(), project_id, session, query, also.join("\n"), printed.join(","), ex.space_id, vec],
    )?;
    Ok(())
}

/// Turns logged searches whose window has closed into learned questions.
/// The first note file the same session read (a `read` row in
/// `served_notes`; a `--sections` search prints several notes nobody chose,
/// so it is not an open) at least `OPEN_MIN_SECS` and at most `LEARN_WINDOW_SECS`
/// after the search is the answer, unless recall, `links` or a hook served
/// that note in between (then that is why it was read). Learned only when
/// the search ranked it: `confirm` first (the next wording may rank worse),
/// `lift` in the top 3, `correction` below; a read of the first hit ends it,
/// even if another note is read later. A note saved after the search is one
/// the agent wrote, not one it found. A failed wording followed by a second
/// search and a read is learned too: its first read is that note. Returns how many were learned.
pub fn derive(conn: &Connection, project_id: i64) -> Result<usize> {
    use crate::limits::{LEARN_WINDOW_SECS, OPEN_MIN_SECS};
    if crate::db::is_readonly(conn) {
        return Ok(0);
    }
    let now = crate::sys::now();
    let mut pending = conn.prepare_cached(
        "SELECT id, at, session, query, printed, space_id, vec FROM searches WHERE project_id = ?1 AND done = 0 AND at <= ?2",
    )?;
    type Row = (i64, i64, Option<String>, String, String, Option<i64>, Option<Vec<u8>>);
    let rows: Vec<Row> = pending
        .query_map(params![project_id, now - LEARN_WINDOW_SECS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut opened = conn.prepare_cached(
        "SELECT note_id, at FROM served_notes WHERE project_id = ?1 AND session = ?2 AND mode = 'read' AND at >= ?3 AND at <= ?4 ORDER BY at LIMIT 1",
    )?;
    let mut nudged = conn.prepare_cached(
        "SELECT 1 FROM served_notes WHERE project_id = ?1 AND session = ?2 AND note_id = ?3 AND mode NOT IN ('read', 'sections', 'brief', 'snippet') AND at >= ?4 AND at <= ?5 LIMIT 1",
    )?;
    let mut written = conn.prepare_cached("SELECT 1 FROM notes WHERE id = ?1 AND saved_at > ?2")?;
    let mut learned = 0;
    for (id, at, session, query, printed, space_id, vec) in rows {
        conn.execute("UPDATE searches SET done = 1 WHERE id = ?1", [id])?;
        let (Some(session), Some(space_id), Some(vec)) = (session, space_id, vec) else { continue };
        let Some((note, when)) = opened.query_row(params![project_id, session, at + OPEN_MIN_SECS, at + LEARN_WINDOW_SECS], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).optional()? else { continue };
        if nudged.exists(params![project_id, session, note, at, when])? || written.exists(params![note, at])? {
            continue;
        }
        let rank = printed.split(',').position(|p| p == note.to_string());
        let kind = match rank {
            Some(0) => "confirm",
            Some(r) if r < 3 => "lift",
            _ => "correction",
        };
        add(conn, project_id, note, &query, &vec, space_id, kind)?;
        learned += 1;
    }
    Ok(learned)
}

/// Stores one learned question for a note (vector as little-endian f32),
/// keeping `LEARNED_PER_NOTE` per note: corrections and lifts before
/// confirmations, newest first.
pub fn add(conn: &Connection, project_id: i64, note_id: i64, query: &str, vec: &[u8], space_id: i64, kind: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO learned(project_id, note_id, query, vec, space_id, at, kind, words, bits) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(note_id, query) DO UPDATE SET at = excluded.at, vec = excluded.vec, space_id = excluded.space_id, bits = excluded.bits",
        params![project_id, note_id, query, vec, space_id, crate::sys::now(), kind, words_of(query), bits_of_blob(vec)],
    )?;
    conn.execute(
        "DELETE FROM learned WHERE note_id = ?1 AND id NOT IN (SELECT id FROM learned WHERE note_id = ?1 ORDER BY kind = 'confirm', at DESC, id DESC LIMIT ?2)",
        params![note_id, crate::limits::LEARNED_PER_NOTE as i64],
    )?;
    Ok(())
}

/// `memcastle learned add`: a question embedded now, for a note.
pub fn add_vector(conn: &Connection, project_id: i64, note_id: i64, query: &str, qvec: &[f32], space_id: i64) -> Result<()> {
    add(conn, project_id, note_id, query, &to_bytes(qvec), space_id, "manual")
}

/// Notes whose learned questions are within `LEARNED_MIN_COSINE` of the
/// question, best first, as (note id, cosine). A superseded note's
/// questions point at the note that replaced it.
pub fn matches(conn: &Connection, project_ids: &[i64], space_id: i64, qvec: &[f32]) -> Result<Vec<(i64, f32)>> {
    let mut out: Vec<(i64, f32)> = Vec::new();
    // Sign bits first (64 bytes a row), the exact cosine only for rows within
    // `LEARNED_BITS_MAX` differing bits: every pair at cosine 0.86 or more
    // on the gold questions was within 133, and 0.5% of all pairs pass 140.
    let qbits = sign_bits(qvec.iter().copied());
    let mut stmt = conn.prepare_cached("SELECT id, bits FROM learned WHERE project_id = ?1 AND space_id = ?2")?;
    let mut exact = conn.prepare_cached(
        "SELECT l.note_id, l.vec, n.project_id, n.path, n.superseded_by FROM learned l JOIN notes n ON n.id = l.note_id AND n.deleted_at IS NULL WHERE l.id = ?1",
    )?;
    let mut by_path = conn.prepare_cached("SELECT id FROM notes WHERE project_id = ?1 AND path = ?2 AND deleted_at IS NULL")?;
    for pid in project_ids {
        let mut near: Vec<i64> = Vec::new();
        let mut rows = stmt.query(params![pid, space_id])?;
        while let Some(r) = rows.next()? {
            let close = match r.get_ref(1)?.as_blob_or_null().map_err(rusqlite::Error::from)? {
                Some(b) => hamming(b, &qbits) <= crate::limits::LEARNED_BITS_MAX,
                None => true,
            };
            if close {
                near.push(r.get(0)?);
            }
        }
        type Row = (i64, Vec<u8>, i64, String, Option<String>);
        for id in near {
            let Some((note, blob, project, path, superseded_by)) =
                exact.query_row([id], |r| Ok::<Row, _>((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).optional()?
            else {
                continue;
            };
            if blob.len() != qvec.len() * 4 {
                continue;
            }
            let cos = dot(&blob, qvec);
            if cos < crate::limits::LEARNED_MIN_COSINE {
                continue;
            }
            let note = match superseded_by {
                Some(newer) => {
                    let dir = std::path::Path::new(&path).parent().map(|p| p.join(&newer).to_string_lossy().into_owned()).unwrap_or(newer.clone());
                    by_path.query_row(params![project, dir], |r| r.get(0)).optional()?.or(by_path.query_row(params![project, newer], |r| r.get(0)).optional()?).unwrap_or(note)
                }
                None => note,
            };
            match out.iter_mut().find(|(n, _)| *n == note) {
                Some(e) => e.1 = e.1.max(cos),
                None => out.push((note, cos)),
            }
        }
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(out)
}

/// Notes whose learned question shares content words with the question:
/// at least two, and at least half of the learned question's own, best
/// first, as (note id, share of the learned question's words). Text only, no vectors: it works without the
/// model and catches a new question that names the same rare things.
pub fn word_matches(conn: &Connection, project_ids: &[i64], words: &[String]) -> Result<Vec<(i64, f32)>> {
    let mut out: Vec<(i64, f32)> = Vec::new();
    if words.len() < 2 {
        return Ok(out);
    }
    // The index returns only rows sharing two words with the question (a
    // match needs two): pairs intersect postings, so a common word alone
    // never pulls thousands of rows.
    let ws = &words[..words.len().min(8)];
    let mut pairs: Vec<String> = Vec::new();
    for (i, a) in ws.iter().enumerate() {
        for b in &ws[i + 1..] {
            pairs.push(format!("(\"{a}\" AND \"{b}\")"));
        }
    }
    let fts = pairs.join(" OR ");
    let mut stmt = conn.prepare_cached(
        "SELECT l.note_id, l.words FROM learned_fts f JOIN learned l ON l.id = f.rowid JOIN notes n ON n.id = l.note_id AND n.deleted_at IS NULL AND n.superseded_by IS NULL WHERE learned_fts MATCH ?1 AND l.project_id = ?2",
    )?;
    for pid in project_ids {
        for row in stmt.query_map(params![fts, pid], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (note, stored) = row?;
            let lw: Vec<&str> = stored.split(' ').filter(|w| !w.is_empty()).collect();
            let shared = lw.iter().filter(|w| words.binary_search_by(|x| x.as_str().cmp(w)).is_ok()).count();
            if shared >= 2 && shared * 2 >= lw.len() {
                let share = shared as f32 / lw.len() as f32;
                match out.iter_mut().find(|(n, _)| *n == note) {
                    Some(e) => e.1 = e.1.max(share),
                    None => out.push((note, share)),
                }
            }
        }
    }
    out.sort_by(|a, b| b.1.total_cmp(&a.1));
    Ok(out)
}

/// One learned question, for `memcastle learned` and the graph.
pub struct Learned {
    pub id: i64,
    pub note_id: i64,
    pub path: String,
    pub query: String,
    pub kind: String,
    pub at: i64,
}

pub fn list(conn: &Connection, project_id: i64) -> Result<Vec<Learned>> {
    let mut stmt = conn.prepare_cached(
        "SELECT l.id, l.note_id, n.path, l.query, l.kind, l.at FROM learned l JOIN notes n ON n.id = l.note_id AND n.deleted_at IS NULL WHERE l.project_id = ?1 ORDER BY n.path, l.at DESC",
    )?;
    let rows = stmt.query_map([project_id], |r| Ok(Learned { id: r.get(0)?, note_id: r.get(1)?, path: r.get(2)?, query: r.get(3)?, kind: r.get(4)?, at: r.get(5)? }))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn forget(conn: &Connection, project_id: i64, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM learned WHERE project_id = ?1 AND id = ?2", params![project_id, id])? > 0)
}

/// Drops logged searches older than `SEARCH_LOG_DAYS`; learned rows stay.
pub fn prune(conn: &Connection) -> Result<usize> {
    if crate::db::is_readonly(conn) {
        return Ok(0);
    }
    Ok(conn.execute("DELETE FROM searches WHERE done = 1 AND at < ?1", [crate::sys::now() - crate::limits::SEARCH_LOG_DAYS * 86_400])?)
}
