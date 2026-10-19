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
    let mut stmt = conn.prepare_cached("SELECT path, MIN(at) FROM edits WHERE project_id = ?1 AND session = ?2 AND at >= ?3 GROUP BY path ORDER BY path")?;
    let rows: Vec<(String, i64)> = stmt.query_map(params![project_id, session, since], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let rows: Vec<(String, i64)> = rows.into_iter().filter(|(p, _)| !p.starts_with("memorycastle/")).collect();
    if rows.is_empty() {
        return Ok(None);
    }
    let first_edit = rows.iter().map(|r| r.1).min().unwrap_or(0);
    Ok(Some(Owed { files: rows.into_iter().map(|r| r.0).collect(), last_note, first_edit }))
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
        "# {PLACEHOLDER_TITLE}\n{author}Files: {}\n\n## Context\n<why this task existed>\n\n## Decision\n<what was chosen and why>\n\n## Rejected\n<what was tried or considered and dropped, and why>\n\n## Measured\n<numbers, before and after>\n\n## Open\n<what remains>\n",
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
