//! The edit gate: the first edit of a file in a session gets what `links`
//! knows about it, so the lookup happens whether or not the agent
//! remembered it. As context by default; with `MEMORYCASTLE_GATE=block` the
//! edit is stopped once with it and the retry passes. A per-session record (`meta` key
//! `gate:<session>`, one path per line, `trip:` entries for tripwires shown) holds
//! the files already looked up: the gate adds to it, and so does any
//! `links` call on a file or symbol. Compaction clears it (`gate --reset`),
//! since the agent loses track of what it looked up with the summary.
//!
//! A write that creates a file runs a search on its path instead (`links`
//! has nothing to say yet): shown as context, and a block when the top hit is
//! at least `GATE_SEARCH_BLOCK` (when blocking is on).

use crate::{db, limits, store, Result};
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

fn key(session: &str) -> String {
    format!("gate:{session}")
}

/// What the gate has recorded for this session; empty with no session.
pub fn seen(conn: &Connection, project_id: i64) -> Result<HashSet<String>> {
    let Some(sid) = store::current_session(conn, project_id)? else { return Ok(HashSet::new()) };
    Ok(db::meta_get(conn, &key(&sid))?.unwrap_or_default().lines().map(str::to_string).collect())
}

/// Adds paths (or `dir:` entries) to this session's record.
pub fn mark(conn: &Connection, project_id: i64, items: &[String]) -> Result<()> {
    if items.is_empty() || db::is_readonly(conn) {
        return Ok(());
    }
    let Some(sid) = store::current_session(conn, project_id)? else { return Ok(()) };
    let mut have = seen(conn, project_id)?;
    let before = have.len();
    have.extend(items.iter().cloned());
    if have.len() == before {
        return Ok(());
    }
    let mut v: Vec<&String> = have.iter().collect();
    v.sort();
    db::meta_set(conn, &key(&sid), &v.into_iter().cloned().collect::<Vec<_>>().join("\n"))
}

/// Forgets this session's record (after compaction).
pub fn reset(conn: &Connection, project_id: i64) -> Result<()> {
    let Some(sid) = store::current_session(conn, project_id)? else { return Ok(()) };
    conn.execute("DELETE FROM meta WHERE key = ?1", [key(&sid)])?;
    Ok(())
}

/// True when the gate stops the first edit (`MEMORYCASTLE_GATE=block`);
/// otherwise the same answer goes to the agent as context.
pub fn blocking() -> bool {
    std::env::var("MEMORYCASTLE_GATE").is_ok_and(|v| v == "block")
}

/// The confidence of the first hit in a search answer (`1. path:line   0.62   section`).
pub fn top_confidence(out: &str) -> Option<f32> {
    let line = out.lines().find(|l| l.starts_with("1. "))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

/// True when a `links` answer holds more than "no notes link to X": notes,
/// or what the code index knows about the file.
fn informative(out: &str) -> bool {
    let out = out.trim();
    !out.is_empty() && (!out.starts_with("no notes link to") || out.lines().count() > 1)
}

/// What a Claude Code PreToolUse event says about an edit.
pub struct Edit {
    pub file: String,
    pub at: Option<u32>,
    pub new_text: Option<String>,
}

/// Reads an Edit, Write or MultiEdit event: the file, the line the first
/// `old_string` starts on, and all the new text.
pub fn claude_edit(json: &str) -> Option<Edit> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    // The event's own session, for the record and the ledger.
    if let Some(sid) = v["session_id"].as_str().filter(|s| !s.is_empty()) {
        std::env::set_var("MEMORYCASTLE_SESSION", sid);
    }
    let t = &v["tool_input"];
    let file = t["file_path"].as_str()?.to_string();
    let mut olds: Vec<&str> = Vec::new();
    let mut news: Vec<&str> = Vec::new();
    for e in std::iter::once(t).chain(t["edits"].as_array().into_iter().flatten()) {
        olds.extend(e["old_string"].as_str());
        news.extend(e["new_string"].as_str().or(e["content"].as_str()));
    }
    let at = olds.first().and_then(|o| o.lines().next()).filter(|l| !l.trim().is_empty()).and_then(|first| {
        let text = std::fs::read_to_string(&file).ok()?;
        text.lines().position(|l| l.contains(first)).map(|i| i as u32 + 1)
    });
    Some(Edit { file, at, new_text: if news.is_empty() { None } else { Some(news.join("\n")) } })
}

pub enum Outcome {
    Pass,
    Context(String),
    Block(String),
}

fn run(root: &Path, args: &[&str]) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| "memcastle".into());
    Command::new(exe)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("MEMORYCASTLE_VIA", if blocking() { "gateblock" } else { "gate" })
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
        .unwrap_or_default()
}

/// The gate for one file about to be written. `rel` is repo-relative;
/// `at` the line being edited when the tool says; `exists` whether the file
/// is on disk (a write that creates it searches its area).
pub fn check(conn: &Connection, project_id: i64, root: &Path, rel: &str, at: Option<u32>, exists: bool, new_text: Option<&str>) -> Result<Outcome> {
    if rel.starts_with("memorycastle/") || store::current_session(conn, project_id)?.is_none() {
        return Ok(Outcome::Pass);
    }
    let have = seen(conn, project_id)?;
    let first_file = !have.contains(rel);
    mark(conn, project_id, &[rel.to_string()])?;
    let at_s = at.map(|a| a.to_string());
    let mut block = Vec::new();
    let mut context = Vec::new();
    if first_file && exists {
        // `--once` so the retry does not print the same notes as context;
        // when the after-bash hook already printed them, ask again without.
        let mut args = vec!["links", rel, "--once"];
        if let Some(a) = &at_s {
            args.extend(["--at", a]);
        }
        let mut out = run(root, &args);
        if out.is_empty() {
            args.retain(|a| *a != "--once");
            out = run(root, &args);
        }
        if informative(&out) {
            block.push(out);
        }
    } else if exists {
        // Looked up already: only the notes about the definition being
        // edited, once, as context.
        if let Some(a) = &at_s {
            let out = run(root, &["links", rel, "--at", a, "--once"]);
            if !out.is_empty() && !out.starts_with("no notes link to") {
                context.push(out);
            }
        }
    }
    // Every edit, not only the first: the new text is what can bring back a
    // rejected approach. Each (note, name) once per session.
    if let Some(added) = new_text {
        let before = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
        let trips: Vec<_> = crate::tripwire::check(conn, project_id, rel, &before, added, None, None)?
            .into_iter()
            .filter(|t| !have.contains(&format!("trip:{}:{}", t.note, t.name)))
            .collect();
        if !trips.is_empty() {
            mark(conn, project_id, &trips.iter().map(|t| format!("trip:{}:{}", t.note, t.name)).collect::<Vec<_>>())?;
            block.push(format!("{}\nIf the note's reason no longer holds, say why and supersede the note; otherwise keep the rejected approach out.", crate::tripwire::render(&trips)));
        }
    }
    // Only a new file, where `links` has nothing to say: for an existing
    // file a search on its path repeated the notes `links` printed and
    // added unrelated ones (0.74 on `src/serve.rs`, 0.64 on `src/fts.rs`).
    if !exists {
        let k = limits::GATE_SEARCH_K.to_string();
        let out = run(root, &["search", rel, "-k", &k]);
        // Under `GATE_SEARCH_BLOCK` nothing: a new path's words matched
        // unrelated notes at 0.46 to 0.57 (`src/zz.rs` got `SPAN_WIDE`).
        if top_confidence(&out).is_some_and(|top| top >= limits::GATE_SEARCH_BLOCK) {
            block.push(format!("search \"{rel}\" (first edit in this area; --sections for the full notes):\n{out}"));
        }
    }
    // Context by default; blocking is opt-in (`MEMORYCASTLE_GATE=block`)
    // until the lookups-to-edits ratio shows context alone is not read.
    if !block.is_empty() && !blocking() {
        block.append(&mut context);
        return Ok(Outcome::Context(format!("memorycastle: {rel}; what is known about it (read the ranges that matter):\n{}", block.join("\n"))));
    }
    if !block.is_empty() {
        block.extend(context);
        return Ok(Outcome::Block(format!(
            "memorycastle: {rel} was looked up for you before this edit:\n{}\nRead the ranges and notes that matter, then retry the edit; it passes now.",
            block.join("\n")
        )));
    }
    if !context.is_empty() {
        return Ok(Outcome::Context(format!("memorycastle: {}", context.join("\n"))));
    }
    Ok(Outcome::Pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_top_confidence() {
        let out = "1. memorycastle/journal/a.md:12   0.64   Decision\n   title\n\n2. b.md:3   0.40   Open\n";
        assert_eq!(top_confidence(out), Some(0.64));
        assert_eq!(top_confidence("no relevant note"), None);
    }

    #[test]
    fn links_answer_with_facts_is_informative() {
        assert!(!informative("no notes link to README.md"));
        assert!(informative("no notes link to src/a.rs\n   defines: fn a:1-3"));
        assert!(informative("memorycastle/journal/x.md  Title  2026-10-20  (abhi+claude)"));
    }
}
