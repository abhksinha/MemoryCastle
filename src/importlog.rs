//! Reads taken from another agent's session log, for agents without hooks
//! (Codex, Antigravity). Recognises `sed -n 'a,bp' f`, `cat f` and
//! `head -n N f`; a read is guided when a memorycastle span for that file
//! (`defined: f:a-b`, `read: f:a-b`, `code: name f:a-b`) appears earlier
//! in the log. Bytes are computed from the file as it is now.

use crate::store;
use crate::Result;
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn import(conn: &Connection, project_id: i64, root: &Path, log: &str) -> Result<usize> {
    let mut spans: HashMap<String, Vec<(u32, u32, u32)>> = HashMap::new();
    let mut n = 0;
    let mut rank_in_block = 0u32;
    for line in log.lines() {
        let t = line.trim();
        if t.starts_with("read first (") {
            rank_in_block = 0;
        }
        if let Some(rest) = t.strip_prefix("read: ").or_else(|| t.strip_prefix("defined: ")) {
            let rank = if t.starts_with("read: ") { rank_in_block += 1; rank_in_block } else { 0 };
            if let Some((path, a, b)) = parse_span(rest.split_whitespace().next().unwrap_or("")) {
                spans.entry(rel(root, &path)).or_default().push((a, b, rank));
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("open ").or_else(|| t.strip_prefix("code: ")) {
            // `open path:a-b name ..` (the brief answer, ranked), `code:
            // path:a-b` before it, or the older `code: name path:a-b`.
            let mut toks = rest.split_whitespace();
            if let Some((path, a, b)) = toks.next().and_then(parse_span) {
                rank_in_block += 1;
                spans.entry(rel(root, &path)).or_default().push((a, b, rank_in_block));
            } else if let Some((path, a, b)) = toks.next().and_then(parse_span) {
                spans.entry(rel(root, &path)).or_default().push((a, b, 0));
            }
            continue;
        }
        if t.starts_with("1. ") || t.starts_with("memcastle search") {
            rank_in_block = 0;
        }
        // Codex logs wrap commands: `/bin/zsh -lc 'sed -n …' in /dir`.
        let cmd = match t.find("-lc ") {
            Some(i) => {
                let rest = t[i + 4..].trim_start();
                match rest.chars().next() {
                    Some(q @ ('\'' | '"')) => rest[1..].split(q).next().unwrap_or(rest),
                    _ => rest,
                }
            }
            None => t,
        };
        for (path, range) in reads_in(cmd) {
            let relp = rel(root, &path);
            if relp.starts_with("memorycastle/") || !root.join(&relp).is_file() {
                continue;
            }
            let bytes = bytes_of(root, &relp, range);
            let (a, b) = range.unwrap_or((1, u32::MAX));
            let rank = spans.get(&relp).and_then(|v| v.iter().rev().find(|(sa, sb, _)| *sa <= b && *sb >= a).map(|(_, _, r)| *r));
            store::record_read_lines(conn, project_id, root, &relp, bytes, range, Some(rank))?;
            n += 1;
        }
    }
    Ok(n)
}

/// A Claude Code transcript (`~/.claude/projects/<dir>/<session>.jsonl`):
/// every `Read` and every `Bash` file read, with the bytes its result
/// returned and the transcript's own timestamp, so a past session becomes
/// an unguided baseline (or a guided one, when its results hold
/// memorycastle spans). `memorycastle search`/`links` run through Bash are
/// recorded as lookups. Imported once per transcript (`meta` key).
pub fn import_claude(conn: &Connection, project_id: i64, root: &Path, path: &Path) -> Result<(usize, usize)> {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let key = format!("imported:{name}");
    if crate::db::meta_get(conn, &key)?.is_some() {
        return Ok((0, 0));
    }
    let text = std::fs::read_to_string(path)?;
    // Pending tool uses by id: (at, what).
    enum Use {
        Read(String, Option<(u32, u32)>),
        Bash(Vec<(String, Option<(u32, u32)>)>, bool),
    }
    let mut pending: HashMap<String, (i64, Use)> = HashMap::new();
    let mut spans: HashMap<String, Vec<(u32, u32, u32)>> = HashMap::new();
    let mut last_at = 0i64;
    let (mut reads, mut lookups) = (0usize, 0usize);
    let session = format!("claude:{name}");
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let at = v.get("timestamp").and_then(|t| t.as_str()).and_then(parse_rfc3339).unwrap_or(last_at);
        last_at = at;
        let Some(content) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) else { continue };
        for c in content {
            match c.get("type").and_then(|t| t.as_str()) {
                Some("tool_use") => {
                    let id = c.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    let input = c.get("input").cloned().unwrap_or_default();
                    match c.get("name").and_then(|n| n.as_str()) {
                        Some("Read") => {
                            let Some(p) = input.get("file_path").and_then(|p| p.as_str()) else { continue };
                            let off = input.get("offset").and_then(|o| o.as_u64());
                            let lim = input.get("limit").and_then(|o| o.as_u64());
                            let range = (off.is_some() || lim.is_some()).then(|| {
                                let a = off.unwrap_or(1).max(1) as u32;
                                (a, a + lim.unwrap_or(2000) as u32 - 1)
                            });
                            pending.insert(id, (at, Use::Read(p.to_string(), range)));
                        }
                        Some("Bash") => {
                            let cmd = input.get("command").and_then(|c| c.as_str()).unwrap_or("");
                            let lookup = is_plain_lookup(cmd);
                            let files = reads_in(cmd);
                            if lookup || !files.is_empty() {
                                pending.insert(id, (at, Use::Bash(files, lookup)));
                            }
                        }
                        _ => {}
                    }
                }
                Some("tool_result") => {
                    let id = c.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("");
                    let Some((used_at, what)) = pending.remove(id) else { continue };
                    let body = match c.get("content") {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(serde_json::Value::Array(parts)) => parts.iter().filter_map(|p| p.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n"),
                        _ => String::new(),
                    };
                    let bytes = body.len();
                    match what {
                        Use::Read(p, range) => {
                            let r = rel(root, &p);
                            if !root.join(&r).is_file() || r.starts_with("memorycastle/") {
                                continue;
                            }
                            let (a, b) = range.unwrap_or((1, u32::MAX));
                            let rank = spans.get(&r).and_then(|v| v.iter().find(|(x, y, _)| *x <= b && *y >= a).map(|(_, _, k)| *k));
                            store::record_read_at(conn, project_id, root, &r, bytes, range, Some(rank), used_at, Some(&session))?;
                            reads += 1;
                        }
                        Use::Bash(files, lookup) => {
                            if lookup {
                                // One call prints at most the search budget; a larger
                                // result means the command did more than a lookup.
                                let served = bytes.min(2 * crate::limits::SEARCH_OUTPUT_BYTES);
                                store::record_usage_at(conn, project_id, "search", served, 0, used_at, Some(&session))?;
                                lookups += 1;
                                // Spans the lookup printed guide later reads.
                                for (p, a, b) in spans_in_output(&body) {
                                    spans.entry(rel(root, &p)).or_default().push((a, b, 1));
                                }
                            }
                            for (p, range) in files {
                                let r = rel(root, &p);
                                if !root.join(&r).is_file() || r.starts_with("memorycastle/") {
                                    continue;
                                }
                                let (a, b) = range.unwrap_or((1, u32::MAX));
                                let rank = spans.get(&r).and_then(|v| v.iter().find(|(x, y, _)| *x <= b && *y >= a).map(|(_, _, k)| *k));
                                store::record_read_at(conn, project_id, root, &r, bytes, range, Some(rank), used_at, Some(&session))?;
                                reads += 1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    crate::db::meta_set(conn, &key, &format!("{reads} reads, {lookups} lookups"))?;
    Ok((reads, lookups))
}

/// A shell line that is one memorycastle lookup and nothing else (an
/// optional `cd dir &&` first), not a loop, an eval or a pipeline of them.
fn is_plain_lookup(cmd: &str) -> bool {
    let t = cmd.trim();
    let t = match t.split_once("&&") {
        Some((head, rest)) if head.trim_start().starts_with("cd ") => rest.trim(),
        _ => t,
    };
    (t.starts_with("memorycastle search ") || t.starts_with("memorycastle links "))
        && !t.contains("&&")
        && !t.contains(';')
        && !t.contains(" for ")
        && !t.contains("--explain")
}

/// `2026-10-18T07:17:55.546Z` to a Unix timestamp.
fn parse_rfc3339(s: &str) -> Option<i64> {
    let day = crate::render::parse_date(s.get(0..10)?)?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let m: i64 = s.get(14..16)?.parse().ok()?;
    let sec: i64 = s.get(17..19)?.parse().ok()?;
    Some(day + h * 3600 + m * 60 + sec)
}

fn rel(root: &Path, p: &str) -> String {
    Path::new(p).strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| p.trim_start_matches("./").to_string())
}

/// `path:a-b` or `path:a`.
/// Spans a lookup's output names: `read: path:a-b`, `defined: path:a-b`,
/// `code: path:a-b`, `open path:a-b`, and the numbered `read first` table rows `n. path:a-b`.
pub fn spans_in_output(body: &str) -> Vec<(String, u32, u32)> {
    let mut out = Vec::new();
    for l in body.lines() {
        let t = l.trim();
        let tok = if let Some(rest) = t.strip_prefix("read: ").or_else(|| t.strip_prefix("defined: ")).or_else(|| t.strip_prefix("code: ")).or_else(|| t.strip_prefix("open ")) {
            rest.split_whitespace().next()
        } else if t.len() > 3 && t.as_bytes()[0].is_ascii_alphanumeric() && t.split_once(". ").is_some_and(|(n, _)| n.len() <= 3 && n.chars().all(|c| c.is_ascii_alphanumeric())) {
            // `1. path:a-b` (older output) or `a. path:a-b` (the lettered table).
            t.split_once(". ").and_then(|(_, r)| r.split_whitespace().next()).filter(|tok| tok.contains(':') && tok.contains('-'))
        } else {
            None
        };
        if let Some((p, a, b)) = tok.and_then(parse_span) {
            out.push((p, a, b));
        }
    }
    out
}

/// The working directory a transcript was recorded in: Claude writes `cwd`
/// on its records, Codex on `session_meta`. Scanned from the first lines.
pub fn transcript_cwd(path: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().take(40) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if let Some(c) = v.get("cwd").and_then(|c| c.as_str()) {
            return Some(PathBuf::from(c));
        }
        if let Some(c) = v.get("payload").and_then(|p| p.get("cwd")).and_then(|c| c.as_str()) {
            return Some(PathBuf::from(c));
        }
    }
    None
}

/// Every Claude Code and Codex transcript on this machine recorded in `root`.
pub fn transcripts_for(root: &Path) -> Vec<(PathBuf, &'static str)> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut out = Vec::new();
    let mut walk = |dir: PathBuf, kind: &'static str, depth: usize| {
        let mut stack = vec![(dir, 0usize)];
        while let Some((d, n)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && n < depth {
                    stack.push((p, n + 1));
                } else if p.extension().is_some_and(|x| x == "jsonl") && transcript_cwd(&p).is_some_and(|c| c == root) {
                    out.push((p, kind));
                }
            }
        }
    };
    walk(home.join(".claude/projects"), "claude", 2);
    walk(home.join(".codex/sessions"), "codex", 4);
    out.sort();
    out
}

/// A Codex rollout (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`): tool
/// calls are `response_item` records of type `function_call` (shell, with
/// a command list) or `custom_tool_call` (exec, a script whose strings hold
/// the commands); outputs follow by `call_id`. Lookups and file reads are
/// found by the same rules as a Claude transcript. Imported once.
pub fn import_codex(conn: &Connection, project_id: i64, root: &Path, path: &Path) -> Result<(usize, usize)> {
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let key = format!("imported:{name}");
    if crate::db::meta_get(conn, &key)?.is_some() {
        return Ok((0, 0));
    }
    let text = std::fs::read_to_string(path)?;
    // call id -> (at, file reads, was a lookup)
    type Pending = HashMap<String, (i64, Vec<(String, Option<(u32, u32)>)>, bool)>;
    let mut pending: Pending = HashMap::new();
    let mut spans: HashMap<String, Vec<(u32, u32, u32)>> = HashMap::new();
    let mut last_at = 0i64;
    let (mut reads, mut lookups) = (0usize, 0usize);
    let session = format!("codex:{name}");
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let at = v.get("timestamp").and_then(|t| t.as_str()).and_then(parse_rfc3339).unwrap_or(last_at);
        last_at = at;
        let Some(p) = v.get("payload") else { continue };
        match p.get("type").and_then(|t| t.as_str()) {
            Some("function_call") | Some("custom_tool_call") => {
                let id = p.get("call_id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                let cmd = match p.get("input").and_then(|i| i.as_str()) {
                    Some(script) => script.to_string(),
                    None => {
                        let args = p.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                        serde_json::from_str::<serde_json::Value>(args)
                            .ok()
                            .and_then(|a| {
                                a.get("command").map(|c| match c {
                                    serde_json::Value::Array(parts) => parts.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "),
                                    serde_json::Value::String(s) => s.clone(),
                                    _ => String::new(),
                                })
                            })
                            .unwrap_or_default()
                    }
                };
                // A script holds commands inside quoted strings; each one
                // is judged as its own command.
                let pieces: Vec<&str> = cmd.split(['\'', '"', '\n']).collect();
                let lookup = pieces.iter().any(|c| is_plain_lookup(c.trim()));
                let mut files = Vec::new();
                for c in &pieces {
                    files.extend(reads_in(c));
                }
                if lookup || !files.is_empty() {
                    pending.insert(id, (at, files, lookup));
                }
            }
            Some("function_call_output") | Some("custom_tool_call_output") => {
                let id = p.get("call_id").and_then(|i| i.as_str()).unwrap_or("");
                let Some((used_at, files, lookup)) = pending.remove(id) else { continue };
                let body = match p.get("output") {
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(serde_json::Value::Array(parts)) => parts.iter().filter_map(|x| x.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join("\n"),
                    _ => String::new(),
                };
                let bytes = body.len();
                if lookup {
                    let served = bytes.min(2 * crate::limits::SEARCH_OUTPUT_BYTES);
                    store::record_usage_at(conn, project_id, "search", served, 0, used_at, Some(&session))?;
                    lookups += 1;
                    for (p, a, b) in spans_in_output(&body.replace("\\n", "\n")) {
                        spans.entry(rel(root, &p)).or_default().push((a, b, 1));
                    }
                }
                for (p, range) in files {
                    let r = rel(root, &p);
                    if !root.join(&r).is_file() || r.starts_with("memorycastle/") {
                        continue;
                    }
                    let (a, b) = range.unwrap_or((1, u32::MAX));
                    let rank = spans.get(&r).and_then(|v| v.iter().find(|(x, y, _)| *x <= b && *y >= a).map(|(_, _, k)| *k));
                    store::record_read_at(conn, project_id, root, &r, bytes, range, Some(rank), used_at, Some(&session))?;
                    reads += 1;
                }
            }
            _ => {}
        }
    }
    crate::db::meta_set(conn, &key, &format!("{reads} reads, {lookups} lookups"))?;
    Ok((reads, lookups))
}

fn parse_span(tok: &str) -> Option<(String, u32, u32)> {
    let (path, range) = tok.rsplit_once(':')?;
    let (a, b) = match range.split_once('-') {
        Some((a, b)) => (a.parse().ok()?, b.parse().ok()?),
        None => {
            let a: u32 = range.parse().ok()?;
            (a, a)
        }
    };
    Some((path.to_string(), a, b))
}

/// File reads in one shell line: (path, line range).
fn reads_in(cmd: &str) -> Vec<(String, Option<(u32, u32)>)> {
    let mut out = Vec::new();
    for part in cmd.split(['|', ';', '&']) {
        let words: Vec<&str> = part.split_whitespace().collect();
        let Some(first) = words.first() else { continue };
        let tool = first.rsplit('/').next().unwrap_or(first);
        match tool {
            "cat" => {
                for w in &words[1..] {
                    if !w.starts_with('-') && is_path(w) {
                        out.push((unquote(w), None));
                    }
                }
            }
            "sed" => {
                let joined = words[1..].join(" ");
                let range = joined.split(['\'', '"', ' ']).find_map(|t| {
                    let t = t.trim_end_matches('p');
                    let (a, b) = t.split_once(',')?;
                    Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))
                });
                if let Some(f) = words.iter().rev().find(|w| is_path(w)) {
                    out.push((unquote(f), range));
                }
            }
            "head" | "tail" => {
                let n = words.iter().position(|w| *w == "-n").and_then(|i| words.get(i + 1)).and_then(|v| v.parse::<u32>().ok());
                if let Some(f) = words.iter().rev().find(|w| is_path(w)) {
                    let range = n.map(|n| if tool == "head" { (1, n) } else { (u32::MAX - n, u32::MAX) });
                    out.push((unquote(f), range));
                }
            }
            _ => {}
        }
    }
    out
}

fn is_path(w: &str) -> bool {
    let w = unquote(w);
    w.contains('/') || w.contains('.') && !w.starts_with('-') && !w.ends_with('p')
}

fn unquote(w: &str) -> String {
    w.trim_matches(|c| c == '\'' || c == '"').to_string()
}

fn bytes_of(root: &Path, relp: &str, range: Option<(u32, u32)>) -> usize {
    let Ok(text) = std::fs::read_to_string(root.join(relp)) else { return 0 };
    match range {
        None => text.len(),
        Some((a, b)) => {
            let n = text.lines().count() as u32;
            let (a, b) = if a > n { (n.saturating_sub(b.saturating_sub(a)).max(1), n) } else { (a.max(1), b.min(n)) };
            text.lines().skip(a as usize - 1).take((b - a + 1) as usize).map(|l| l.len() + 1).sum()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_reads_are_recognised() {
        let r = reads_in("sed -n '64,118p' src/a.rs | head -3");
        assert_eq!(r[0].0, "src/a.rs");
        assert_eq!(r[0].1, Some((64, 118)));
        let r = reads_in("cat src/b.kt");
        assert_eq!(r[0], ("src/b.kt".to_string(), None));
        assert_eq!(parse_span("src/a.rs:4-9"), Some(("src/a.rs".into(), 4, 9)));
    }
}
