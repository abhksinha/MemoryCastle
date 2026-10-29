//! A note drafted from the ledger when a session that edited code ends
//! without saving one: `Files:` filled from the edits table, `Author:` and
//! `Date:` from the session, sections empty with one-line prompts. Turning
//! "write a note" into "fill two sections" is the cheapest fix for the
//! weakest link, note discipline.

use crate::{store, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

/// The placeholder title; `lint` refuses it.
pub const PLACEHOLDER_TITLE: &str = "<state the decision in one line>";

/// True when the note's title is still the draft placeholder.
pub fn is_unfilled(content: &str) -> bool {
    content.lines().find(|l| l.starts_with("# ")).is_some_and(|l| l[2..].trim() == PLACEHOLDER_TITLE)
}

/// What a session owes: code edited after the last note it saved.
#[derive(Debug, Clone)]
pub struct Owed {
    /// Files edited since the last save, outside `memorycastle/`, sorted.
    pub files: Vec<String>,
    /// The last note saved in this session, when there is one.
    pub last_note: Option<(String, i64)>,
    /// When the oldest owed edit was made.
    pub first_edit: i64,
}

/// The edits this session made after its last saved note. None when the
/// session edited nothing outside `memorycastle/` since then: the hooks
/// ask at compaction, after `git commit`, and when the agent stops.
pub fn owed(conn: &Connection, project_id: i64) -> Result<Option<Owed>> {
    let Some(session) = store::current_session(conn, project_id)? else { return Ok(None) };
    let started = store::session_started(conn, project_id)?.unwrap_or(0);
    let last_note: Option<(String, i64)> = conn
        .query_row(
            "SELECT path, saved_at FROM notes WHERE project_id = ?1 AND saved_at >= ?2 AND deleted_at IS NULL ORDER BY saved_at DESC LIMIT 1",
            params![project_id, started],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    let since = last_note.as_ref().map(|n| n.1).unwrap_or(started);
    // After a note, strictly after: a shell edit is recorded at the file's
    // mtime, and one written in the same second as the note belongs to it.
    // From the session start, the same second counts.
    let from = if last_note.is_some() { since + 1 } else { since };
    let mut stmt = conn.prepare_cached("SELECT path, MIN(at) FROM edits WHERE project_id = ?1 AND session = ?2 AND at >= ?3 GROUP BY path ORDER BY path")?;
    let rows: Vec<(String, i64)> = stmt.query_map(params![project_id, session, from], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let rows: Vec<(String, i64)> = rows.into_iter().filter(|(p, _)| !p.starts_with("memorycastle/")).collect();
    if rows.is_empty() {
        return Ok(None);
    }
    let first_edit = rows.iter().map(|r| r.1).min().unwrap_or(0);
    Ok(Some(Owed { files: rows.into_iter().map(|r| r.0).collect(), last_note, first_edit }))
}

/// The last note this session saved, when its `Files:` line leaves out
/// files edited for it (since the note before it): the note path and the
/// files. `save` warns about the same thing; the stop hook holds on it.
pub fn uncovered(conn: &Connection, project_id: i64, root: &Path) -> Result<Option<(String, Vec<String>)>> {
    let started = store::session_started(conn, project_id)?.unwrap_or(0);
    let Ok(note): rusqlite::Result<String> = conn.query_row(
        "SELECT path FROM notes WHERE project_id = ?1 AND saved_at >= ?2 AND deleted_at IS NULL ORDER BY saved_at DESC LIMIT 1",
        params![project_id, started],
        |r| r.get(0),
    ) else {
        return Ok(None);
    };
    let Ok(content) = std::fs::read_to_string(root.join(&note)) else { return Ok(None) };
    let missing = store::files_missing_from_note(conn, project_id, &note, &content)?;
    Ok(if missing.is_empty() { None } else { Some((note, missing)) })
}

/// Writes the draft and returns its path, or None when nothing is owed.
/// When a note or draft with today's name exists already, its path is
/// returned so the hook can say "append to".
pub fn write(conn: &Connection, project_id: i64, root: &Path) -> Result<Option<PathBuf>> {
    let Some(Owed { files: edited, .. }) = owed(conn, project_id)? else { return Ok(None) };
    let today = crate::render::fmt_date(crate::sys::now());
    let topic = branch(root).unwrap_or_else(|| "session".into());
    let rel = format!("memorycastle/journal/{today}-{topic}.md");
    let path = root.join(&rel);
    if path.exists() {
        return Ok(Some(path));
    }
    let author = match (store::git_user(root), store::current_agent(conn, project_id)?) {
        (Some(u), Some(a)) => format!("Author: {u}+{a} | Date: {today}\n"),
        _ => format!("Date: {today}\n"),
    };
    let files: Vec<String> = edited.iter().map(|p| format!("`{p}`")).collect();
    let text = format!(
        "# {PLACEHOLDER_TITLE}\n{author}Files: {}\n\n## Decision\n<One sentence under 200 characters: what was chosen, and why.> <Details after it.>\n\n## Rejected\n- <approach>: <the reason it failed>\n\n## Measured\n- <what>: <before> to <after> <unit>, on <what>\n\n## Open\n- <what the next agent should do first>\n",
        files.join(", ")
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text)?;
    Ok(Some(path))
}

/// The checked-out branch as a file-name topic; None on main or master.
fn branch(root: &Path) -> Option<String> {
    let head = std::fs::read_to_string(root.join(".git/HEAD")).ok()?;
    let name = head.trim().strip_prefix("ref: refs/heads/")?;
    if name == "main" || name == "master" {
        return None;
    }
    let topic: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let topic = topic.trim_matches('-').chars().take(40).collect::<String>();
    if topic.is_empty() {
        None
    } else {
        Some(topic)
    }
}

/// The functions this note's task changed that the note does not name, most
/// changed lines first, at most `SAVE_UNNAMED_MAX`: a note that names a
/// function in backticks is what `links <function>` and the edit gate find,
/// and notes named 15% of the functions their commit changed (2026-10-22).
/// Changed lines are the working tree against the last commit before the
/// previous note (or the session start); tests are left out.
pub fn unnamed(conn: &Connection, project_id: i64, root: &Path, rel_path: &str, content: &str) -> Result<Vec<(String, usize)>> {
    let (since, edited) = store::edited_for_note(conn, project_id, rel_path)?;
    let code: Vec<String> = edited.into_iter().filter(|f| !is_test_path(f)).collect();
    if code.is_empty() {
        return Ok(Vec::new());
    }
    let from = if since > 0 { since } else { store::session_started(conn, project_id)?.unwrap_or(0) };
    let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(root).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let base = git(&["rev-list", "-1", &format!("--before={from}"), "HEAD"]);
    let base = base.trim();
    let base = if base.is_empty() { "HEAD" } else { base };
    let named: Vec<String> = crate::graph::extract("", content).into_iter().filter(|(k, _)| *k == crate::graph::Kind::Symbol).map(|(_, v)| v).collect();
    let mut lines: Vec<(String, usize)> = Vec::new();
    for f in &code {
        let Ok(text) = std::fs::read_to_string(root.join(f)) else { continue };
        let defs = crate::code::definitions(f, &text);
        let src: Vec<&str> = text.lines().collect();
        let diff = git(&["diff", "-U0", base, "--", f]);
        for h in diff.lines().filter(|l| l.starts_with("@@")) {
            let Some(plus) = h.split(' ').find(|p| p.starts_with('+')) else { continue };
            let mut it = plus[1..].split(',');
            let start: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let len: u32 = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
            for ln in start..start + len.max(1) {
                let Some(d) = defs.iter().filter(|d| d.kind == "fn" && d.line <= ln && ln <= d.end).max_by_key(|d| d.line) else { continue };
                // A test: `#[test]`, `@Test` or `def test_` just above or on the line.
                let above = (d.line.saturating_sub(3)..d.line).filter_map(|i| src.get(i as usize)).any(|l| l.contains("#[test]") || l.contains("@Test"));
                if above || d.name.starts_with("test") || named.iter().any(|n| crate::graph::same_symbol(n, &d.name)) {
                    continue;
                }
                match lines.iter_mut().find(|(n, _)| *n == d.name) {
                    Some(e) => e.1 += 1,
                    None => lines.push((d.name.clone(), 1)),
                }
            }
        }
    }
    lines.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    lines.truncate(crate::limits::SAVE_UNNAMED_MAX);
    Ok(lines)
}

fn is_test_path(f: &str) -> bool {
    f.starts_with("tests/") || f.contains("/tests/") || f.contains("/test/") || f.contains("_test.") || f.contains("Test.")
}
