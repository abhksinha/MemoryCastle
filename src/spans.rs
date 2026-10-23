//! Ranked lines to read: the few code spans a question most needs, in the
//! order to read them, cheapest first on ties. Deterministic, from the code
//! index and the notes' declared links; no model and no embedding of code.
//!
//! Signals, strongest first:
//! 1. a symbol the question names, defined in the index (`named`);
//! 2. a symbol a top-ranked note names, in the note's rank order (`note`);
//! 3. the lines where a file uses a question symbol (`uses`);
//! 4. a definition whose name contains a content word of the question.
//!
//! Every span is re-anchored (`code::current_span`) so it is never stale.

use crate::code::{self, Def};
use crate::graph;
use crate::search::Hit;
use crate::Result;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReadSpan {
    /// Repo-relative.
    pub path: String,
    /// Absolute, when the repo root is known.
    pub abs: Option<String>,
    pub start: u32,
    pub end: u32,
    pub name: String,
    pub kind: &'static str,
    /// 0 to 1.
    pub score: f32,
    pub why: String,
    /// Bytes `sed -n 'start,endp'` would return.
    pub bytes: usize,
}

/// A backticked name that is one of the note's own files (`cite.tex`), not
/// a symbol (`tex`).
pub fn is_note_file(sym: &str, note_files: &[&String]) -> bool {
    sym.contains('.') && note_files.iter().any(|f| graph::same_file(f, sym))
}

/// The definitions of a name a note writes that are about that note: those
/// in the note's own `Files:`, else the name's only one or two definitions
/// (not an enum variant). A note writing `default`, `new` or `Build` meant
/// the word, and the 25 to 45 files defining it were noise (rushtex,
/// 2026-10-23).
pub fn note_defs(defs: Vec<Def>, note_files: &[&String]) -> Vec<Def> {
    let (mine, other): (Vec<Def>, Vec<Def>) = defs.into_iter().partition(|d| note_files.iter().any(|f| graph::same_file(f, &d.path)));
    if !mine.is_empty() {
        return mine;
    }
    if other.len() <= 2 && other.iter().all(|d| d.kind != "variant") {
        return other;
    }
    Vec::new()
}

/// Up to `limits::READ_SPANS` spans for `query` given the ranked `hits`.
pub fn ranked(conn: &Connection, project_id: i64, root: Option<&Path>, query: &str, hits: &[Hit]) -> Result<Vec<ReadSpan>> {
    if !code::has_index(conn, project_id)? {
        return Ok(Vec::new());
    }
    let (_, q_syms) = graph::query_entities(query);
    let mut cands: Vec<(Def, f32, String)> = Vec::new();
    let mut seen: HashSet<(String, u32)> = HashSet::new();
    let mut push = |d: Def, score: f32, why: String, cands: &mut Vec<(Def, f32, String)>| {
        if seen.insert((d.path.clone(), d.line)) {
            cands.push((d, score, why));
        }
    };

    // 1. named in the question
    let mut top_files: Vec<String> = Vec::new();
    for s in &q_syms {
        for d in code::defs_named(conn, project_id, s)? {
            if !top_files.contains(&d.path) {
                top_files.push(d.path.clone());
            }
            push(d, 1.0, "named in question".into(), &mut cands);
        }
    }

    // 2. symbols the top notes name, by note rank
    let mut note_rank: Vec<i64> = Vec::new();
    for h in hits {
        if !note_rank.contains(&h.note_id) {
            note_rank.push(h.note_id);
        }
    }
    // A note names many symbols; the ones whose name shares a word with the
    // question come first ("gate" picks `distinctive_terms` over the note's
    // other constants), the rest keep the note's order.
    let q_words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() >= 4 && !crate::search::STOPWORDS.contains(&w.to_ascii_lowercase().as_str()))
        .map(|w| crate::search::stem(&w.to_ascii_lowercase()).to_string())
        .collect();
    let links = graph::links_of(conn, &note_rank)?;
    for (i, id) in note_rank.iter().enumerate().take(crate::limits::READ_SPANS) {
        let w = 0.9 * (1.0 - i as f32 * 0.15).max(0.3);
        if let Some(l) = links.get(id) {
            let note_files: Vec<&String> = l.iter().filter(|(k, _)| *k == graph::Kind::File).map(|(_, f)| f).collect();
            for (k, sym) in l {
                if *k != graph::Kind::Symbol {
                    continue;
                }
                let lower = sym.rsplit("::").next().unwrap_or(sym).rsplit('.').next().unwrap_or(sym).to_ascii_lowercase();
                if lower.trim_matches('_').len() < 3 || is_note_file(sym, &note_files) {
                    continue;
                }
                let shares = q_words.iter().any(|q| lower.contains(q.as_str()));
                let (w, why) = if shares { (w + 0.05, format!("named by note #{}, name matches question", i + 1)) } else { (w, format!("named by note #{}", i + 1)) };
                for d in note_defs(code::defs_named(conn, project_id, sym)?, &note_files).into_iter().take(2) {
                    push(d, w, why.clone(), &mut cands);
                }
            }
        }
    }

    // 3. definitions in files that use a question symbol
    if !q_syms.is_empty() {
        let mut stmt = conn.prepare_cached(
            "SELECT src FROM code_mentions WHERE project_id = ?1 AND name = ?2 ORDER BY n DESC LIMIT 3",
        )?;
        for s in &q_syms {
            let short = s.rsplit("::").next().unwrap_or(s).rsplit('.').next().unwrap_or(s).trim_end_matches("()");
            let users: Vec<String> = stmt.query_map(params![project_id, short], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            for u in users {
                if top_files.contains(&u) {
                    continue;
                }
                // The enclosing definition may be hundreds of lines; point at
                // the first use itself, with a few lines of context.
                if let Some(d) = first_use(root, &u, short) {
                    push(d, 0.6, format!("uses {short}"), &mut cands);
                }
            }
        }
    }

    // 4. a definition whose name contains a content word of the question
    {
        let mut stmt = conn.prepare_cached(
            "SELECT path, name, kind, line, end_line, anchor FROM code_defs WHERE project_id = ?1 AND lower(name) LIKE ?2 ORDER BY end_line - line LIMIT 2",
        )?;
        for w in query.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| w.len() >= 5).map(str::to_ascii_lowercase) {
            if crate::search::STOPWORDS.contains(&w.as_str()) {
                continue;
            }
            let rows: Vec<Def> = stmt.query_map(params![project_id, format!("%{w}%")], row_def)?.collect::<rusqlite::Result<_>>()?;
            for d in rows {
                push(d, 0.5, format!("name matches '{w}'"), &mut cands);
            }
        }
    }

    // re-anchor, size, sort: score desc, then shorter first
    let mut out = Vec::new();
    let mut file_cache: HashMap<String, Vec<usize>> = HashMap::new();
    for (d, score, why) in cands {
        let span = match root {
            Some(r) if d.kind != "use" => code::current_span(conn, project_id, r, &d)?,
            _ => Some((d.line, d.end)),
        };
        let Some((a, b)) = span else { continue };
        let bytes = match root {
            Some(r) => {
                let offs = file_cache.entry(d.path.clone()).or_insert_with(|| {
                    std::fs::read_to_string(r.join(&d.path)).map(|t| t.lines().map(|l| l.len() + 1).collect()).unwrap_or_default()
                });
                offs.iter().skip(a.saturating_sub(1) as usize).take((b - a + 1) as usize).sum()
            }
            None => 0,
        };
        out.push(ReadSpan {
            abs: root.map(|r| r.join(&d.path).to_string_lossy().into_owned()),
            path: d.path,
            start: a,
            end: b,
            name: d.name,
            kind: d.kind,
            score,
            why,
            bytes,
        });
    }
    out.sort_by(|x, y| y.score.partial_cmp(&x.score).unwrap_or(std::cmp::Ordering::Equal).then(x.bytes.cmp(&y.bytes)));
    // A name-match guess beside a definite span is noise the agent pays for:
    // when a strong span exists, the weak ones are not printed.
    if out.first().is_some_and(|s| s.score >= crate::limits::SPAN_STRONG) {
        out.retain(|s| s.score >= crate::limits::SPAN_WEAK);
    }
    out.truncate(crate::limits::READ_SPANS);
    Ok(out)
}

fn row_def(r: &rusqlite::Row) -> rusqlite::Result<Def> {
    let kind: String = r.get(2)?;
    Ok(Def {
        path: r.get(0)?,
        name: r.get(1)?,
        kind: code::kind_str(&kind),
        line: r.get::<_, i64>(3)? as u32,
        end: r.get::<_, i64>(4)? as u32,
        anchor: r.get(5)?,
    })
}

/// The first line in `path` that mentions `name`, as a span of
/// `USE_CONTEXT_LINES` either side. Anchor is that line, so re-anchoring
/// finds it again after edits.
fn first_use(root: Option<&Path>, path: &str, name: &str) -> Option<Def> {
    first_use_by(root, path, name, is_call)
}

/// A constant is used as a value (`limits::NAME`, `NAME.min(x)`), never
/// called, so its first use is the first line holding the identifier with
/// boundaries on both sides, outside strings and comments.
fn first_value_use(root: Option<&Path>, path: &str, name: &str) -> Option<Def> {
    first_use_by(root, path, name, is_identifier_use)
}

pub fn is_identifier_use(line: &str, name: &str) -> bool {
    // An import names the constant without using it.
    let t = line.trim_start();
    if t.starts_with("use ") || t.starts_with("import ") || t.starts_with("from ") || t.starts_with("pub use ") {
        return false;
    }
    let line = code::strip_strings_and_comments(line);
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(i) = line[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before_ok = start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let after_ok = end == bytes.len() || !(bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_');
        // A definition line is not a use.
        let defines = line[..start].trim_end().ends_with("const") || line[..start].trim_end().ends_with("static");
        if before_ok && after_ok && !defines {
            return true;
        }
        from = end;
    }
    false
}

fn first_use_by(root: Option<&Path>, path: &str, name: &str, hit: fn(&str, &str) -> bool) -> Option<Def> {
    let text = std::fs::read_to_string(root?.join(path)).ok()?;
    let (i, line) = text.lines().enumerate().find(|(_, l)| hit(l, name))?;
    let n = text.lines().count() as u32;
    let at = i as u32 + 1;
    let c = crate::limits::USE_CONTEXT_LINES;
    Some(Def {
        path: path.to_string(),
        name: format!("use of {name}"),
        kind: "use",
        line: at.saturating_sub(c).max(1),
        end: (at + c).min(n),
        anchor: line.trim().chars().take(160).collect(),
    })
}

/// Whether `line` uses `name` as a call or a path (`name(`, `name!(`,
/// `x.name(`, `name::`), with identifier boundaries on both sides. A line
/// that merely contains the word (`served_notes`, a comment saying
/// "notes") is not a caller; measured on `quality::notes`, the plain
/// substring test named four files that never call it.
pub fn is_call(line: &str, name: &str) -> bool {
    // `INSERT INTO notes(` in a SQL string is not a call.
    let line = code::strip_strings_and_comments(line);
    let line = line.as_str();
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(i) = line[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before_ok = start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let rest = &line[end..];
        let after_ok = rest.starts_with('(') || rest.starts_with("!(") || rest.starts_with("::");
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// A call site span: (path, first line, last line, calling function, confidence).
pub type CallSite = (String, u32, u32, String, f32);

/// Every line that calls `d` in the files its callers are in: (path, a, b,
/// calling function, confidence), a span of ±3 lines clipped to the calling
/// function, sites closer than that merged, most confident first then by
/// file and line. Callers under `CALL_MIN_CONF` only with `all`.
pub fn call_sites(conn: &Connection, project_id: i64, root: &Path, d: &Def, all: bool) -> Result<Vec<CallSite>> {
    let min = crate::limits::CALL_MIN_CONF;
    let callers: Vec<(String, String, u32, f32)> = code::callers_for(conn, project_id, d)?.into_iter().filter(|c| all || c.3 >= min).collect();
    let mut files: Vec<&String> = callers.iter().map(|c| &c.1).collect();
    files.sort();
    files.dedup();
    let mut out: Vec<CallSite> = Vec::new();
    for f in files {
        let Ok(text) = std::fs::read_to_string(root.join(f)) else { continue };
        let fdefs = code::definitions(f, &text);
        for (i, raw) in text.lines().enumerate() {
            let ln = i as u32 + 1;
            if !raw.contains(&d.name) || (f == &d.path && ln >= d.line && ln <= d.end) {
                continue;
            }
            let line = code::strip_strings_and_comments(raw);
            if code::qualifier(&line, &d.name).is_none() {
                continue;
            }
            let Some(enc) = fdefs.iter().filter(|x| x.kind == "fn" && x.line <= ln && ln <= x.end).max_by_key(|x| x.line) else { continue };
            // The site counts when its function is a caller this definition kept.
            let Some(c) = callers.iter().find(|c| c.0 == enc.name && &c.1 == f) else { continue };
            let (a, b) = (ln.saturating_sub(3).max(enc.line), (ln + 3).min(enc.end));
            match out.last_mut() {
                Some(last) if last.0 == *f && last.3 == enc.name && a <= last.2 + 1 => last.2 = last.2.max(b),
                _ => out.push((f.clone(), a, b, enc.name.clone(), c.3)),
            }
        }
    }
    out.sort_by(|x, y| y.4.total_cmp(&x.4).then(x.0.cmp(&y.0)).then(x.1.cmp(&y.1)));
    Ok(out)
}

/// For `links --context`: the constants a definition's body names (printed as
/// `read:` lines to their definitions) and the lines that call it in other
/// files. Reads the defining file once and the using files once each.
/// For a function, one `read:` span per call site (±3 lines, inside the
/// calling function, overlapping sites merged), likely callers only unless
/// `all`, at most `CONTEXT_SITES` unless `all`.
pub fn context(conn: &Connection, project_id: i64, root: &Path, defs: &[Def], all: bool) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut seen: HashSet<(String, u32)> = HashSet::new();
    for d in defs {
        let Ok(text) = std::fs::read_to_string(root.join(&d.path)) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        let a = d.line.saturating_sub(1) as usize;
        let b = (d.end as usize).min(lines.len());
        if a >= b {
            continue;
        }
        // Strings and comments blanked: `UNION ALL` in a SQL string is not
        // the constant `ALL`.
        let body = lines[a..b].iter().map(|l| code::strip_strings_and_comments(l)).collect::<Vec<_>>().join("\n");
        // constants: identifiers in the body that the index defines as const, in this file first
        let mut names: Vec<&str> = code::identifiers(&body).filter(|w| w.chars().any(|c| c.is_ascii_uppercase()) && *w != d.name).collect();
        names.sort_unstable();
        names.dedup();
        // Filtered to constants before the cap: sorted names put `Option`,
        // `Result` and `Some` first and `SPAN_WIDE` never got a slot. A
        // variant counts only in the definition's own file, since `Option`
        // in a tuple variant was indexed as one elsewhere.
        let mut printed = 0;
        for name in &names {
            if printed == crate::limits::CONTEXT_ITEMS {
                break;
            }
            for c in code::defs_named(conn, project_id, name)?.into_iter().filter(|c| c.kind == "const" || (c.kind == "variant" && c.path == d.path)).take(1) {
                if seen.insert((c.path.clone(), c.line)) {
                    out.push(format!("read: {}:{}-{}  {}  uses constant", c.path, c.line, c.end.max(c.line), c.name));
                    printed += 1;
                }
            }
        }
        // callers: files that mention the name, first use line each. A
        // constant is used as a value, so the match is the identifier, and
        // the defining file counts too (a limit is used where it is set).
        let is_const = d.kind == "const" || d.kind == "static";
        if d.kind == "fn" {
            let sites = call_sites(conn, project_id, root, d, all)?;
            let cap = if all { usize::MAX } else { crate::limits::CONTEXT_SITES };
            for (path, a, b, caller, conf) in sites.iter().take(cap) {
                if seen.insert((path.clone(), *a)) {
                    out.push(format!("read: {path}:{a}-{b}  {caller}  calls {} {conf:.1}", d.name));
                }
            }
            if sites.len() > cap {
                out.push(format!("+{} more call sites; links {} --context --all prints every one", sites.len() - cap, d.name));
            }
            continue;
        }
        let mut stmt = conn.prepare_cached("SELECT src FROM code_mentions WHERE project_id = ?1 AND name = ?2 AND (src != ?3 OR ?5) ORDER BY n DESC LIMIT ?4")?;
        let users: Vec<String> = stmt.query_map(params![project_id, d.name, d.path, crate::limits::CONTEXT_ITEMS as i64, is_const], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for u in users {
            let found = if is_const { first_value_use(Some(root), &u, &d.name) } else { first_use(Some(root), &u, &d.name) };
            if let Some(use_def) = found {
                if seen.insert((use_def.path.clone(), use_def.line)) {
                    let verb = if is_const { "uses" } else { "calls" };
                    out.push(format!("read: {}:{}-{}  {}  {verb} {}", use_def.path, use_def.line, use_def.end, use_def.name, d.name));
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod call_tests {
    use super::{is_call, is_identifier_use};
    #[test]
    fn value_uses_of_a_constant() {
        assert!(is_identifier_use("if lines <= crate::limits::WHOLE_READ_LINES { size }", "WHOLE_READ_LINES"));
        assert!(is_identifier_use("let x = WHOLE_READ_LINES.min(n);", "WHOLE_READ_LINES"));
        assert!(!is_identifier_use("pub const WHOLE_READ_LINES: usize = 2000;", "WHOLE_READ_LINES"), "the definition is not a use");
        assert!(!is_identifier_use("// WHOLE_READ_LINES is the cap", "WHOLE_READ_LINES"), "a comment is not a use");
        assert!(!is_identifier_use("let WHOLE_READ_LINES_X = 1;", "WHOLE_READ_LINES"), "a longer identifier is not a use");
        assert!(!is_identifier_use("use crate::limits::{OPEN_MIN_SECS, WHOLE_READ_LINES};", "WHOLE_READ_LINES"), "an import is not a use");
    }
    #[test]
    fn call_shapes_only() {
        assert!(is_call("    let n = notes(conn, pid)?;", "notes"));
        assert!(is_call("quality::notes(&ctx.conn, pid)", "notes"));
        assert!(is_call("    out.push(x.notes(3));", "notes"));
        assert!(is_call("    notes::render(&n, false)", "notes"));
        assert!(!is_call("INSERT INTO served_notes(at)", "notes"));
        assert!(!is_call("// the notes a search printed", "notes"));
        assert!(!is_call("let notes_for = 3;", "notes"));
        assert!(!is_call("conn.execute(\"INSERT INTO notes(project_id) VALUES (1)\")", "notes"));
    }
}
