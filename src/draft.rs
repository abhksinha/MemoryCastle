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

/// Writes the draft and returns its path, or None when the session saved a
/// note already, edited nothing outside `memorycastle/`, or a draft exists.
pub fn write(conn: &Connection, project_id: i64, root: &Path) -> Result<Option<PathBuf>> {
    let Some(session) = store::current_session(conn, project_id)? else { return Ok(None) };
    let started = store::session_started(conn, project_id)?.unwrap_or(0);
    let saved: i64 = conn.query_row("SELECT count(*) FROM notes WHERE project_id = ?1 AND saved_at >= ?2 AND deleted_at IS NULL", params![project_id, started], |r| r.get(0))?;
    if saved > 0 {
        return Ok(None);
    }
    let mut stmt = conn.prepare_cached("SELECT DISTINCT path FROM edits WHERE project_id = ?1 AND session = ?2 ORDER BY path")?;
    let edited: Vec<String> = stmt.query_map(params![project_id, session], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?.into_iter().filter(|p| !p.starts_with("memorycastle/")).collect();
    if edited.is_empty() {
        return Ok(None);
    }
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
