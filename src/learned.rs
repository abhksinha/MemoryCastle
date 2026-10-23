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
/// the search did not rank it first; a read of the first hit ends it, even
/// if another note is read later. A note saved after the search is one the
/// agent wrote, not one it found. Returns how many were learned.
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
        if rank == Some(0) {
            continue;
        }
        let kind = if rank.is_some_and(|r| r < 3) { "lift" } else { "correction" };
        add(conn, project_id, note, &query, &vec, space_id, kind)?;
        learned += 1;
    }
    Ok(learned)
}

/// Stores one learned question for a note (vector as little-endian f32),
/// keeping the newest `LEARNED_PER_NOTE` per note.
pub fn add(conn: &Connection, project_id: i64, note_id: i64, query: &str, vec: &[u8], space_id: i64, kind: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO learned(project_id, note_id, query, vec, space_id, at, kind) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(note_id, query) DO UPDATE SET at = excluded.at, vec = excluded.vec, space_id = excluded.space_id",
        params![project_id, note_id, query, vec, space_id, crate::sys::now(), kind],
    )?;
    conn.execute(
        "DELETE FROM learned WHERE note_id = ?1 AND id NOT IN (SELECT id FROM learned WHERE note_id = ?1 ORDER BY at DESC, id DESC LIMIT ?2)",
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
    let mut stmt = conn.prepare_cached(
        "SELECT l.note_id, l.vec, n.project_id, n.path, n.superseded_by FROM learned l JOIN notes n ON n.id = l.note_id AND n.deleted_at IS NULL WHERE l.project_id = ?1 AND l.space_id = ?2",
    )?;
    let mut by_path = conn.prepare_cached("SELECT id FROM notes WHERE project_id = ?1 AND path = ?2 AND deleted_at IS NULL")?;
    for pid in project_ids {
        type Row = (i64, Vec<u8>, i64, String, Option<String>);
        let rows: Vec<Row> =
            stmt.query_map(params![pid, space_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<rusqlite::Result<_>>()?;
        for (note, blob, project, path, superseded_by) in rows {
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
