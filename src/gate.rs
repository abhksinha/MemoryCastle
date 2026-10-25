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

/// The grep gate: a code search for one name the code index defines is
/// answered once per session with `links <name>` instead (exit 2, the
/// retry passes), because agents grepped beside `links` or instead of it and
/// a line saying the caller list is complete arrived after the grep had run.
/// `MEMORYCASTLE_GREP=off` turns it off.
pub fn check_grep(conn: &Connection, project_id: i64, root: &Path, json: &str) -> Result<Outcome> {
    if std::env::var("MEMORYCASTLE_GREP").is_ok_and(|v| v == "off") || std::env::var_os("MEMORYCASTLE_VIA").is_some() {
        return Ok(Outcome::Pass);
    }
    let Some(sym) = grep_target(json) else { return Ok(Outcome::Pass) };
    if store::current_session(conn, project_id)?.is_none() {
        return Ok(Outcome::Pass);
    }
    let short = sym.rsplit("::").next().unwrap_or(&sym).rsplit('.').next().unwrap_or(&sym);
    if short.len() < crate::code::MIN_CALL_LEN {
        return Ok(Outcome::Pass);
    }
    let defs = crate::code::defs_named(conn, project_id, &sym)?;
    let files: HashSet<&str> = defs.iter().map(|d| d.path.as_str()).collect();
    if files.is_empty() || files.len() > crate::code::PROMISCUOUS_DEFS {
        return Ok(Outcome::Pass);
    }
    // `--once`: empty when this name was looked up this session, by the
    // agent or by this gate, so the retry always passes.
    let out = run(root, &["links", &sym, "--once"]);
    if !informative(&out) {
        return Ok(Outcome::Pass);
    }
    Ok(Outcome::Block(format!(
        "memorycastle answered this search with `links {sym}` (once; the same search runs if you retry it):\n{out}\nThe callers above are every direct call, each with its lines. Grep is still the tool for matches in strings, comments, macros or calls through traits."
    )))
}

/// The one name a Grep tool call or a Bash `grep`/`rg` searches the code
/// for, when the pattern is nothing but that name (optionally qualified,
/// `\b`-bounded or followed by `(`). Sets the event's session.
pub fn grep_target(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if let Some(sid) = v["session_id"].as_str().filter(|s| !s.is_empty()) {
        std::env::set_var("MEMORYCASTLE_SESSION", sid);
    }
    let t = &v["tool_input"];
    match v["tool_name"].as_str()? {
        "Grep" if t["path"].as_str().is_some_and(names_file) => None,
        // Case-insensitive is a text search; context lines ask for the body.
        "Grep" if ["-i", "-A", "-B", "-C", "context"].iter().any(|k| !t[*k].is_null() && t[*k] != false) => None,
        "Grep" => symbol_pattern(t["pattern"].as_str()?),
        "Bash" => bash_grep(t["command"].as_str()?),
        _ => None,
    }
}

/// The pattern of the first `grep`, `rg` or `git grep` that searches files,
/// not a pipe's output; none when the same command runs `links` itself.
fn bash_grep(cmd: &str) -> Option<String> {
    if cmd.contains("memcastle links") || cmd.contains("memorycastle links") {
        return None;
    }
    // A heredoc's body is a script's text, not commands.
    let cmd = match cmd.find("<<") {
        Some(i) => &cmd[..cmd[i..].find('\n').map_or(cmd.len(), |j| i + j)],
        None => cmd,
    };
    let mut piped = false;
    let mut seg = String::new();
    let mut segs = Vec::new();
    let mut chars = cmd.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => {
                quote = None;
                seg.push(c);
            }
            (Some(_), _) => seg.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                seg.push(c);
            }
            (None, ';' | '\n' | '&' | '|') => {
                let pipe = c == '|' && chars.peek() != Some(&'|');
                if matches!(chars.peek(), Some('&' | '|')) {
                    chars.next();
                }
                segs.push((std::mem::take(&mut seg), piped));
                piped = pipe;
            }
            _ => seg.push(c),
        }
    }
    segs.push((seg, piped));
    for (s, piped) in segs {
        let words = shell_words(&s);
        let mut w = words.iter().map(String::as_str).peekable();
        match w.next()? {
            "git" if w.peek() == Some(&"grep") => {
                w.next();
            }
            "grep" | "egrep" | "rg" => {}
            _ => continue,
        }
        if piped {
            continue;
        }
        let takes_value = |f: &str| matches!(f, "-A" | "-B" | "-C" | "-m" | "-g" | "-t" | "-T" | "-f" | "--glob" | "--type" | "--include" | "--exclude" | "--max-count");
        let mut pattern = None;
        while let Some(a) = w.next() {
            if a == "-e" || a == "--regexp" {
                pattern = w.next();
                break;
            }
            if let Some(p) = a.strip_prefix("--regexp=") {
                pattern = Some(p);
                break;
            }
            if a.starts_with('-') {
                if text_search(a) {
                    return None;
                }
                if takes_value(a) {
                    w.next();
                }
                continue;
            }
            pattern = Some(a);
            break;
        }
        // A search inside named files looks for lines there, which
        // `links` does not answer; only a search of the tree is a lookup.
        if w.any(|a| if a.starts_with('-') { text_search(a) } else { names_file(a) || a.starts_with('$') }) {
            return None;
        }
        return symbol_pattern(pattern?);
    }
    None
}

/// A flag that makes a grep a text search, not a name lookup:
/// case-insensitive, or context lines, which ask for the body grep prints and
/// `links` only points at.
fn text_search(flag: &str) -> bool {
    let short = !flag.starts_with("--") && flag[1..].chars().any(|c| matches!(c, 'i' | 'A' | 'B' | 'C'));
    short || matches!(flag, "--ignore-case" | "--context" | "--after-context" | "--before-context") || flag.starts_with("--context=")
}

/// True for a path naming a file (`src/main.rs`, `Cargo.toml`), not a
/// directory (`src/`, `.`).
fn names_file(p: &str) -> bool {
    let last = p.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    !p.ends_with('/') && last.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty() && ext.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// Splits a shell segment into words, quotes removed.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            _ => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A pattern that is only a name: `search`, `\bsearch\b`, `search\(`,
/// `render::search`, `fn search`, `self.search`. Anything else (a regex, two
/// words, a string) is not a name lookup.
fn symbol_pattern(p: &str) -> Option<String> {
    let mut s = p.trim().replace("\\b", "").replace("\\<", "").replace("\\>", "").replace("\\.", ".");
    for pre in ["fn ", "def ", "func ", "function "] {
        if let Some(r) = s.strip_prefix(pre) {
            s = r.to_string();
        }
    }
    let s = s.trim_end_matches("\\(").trim_end_matches('(');
    let s = s.strip_prefix("self.").or_else(|| s.strip_prefix("self::")).unwrap_or(s);
    let ident = |w: &str| w.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let parts: Vec<&str> = s.split("::").flat_map(|x| x.split('.')).collect();
    // `Kind::Variant`: an enum variant is not a definition, and its last
    // part may name an unrelated type.
    let upper = |w: &&str| w.starts_with(|c: char| c.is_ascii_uppercase());
    if parts.len() > 1 && parts.iter().all(upper) {
        return None;
    }
    // `pdftex.map`, `main.rs`: a file name, not a member.
    if s.rsplit_once('.').is_some_and(|(_, ext)| (1..=4).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())) {
        return None;
    }
    (!s.is_empty() && parts.iter().all(|w| ident(w))).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grep_of_one_name_is_a_lookup() {
        let ev = |tool: &str, k: &str, v: &str| serde_json::json!({"tool_name": tool, "tool_input": {k: v}}).to_string();
        assert_eq!(grep_target(&ev("Grep", "pattern", "render::search")).as_deref(), Some("render::search"));
        assert_eq!(grep_target(&ev("Grep", "pattern", "\\bcall_lines\\(")).as_deref(), Some("call_lines"));
        assert_eq!(grep_target(&ev("Grep", "pattern", "fn notes_for_def")).as_deref(), Some("notes_for_def"));
        assert_eq!(grep_target(&ev("Grep", "pattern", "foo|bar")), None);
        assert_eq!(grep_target(&ev("Grep", "pattern", "TODO fix")), None);
        assert_eq!(grep_target(&ev("Bash", "command", "cd x && grep -rn 'call_lines(' src/")).as_deref(), Some("call_lines"));
        assert_eq!(grep_target(&ev("Bash", "command", "rg -n -g '*.rs' site_confidence")).as_deref(), Some("site_confidence"));
        assert_eq!(grep_target(&ev("Bash", "command", "git grep -e defs_named")).as_deref(), Some("defs_named"));
        // Filtering output, and a command that already runs `links`, are not lookups.
        assert_eq!(grep_target(&ev("Bash", "command", "cargo test 2>&1 | grep failed")), None);
        assert_eq!(grep_target(&ev("Bash", "command", "memcastle links x; grep -rn call_lines src")), None);
        assert_eq!(grep_target(&ev("Bash", "command", "ls src || grep -rn xyz")).as_deref(), Some("xyz"));
        assert_eq!(grep_target(&ev("Bash", "command", "python3 - <<'EOF'\nimport os\nos.system('grep -rn call_lines src')\nEOF")), None);
        assert_eq!(grep_target(&ev("Grep", "pattern", "MatrixEnvKind::Matrix")), None);
        assert_eq!(grep_target(&ev("Bash", "command", "grep -rln -i kern layout/")), None);
        assert_eq!(grep_target(&ev("Bash", "command", "grep -rn 'fn soa_ink_extent' -A40 src")), None);
        assert_eq!(grep_target(&ev("Bash", "command", "grep -n last_node_type $f")), None);
        assert_eq!(grep_target(&ev("Grep", "pattern", "pdftex.map")), None);
        assert_eq!(grep_target(&ev("Grep", "pattern", "PeerProfileStore.getDisplayName")).as_deref(), Some("PeerProfileStore.getDisplayName"));
        // A search inside one file is not a lookup of the name.
        assert_eq!(grep_target(&ev("Bash", "command", "grep -n render examples/eval_stale.rs")), None);
        assert_eq!(grep_target(&ev("Bash", "command", "grep -rn render src/ tests")).as_deref(), Some("render"));
        let in_file = serde_json::json!({"tool_name": "Grep", "tool_input": {"pattern": "render", "path": "src/main.rs"}}).to_string();
        assert_eq!(grep_target(&in_file), None);
    }

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
