//! The rejected-approach tripwire: an edit that introduces a name a note
//! rejected, in a file that note is linked to, is told so before it lands.
//! Deterministic and on the keyword path: the graph picks the notes (their
//! `Files:` and symbol links), the note's `## Rejected` section supplies the
//! names (backticked), and the check is a string match against the edit's
//! new text and the file as it was.
//!
//! A name fires only when all hold: it is backticked in the note's Rejected
//! section and not in its Decision; it is distinctive (a structural
//! character, or in at most `DISTINCTIVE_MAX_SHARE` of the project's
//! chunks); the edit's new text holds it and the file before did not; and
//! no later note's Decision names it (a reversal, recorded, is not a trip).

use crate::graph::{self, Kind};
use crate::{limits, store, Result};
use rusqlite::{params, Connection};

#[derive(Debug, Clone)]
pub struct Trip {
    pub name: String,
    pub note: String,
    pub line: u32,
    /// The Rejected line that names it, trimmed.
    pub text: String,
}

/// Backticked names in a section body: identifiers, flags, short phrases.
fn backticked(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, part) in body.split('`').enumerate() {
        let p = part.trim();
        if i % 2 == 1 && (3..=40).contains(&p.len()) && !p.ends_with(".md") && !p.contains('<') && p.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_') && !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// True when `name` occurs in `text` as a whole name (identifier
/// boundaries on both sides when the name starts or ends with a word character).
pub fn holds(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let first_word = name.chars().next().is_some_and(word);
    let last_word = name.chars().last().is_some_and(word);
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let at = from + i;
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        if !(first_word && before.is_some_and(word)) && !(last_word && after.is_some_and(word)) {
            return true;
        }
        from = at + name.len().max(1);
    }
    false
}

/// A name with structure (`_`, `::`, `.`, `-`, a space, a digit, an inner
/// capital) is distinctive by its shape; a plain word only when few of the
/// project's chunks hold it, so `porter` counts and `search` does not.
fn distinctive(conn: &Connection, project_id: i64, name: &str) -> Result<bool> {
    let shaped = name.chars().any(|c| matches!(c, '_' | ':' | '.' | '-' | ' ' | '(') || c.is_ascii_digit()) || name.chars().skip(1).any(|c| c.is_uppercase());
    if shaped {
        return Ok(true);
    }
    if name.len() < 4 {
        return Ok(false);
    }
    let total: i64 = conn.query_row("SELECT count(*) FROM chunks WHERE project_id = ?1", [project_id], |r| r.get(0))?;
    let limit = ((total as f32) * limits::DISTINCTIVE_MAX_SHARE).max(1.0) as i64;
    let df: i64 = conn
        .query_row(
            "SELECT count(*) FROM chunks_fts f JOIN chunks c ON c.id = f.rowid WHERE chunks_fts MATCH ?1 AND c.project_id = ?2",
            params![format!("\"{}\"", name.replace('"', "")), project_id],
            |r| r.get(0),
        )
        .unwrap_or(i64::MAX);
    Ok(df <= limit)
}

/// The notes linked to `rel`, or to a definition in it, that are current.
fn notes_about(conn: &Connection, project_id: i64, rel: &str) -> Result<Vec<graph::NoteRef>> {
    let mut notes = graph::notes_for(conn, project_id, Kind::File, rel)?;
    let mut stmt = conn.prepare_cached("SELECT DISTINCT name FROM code_defs WHERE project_id = ?1 AND path = ?2")?;
    let names: Vec<String> = stmt.query_map(params![project_id, rel], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    for n in names {
        for note in graph::notes_for(conn, project_id, Kind::Symbol, &n)? {
            if !notes.iter().any(|m| m.id == note.id) {
                notes.push(note);
            }
        }
    }
    notes.retain(|n| n.superseded_by.is_none());
    Ok(notes)
}

/// Another note dated from `since` to `until` whose Decision names `name`:
/// the rejection was reversed on the record.
fn reversed(conn: &Connection, project_id: i64, note_id: i64, name: &str, since: i64, until: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM sections s JOIN notes n ON n.id = s.note_id
         WHERE n.project_id = ?1 AND n.id != ?2 AND n.deleted_at IS NULL AND s.name = 'Decision'
           AND coalesce(n.dated, n.saved_at) BETWEEN ?3 AND ?4 AND instr(s.body, ?5) > 0",
        params![project_id, note_id, since, until, format!("`{name}`")],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// The Rejected line naming `name`, and its line in the note.
fn rejected_line(conn: &Connection, note_id: i64, name: &str) -> Result<(u32, String)> {
    let content: String = conn.query_row("SELECT content FROM notes WHERE id = ?1", [note_id], |r| r.get(0))?;
    let mut in_rejected = false;
    let tick = format!("`{name}`");
    for (i, l) in content.lines().enumerate() {
        if let Some(h) = l.strip_prefix("## ") {
            in_rejected = h.trim().eq_ignore_ascii_case("rejected");
            continue;
        }
        if in_rejected && l.contains(&tick) {
            let t = l.trim().trim_start_matches("- ").to_string();
            let t = if t.chars().count() > limits::TRIP_LINE_CHARS { format!("{}...", t.chars().take(limits::TRIP_LINE_CHARS).collect::<String>()) } else { t };
            return Ok(((i + 1) as u32, t));
        }
    }
    Ok((0, String::new()))
}

/// The added text without whole-line comments: a comment that names a
/// rejected approach describes it, it does not bring it back.
fn code_lines(added: &str) -> String {
    added
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with("//") || t.starts_with('#') && !t.starts_with("#!") && !t.starts_with("#[") || t.starts_with("--") || t.starts_with("/*") || t.starts_with('*'))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// When a note became known, for a replay: the time of the commit that
/// added it. Live, every saved note counts.
pub type Known<'a> = Option<&'a std::collections::HashMap<String, i64>>;

/// The trips for an edit of `rel`: `before` is the file as it was, `added`
/// the text the edit brings. With `as_of` (replay), a note counts only when
/// `known` says it existed by then, and a reversal only when dated by then.
pub fn check(conn: &Connection, project_id: i64, rel: &str, before: &str, added: &str, as_of: Option<i64>, known: Known) -> Result<Vec<Trip>> {
    let added = &code_lines(added);
    if added.trim().is_empty() || rel.starts_with("memorycastle/") {
        return Ok(Vec::new());
    }
    let mut trips: Vec<Trip> = Vec::new();
    for n in notes_about(conn, project_id, rel)? {
        let dated = n.dated.unwrap_or(n.saved_at);
        if let (Some(t), Some(k)) = (as_of, known) {
            if k.get(&n.path).is_none_or(|added_at| *added_at >= t) {
                continue;
            }
        } else if as_of.is_some_and(|t| dated > t) {
            continue;
        }
        let Some(rejected) = store::section_body(conn, n.id, "Rejected")? else { continue };
        let decision = store::section_body(conn, n.id, "Decision")?.unwrap_or_default();
        for name in backticked(&rejected) {
            if decision.contains(&format!("`{name}`")) || trips.iter().any(|t| t.name == name) {
                continue;
            }
            if !holds(added, &name) || holds(before, &name) || !distinctive(conn, project_id, &name)? {
                continue;
            }
            if reversed(conn, project_id, n.id, &name, dated, as_of.unwrap_or(i64::MAX))? {
                continue;
            }
            let (line, text) = rejected_line(conn, n.id, &name)?;
            trips.push(Trip { name, note: n.path.clone(), line, text });
        }
    }
    Ok(trips)
}

/// The trips as lines for the agent.
pub fn render(trips: &[Trip]) -> String {
    trips
        .iter()
        .map(|t| format!("tripwire: this edit adds `{}`, which {}:{} rejected: {}", t.name, t.note, t.line, t.text))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_whole() {
        assert!(holds("tokenize=\"porter unicode61\"", "porter"));
        assert!(!holds("reporters", "porter"));
        assert!(holds("x | tee out.log", "tee"));
        assert!(!holds("committee", "tee"));
        assert!(holds("sed -i '' s/a/b/", "sed -i"));
    }

    #[test]
    fn comments_do_not_trip() {
        assert_eq!(code_lines("# sed -i, a script\nx = 1\n    // porter\n#[derive(Debug)]"), "x = 1\n#[derive(Debug)]");
    }

    #[test]
    fn backticks_skip_notes_and_placeholders() {
        let b = backticked("- `porter` tokenizer; see `2026-10-20-x.md`; `<file>`; `ab`; `WITHOUT ROWID`");
        assert_eq!(b, vec!["porter".to_string(), "WITHOUT ROWID".to_string()]);
    }
}
