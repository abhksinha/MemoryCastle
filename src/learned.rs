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
use std::collections::HashMap;

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
/// search and a read is learned too: its first read is that note.
///
/// Without a note read, the first code file the session read or edited in
/// the window credits the best-ranked printed note that covers it (names it
/// in `Files:` or names a symbol defined in it), as `code`, when the file is
/// specific (`learned_specificity`, at least `LEARN_FILE_MIN_SPECIFIC`): a
/// file most notes name says nothing about which note answered. Either way
/// the first range read in a file the credited note covers is kept with the
/// question, so the next similar question is told where to read.
/// Returns how many were learned.
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
    // Code the session touched after the search: reads (with their range)
    // and edits, oldest first, notes and memorycastle's own files left out.
    let mut touched = conn.prepare_cached(
        "SELECT path, start, end FROM (
             SELECT at, path, start, end FROM reads WHERE project_id = ?1 AND session = ?2 AND at >= ?3 AND at <= ?4
             UNION ALL SELECT at, path, 0, 0 FROM edits WHERE project_id = ?1 AND session = ?2 AND at >= ?3 AND at <= ?4
         ) WHERE path NOT LIKE 'memorycastle/%' ORDER BY at LIMIT 40",
    )?;
    let cover = Coverage::load(conn, project_id)?;
    let mut learned = 0;
    for (id, at, session, query, printed, space_id, vec) in rows {
        conn.execute("UPDATE searches SET done = 1 WHERE id = ?1", [id])?;
        let (Some(session), Some(space_id), Some(vec)) = (session, space_id, vec) else { continue };
        let printed: Vec<i64> = printed.split(',').filter_map(|p| p.parse().ok()).collect();
        let code: Vec<(String, u32, u32)> = touched
            .query_map(params![project_id, session, at + OPEN_MIN_SECS, at + LEARN_WINDOW_SECS], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let read_note = opened.query_row(params![project_id, session, at + OPEN_MIN_SECS, at + LEARN_WINDOW_SECS], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).optional()?;
        let (note, kind) = match read_note {
            Some((note, when)) => {
                if nudged.exists(params![project_id, session, note, at, when])? || written.exists(params![note, at])? {
                    continue;
                }
                let kind = match printed.iter().position(|&p| p == note) {
                    Some(0) => "confirm",
                    Some(r) if r < 3 => "lift",
                    _ => "correction",
                };
                (note, kind)
            }
            None => {
                // The first file touched that a printed note covers, if it
                // is specific; the best-ranked covering note gets it.
                let Some(note) = code.iter().find_map(|(path, a, b)| {
                    let (reach, by_def) = cover.covering(path, *a, *b);
                    if cover.specificity(reach.len()) < crate::limits::LEARN_FILE_MIN_SPECIFIC {
                        return None;
                    }
                    let mut shown = printed.iter().copied().filter(|n| reach.contains(n));
                    let first = shown.next()?;
                    // A definition read goes to the best-ranked note naming
                    // it; a whole-file read only when one printed note covers
                    // the file, since then nothing says which one answered.
                    (by_def || shown.next().is_none()).then_some(first)
                }) else {
                    continue;
                };
                if written.exists(params![note, at])? {
                    continue;
                }
                (note, "code")
            }
        };
        let span = code.iter().find(|(path, a, b)| *a > 0 && cover.covering(path, *a, *b).0.contains(&note)).cloned();
        add_with_span(conn, project_id, note, &query, &vec, space_id, kind, span.as_ref())?;
        learned += 1;
    }
    Ok(learned)
}

/// Which notes cover what was read: for a range inside definitions some
/// note names, the notes naming them (a read of one constant in a file 45
/// notes name is about the notes naming that constant); otherwise the
/// notes naming the file or a symbol defined in it. Loaded once per `derive`.
struct Coverage {
    files: Vec<(i64, String)>,
    syms: HashMap<String, Vec<i64>>,
    notes: usize,
    defs: HashMap<String, Vec<(String, u32, u32)>>,
}

impl Coverage {
    fn load(conn: &Connection, project_id: i64) -> Result<Coverage> {
        let mut c = Coverage { files: Vec::new(), syms: HashMap::new(), notes: 0, defs: HashMap::new() };
        c.notes = conn.query_row("SELECT COUNT(*) FROM notes WHERE project_id = ?1 AND deleted_at IS NULL", [project_id], |r| r.get::<_, i64>(0))? as usize;
        let mut stmt = conn.prepare_cached(
            "SELECT l.note_id, l.kind, l.target FROM links l JOIN notes n ON n.id = l.note_id AND n.deleted_at IS NULL WHERE l.project_id = ?1 AND l.kind IN (0, 2)",
        )?;
        for row in stmt.query_map([project_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))? {
            let (note, kind, target) = row?;
            if kind == 0 {
                c.files.push((note, target));
            } else {
                let bare = target.rsplit("::").next().unwrap_or(&target).rsplit('.').next().unwrap_or(&target).trim_end_matches("()").to_string();
                c.syms.entry(bare).or_default().push(note);
            }
        }
        let mut defs = conn.prepare_cached("SELECT path, name, line, end_line FROM code_defs WHERE project_id = ?1")?;
        for row in defs.query_map([project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?, r.get::<_, u32>(3)?)))? {
            let (path, name, a, b) = row?;
            c.defs.entry(path).or_default().push((name, a, b));
        }
        Ok(c)
    }

    /// Notes covering lines `a..=b` of `path` (`a` 0: the whole file, an
    /// edit), and whether a definition in the range decided it.
    fn covering(&self, path: &str, a: u32, b: u32) -> (Vec<i64>, bool) {
        let named = |pick: &dyn Fn(u32, u32) -> bool| -> Vec<i64> {
            let mut out: Vec<i64> = Vec::new();
            for (name, x, y) in self.defs.get(path).into_iter().flatten() {
                if pick(*x, *y) {
                    out.extend(self.syms.get(name).into_iter().flatten());
                }
            }
            out
        };
        let mut out = if a > 0 { named(&|x, y| x <= b && y >= a) } else { Vec::new() };
        let by_def = !out.is_empty();
        if out.is_empty() {
            out = self.files.iter().filter(|(_, f)| crate::graph::same_file(f, path)).map(|(n, _)| *n).collect();
            out.extend(named(&|_, _| true));
        }
        out.sort_unstable();
        out.dedup();
        (out, by_def)
    }

    /// `ln(notes / reach) / ln(notes)`: 1 for what one note covers, 0 for
    /// what every note covers (as the working set discounts hub files).
    fn specificity(&self, reach: usize) -> f32 {
        if reach == 0 || self.notes < 2 {
            return 0.0;
        }
        ((self.notes as f32 / reach as f32).ln() / (self.notes as f32).ln()).clamp(0.0, 1.0)
    }
}

/// Stores one learned question for a note (vector as little-endian f32),
/// keeping `LEARNED_PER_NOTE` per note: corrections and lifts first, then
/// code-read credits, then confirmations, newest first.
pub fn add(conn: &Connection, project_id: i64, note_id: i64, query: &str, vec: &[u8], space_id: i64, kind: &str) -> Result<()> {
    add_with_span(conn, project_id, note_id, query, vec, space_id, kind, None)
}

/// As `add`, with the code range the asker read next, when there was one.
#[allow(clippy::too_many_arguments)]
pub fn add_with_span(conn: &Connection, project_id: i64, note_id: i64, query: &str, vec: &[u8], space_id: i64, kind: &str, span: Option<&(String, u32, u32)>) -> Result<()> {
    let (path, a, b) = span.map_or((None, 0, 0), |(p, a, b)| (Some(p.as_str()), *a, *b));
    conn.execute(
        "INSERT INTO learned(project_id, note_id, query, vec, space_id, at, kind, words, bits, path, start, end) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(note_id, query) DO UPDATE SET at = excluded.at, vec = excluded.vec, space_id = excluded.space_id, bits = excluded.bits,
             path = COALESCE(excluded.path, path), start = CASE WHEN excluded.path IS NULL THEN start ELSE excluded.start END, end = CASE WHEN excluded.path IS NULL THEN end ELSE excluded.end END",
        params![project_id, note_id, query, vec, space_id, crate::sys::now(), kind, words_of(query), bits_of_blob(vec), path, a, b],
    )?;
    conn.execute(
        "DELETE FROM learned WHERE note_id = ?1 AND id NOT IN (SELECT id FROM learned WHERE note_id = ?1 ORDER BY CASE kind WHEN 'confirm' THEN 2 WHEN 'code' THEN 1 ELSE 0 END, at DESC, id DESC LIMIT ?2)",
        params![note_id, crate::limits::LEARNED_PER_NOTE as i64],
    )?;
    Ok(())
}

/// `memcastle learned add`: a question embedded now, for a note.
pub fn add_vector(conn: &Connection, project_id: i64, note_id: i64, query: &str, qvec: &[f32], space_id: i64) -> Result<()> {
    add(conn, project_id, note_id, query, &to_bytes(qvec), space_id, "manual")
}

/// What learned questions within `LEARNED_MIN_COSINE` of the question
/// point at: their notes, best first, as (note id, cosine), and the code
/// ranges their askers read next, best first. A superseded note's questions
/// point at the note that replaced it.
#[derive(Default)]
pub struct Matches {
    pub notes: Vec<(i64, f32)>,
    pub spans: Vec<(String, u32, u32)>,
}

pub fn matches(conn: &Connection, project_ids: &[i64], space_id: i64, qvec: &[f32]) -> Result<Matches> {
    let mut out: Vec<(i64, f32)> = Vec::new();
    let mut spans: Vec<(f32, String, u32, u32)> = Vec::new();
    // Sign bits first (64 bytes a row), the exact cosine only for rows within
    // `LEARNED_BITS_MAX` differing bits: every pair at cosine 0.86 or more
    // on the gold questions was within 133, and 0.5% of all pairs pass 140.
    let qbits = sign_bits(qvec.iter().copied());
    let mut stmt = conn.prepare_cached("SELECT id, bits FROM learned WHERE project_id = ?1 AND space_id = ?2")?;
    let mut exact = conn.prepare_cached(
        "SELECT l.note_id, l.vec, n.project_id, n.path, n.superseded_by, l.path, l.start, l.end FROM learned l JOIN notes n ON n.id = l.note_id AND n.deleted_at IS NULL WHERE l.id = ?1",
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
        type Row = (i64, Vec<u8>, i64, String, Option<String>, Option<String>, u32, u32);
        for id in near {
            let Some((note, blob, project, path, superseded_by, code, a, b)) =
                exact.query_row([id], |r| Ok::<Row, _>((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).optional()?
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
            if let Some(code) = code.filter(|_| a > 0) {
                spans.push((cos, code, a, b));
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
    spans.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut seen = std::collections::HashSet::new();
    let spans = spans.into_iter().filter(|(_, p, a, b)| seen.insert((p.clone(), *a, *b))).map(|(_, p, a, b)| (p, a, b)).collect();
    Ok(Matches { notes: out, spans })
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

/// `links <file>`: what earlier questions led their askers to read in this
/// file, as one line, newest question quoted; `None` when nothing did.
pub fn asked_line(conn: &Connection, project_id: i64, target: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT query, path, start, end FROM learned WHERE project_id = ?1 AND path IS NOT NULL ORDER BY at DESC")?;
    let rows: Vec<(String, String, u32, u32)> = stmt
        .query_map([project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?, r.get::<_, u32>(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, p, _, _)| crate::graph::same_file(target, p.as_str()))
        .collect();
    let Some((q, p, a, b)) = rows.first() else { return Ok(None) };
    let more = if rows.len() > 1 { format!("{} questions led here; newest", rows.len()) } else { "1 question led here".to_string() };
    Ok(Some(format!("   asked here  {more}: \"{q}\", read {p}:{a}-{b}\n")))
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
