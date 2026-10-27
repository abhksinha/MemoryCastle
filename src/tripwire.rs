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
    // Language keywords and literals are in every edit; a note naming one
    // (`true` as an example of a false trip) is not rejecting it.
    const KEYWORDS: &[&str] = &["true", "false", "null", "none", "self", "this", "else", "return", "match", "loop", "while", "async", "await", "const", "static", "struct", "class", "impl", "enum", "type", "void", "string", "split", "trim", "push", "join", "some", "unwrap", "clone", "into"];
    if name.len() < 4 || KEYWORDS.contains(&name.to_ascii_lowercase().as_str()) {
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
    let mut notes = graph::with_successors(conn, project_id, notes)?;
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
    let tick = format!("`{name}`");
    rejected_line_where(conn, note_id, |l| l.contains(&tick))
}

/// The first Rejected line `hit` accepts, clipped to `TRIP_LINE_CHARS`.
fn rejected_line_where(conn: &Connection, note_id: i64, hit: impl Fn(&str) -> bool) -> Result<(u32, String)> {
    let content: String = conn.query_row("SELECT content FROM notes WHERE id = ?1", [note_id], |r| r.get(0))?;
    let mut in_rejected = false;
    for (i, l) in content.lines().enumerate() {
        if let Some(h) = l.strip_prefix("## ") {
            in_rejected = h.trim().eq_ignore_ascii_case("rejected");
            continue;
        }
        if in_rejected && hit(l) {
            let t = l.trim().trim_start_matches("- ").to_string();
            let t = if t.chars().count() > limits::TRIP_LINE_CHARS { format!("{}...", t.chars().take(limits::TRIP_LINE_CHARS).collect::<String>()) } else { t };
            return Ok(((i + 1) as u32, t));
        }
    }
    Ok((0, String::new()))
}

/// The added text without whole-line comments: a comment that names a
/// rejected approach describes it, it does not bring it back.
/// A number the edit adds that a Rejected line of a note about the edited
/// definition states: a value tried and dropped (`0.40` for the floor,
/// batches of `16`), which no backticked name catches. Only notes naming the
/// definition holding line `at`: over every note on the file, broad notes
/// full of numbers matched by chance. 2 of 7 such edits caught on 36
/// labelled ones, no false alarm (2026-10-27-local-stand-ins-for-jev.md).
/// `as_of` (replay): only notes whose day ended before it.
pub fn check_numbers(conn: &Connection, project_id: i64, rel: &str, at: u32, before: &str, added: &str, as_of: Option<i64>) -> Result<Vec<Trip>> {
    let added = &code_lines(added);
    if added.trim().is_empty() || rel.starts_with("memorycastle/") || !is_code_path(rel) {
        return Ok(Vec::new());
    }
    let Some(def) = crate::code::def_enclosing(conn, project_id, rel, at)? else { return Ok(Vec::new()) };
    let mut notes = graph::with_successors(conn, project_id, graph::notes_for_def(conn, project_id, &def)?)?;
    notes.retain(|n| n.superseded_by.is_none() && as_of.is_none_or(|t| n.dated.unwrap_or(n.saved_at) + 86_400 <= t));
    let (now, was) = (numbers(added), numbers(before));
    let mut trips: Vec<Trip> = Vec::new();
    for n in notes {
        let Some(rejected) = store::section_body(conn, n.id, "Rejected")? else { continue };
        let kept = numbers(&store::section_body(conn, n.id, "Decision")?.unwrap_or_default());
        for num in values(&rejected) {
            if !now.contains(&num) || was.contains(&num) || kept.contains(&num) || trips.iter().any(|t| t.name == num) {
                continue;
            }
            let (line, text) = rejected_line_where(conn, n.id, |l| values(l).contains(&num))?;
            trips.push(Trip { name: num, note: n.path.clone(), line, text });
        }
    }
    Ok(trips)
}

/// The numbers written in `text` as tokens (`16`, `0.40`, not `f16` or the
/// `4` in `v0.4`), leaving out 0, 1 and 2, which every file holds.
fn numbers(text: &str) -> std::collections::HashSet<String> {
    numbers_where(text, false)
}

/// The numbers a Rejected line states as values: not a count followed by a
/// word (`25 rushtex functions`, `4 to 8`), which a replay of this repo's
/// commits matched against `CODE_VERSION` 25.
fn values(text: &str) -> std::collections::HashSet<String> {
    numbers_where(text, true)
}

fn numbers_where(text: &str, values_only: bool) -> std::collections::HashSet<String> {
    let b = text.as_bytes();
    let mut out = std::collections::HashSet::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.')) {
            let start = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
                i += 1;
            }
            let after = b.get(i).copied().unwrap_or(b' ');
            let count = values_only && after == b' ' && b.get(i + 1).is_some_and(u8::is_ascii_alphabetic);
            if !(after.is_ascii_alphabetic() || after == b'_' || count) {
                let n = &text[start..i];
                if !matches!(n, "0" | "1" | "2") {
                    out.insert(n.to_string());
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

fn code_lines(added: &str) -> String {
    added
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            // Comments, and assertions: a test names what it rules out
            // (`assert_eq!(cue("instead of porter"), ..)`).
            !(t.starts_with("//") || t.starts_with('#') && !t.starts_with("#!") && !t.starts_with("#[") || t.starts_with("--") || t.starts_with("/*") || t.starts_with('*') || t.starts_with("assert") || t.starts_with("expect("))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Prose and test files do not bring an approach back: `CLAUDE.md`
/// describing a rejected name, or a test asserting it stays out.
fn is_code_path(rel: &str) -> bool {
    let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let doc = ["md", "markdown", "txt", "rst", "adoc", "org"].contains(&ext.as_str());
    let test = rel.starts_with("tests/") || rel.contains("/tests/") || rel.contains("/test/") || rel.contains("__tests__/") || name.starts_with("test_") || name.contains("_test.") || name.contains(".test.") || name.contains(".spec.");
    !doc && !test
}

/// When a note became known, for a replay: the time of the commit that
/// added it. Live, every saved note counts.
pub type Known<'a> = Option<&'a std::collections::HashMap<String, i64>>;

/// The file as this session last saw it, kept in the temp dir per
/// (session, path): the before for a shell edit. HEAD is wrong when the
/// checkout already holds uncommitted work: every line added earlier read as
/// added by this command, and a name the file already had tripped.
fn snapshot_path(session: &str, rel: &str) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(format!("{session}\0{rel}").as_bytes());
    let name: String = h.iter().take(12).map(|b| format!("{b:02x}")).collect();
    std::env::temp_dir().join("memorycastle-snap").join(name)
}

pub fn read_snapshot(session: &str, rel: &str) -> Option<String> {
    std::fs::read_to_string(snapshot_path(session, rel)).ok()
}

/// Kept only for files the code index would read (`code::MAX_FILE_BYTES`).
pub fn write_snapshot(session: &str, rel: &str, text: &str) {
    if text.len() as u64 > crate::code::MAX_FILE_BYTES {
        return;
    }
    let p = snapshot_path(session, rel);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, text);
}

/// Lines of `after` that `before` does not hold, by trimmed text.
pub fn added_lines(before: &str, after: &str) -> String {
    let had: std::collections::HashSet<&str> = before.lines().map(str::trim).collect();
    after.lines().filter(|l| !had.contains(l.trim())).collect::<Vec<_>>().join("\n")
}

/// The trips for an edit of `rel`: `before` is the file as it was, `added`
/// the text the edit brings. With `as_of` (replay), a note counts only when
/// `known` says it existed by then, and a reversal only when dated by then.
pub fn check(conn: &Connection, project_id: i64, rel: &str, before: &str, added: &str, as_of: Option<i64>, known: Known) -> Result<Vec<Trip>> {
    let added = &code_lines(added);
    if added.trim().is_empty() || rel.starts_with("memorycastle/") || !is_code_path(rel) {
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

    #[test]
    fn numbers_are_whole_tokens() {
        let n = numbers("- 0.40: loses 4 to 8 answers; f16, v0.4, x2, 16_000 and batches of 16.");
        let mut v: Vec<_> = n.into_iter().collect();
        v.sort();
        assert_eq!(v, ["0.40", "16", "4", "8"]);
        let mut v: Vec<_> = values("- 0.40: loses 4 to 8 answers; 25 rushtex functions; batches of 2, 4, 8 and 16.").into_iter().collect();
        v.sort();
        assert_eq!(v, ["0.40", "16", "4"]);
    }
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
    fn docs_tests_and_assertions_do_not_trip() {
        assert!(!is_code_path("CLAUDE.md") && !is_code_path("tests/store.rs") && !is_code_path("web/src/a.test.ts"));
        assert!(is_code_path("src/search.rs") && is_code_path("doc/cite.tex"));
        assert_eq!(code_lines("assert!(x(\"porter\"));\nlet t = \"porter\";"), "let t = \"porter\";");
    }

    #[test]
    fn comments_do_not_trip() {
        assert_eq!(code_lines("# sed -i, a script\nx = 1\n    // porter\n#[derive(Debug)]"), "x = 1\n#[derive(Debug)]");
    }

    #[test]
    fn shell_edits_diff_against_the_session_snapshot() {
        let sid = format!("t{}", std::process::id());
        // The checkout already holds `inJustDecodeBounds` from earlier work;
        // the command only adds `scale`.
        write_snapshot(&sid, "a.kt", "fun f() {\n    o.inJustDecodeBounds = true\n}\n");
        let now = "fun f() {\n    o.inJustDecodeBounds = true\n    val scale = 2\n}\n";
        let snap = read_snapshot(&sid, "a.kt").unwrap();
        assert_eq!(added_lines(&snap, now), "    val scale = 2");
        assert!(!holds(&added_lines(&snap, now), "inJustDecodeBounds"));
        let _ = std::fs::remove_file(snapshot_path(&sid, "a.kt"));
    }

    #[test]
    fn backticks_skip_notes_and_placeholders() {
        let b = backticked("- `porter` tokenizer; see `2026-10-20-x.md`; `<file>`; `ab`; `WITHOUT ROWID`");
        assert_eq!(b, vec!["porter".to_string(), "WITHOUT ROWID".to_string()]);
    }
}
