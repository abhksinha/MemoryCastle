//! Interlinkages between notes, codebase files and symbols, derived from the
//! lines agents already write (`Files:`, `Supersedes:`, backticked names,
//! mentions of other notes). No inference: every edge traces to a line in a
//! note. Stored in `links`, an indexed table, so every graph question is one
//! index lookup. The TSV and HTML exports are derived from it on request.

use crate::db::placeholders;
use crate::error::Result;
use crate::fts;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

/// Edge kinds, stored as one small integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Kind {
    /// Note is about this codebase file (`Files:` line, or a path in the body).
    File = 0,
    /// Note refers to another note (`Supersedes:` or a mention in the body).
    Note = 1,
    /// Note names a code symbol in backticks.
    Symbol = 2,
}

impl Kind {
    pub fn from_i64(v: i64) -> Kind {
        match v {
            0 => Kind::File,
            1 => Kind::Note,
            _ => Kind::Symbol,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Note => "note",
            Kind::Symbol => "symbol",
        }
    }
}

/// Most symbol edges a note may contribute. Keeps a pathological note bounded.
pub use crate::limits::GRAPH_MAX_SYMBOLS_PER_NOTE as MAX_SYMBOLS;

/// Resolves a note reference written inside `from_path` to a store path: a
/// bare file name is relative to the referring note's directory; `journal/x`
/// under `memorycastle/journal/` is accepted as the short form.
pub fn resolve_note_ref(from_path: &str, target: &str) -> String {
    let t = target.trim_matches(|c| c == '`' || c == '.' || c == ',' || c == '"' || c == '(' || c == ')');
    let t = t.strip_prefix("./").unwrap_or(t);
    if t.contains('/') {
        if let Some(rest) = t.strip_prefix("journal/") {
            if from_path.starts_with("memorycastle/journal/") {
                return format!("memorycastle/journal/{rest}");
            }
        }
        return t.to_string();
    }
    match from_path.rfind('/') {
        Some(i) => format!("{}/{}", &from_path[..i], t),
        None => t.to_string(),
    }
}

/// Everything the note links to. One pass over the text; header lines first.
pub fn extract(note_path: &str, content: &str) -> Vec<(Kind, String)> {
    let mut out: Vec<(Kind, String)> = Vec::new();
    let mut push = |k: Kind, t: String| {
        if !out.iter().any(|(ok, ot)| *ok == k && *ot == t) {
            out.push((k, t));
        }
    };
    let mut symbols = 0usize;
    let mut in_fence = false;
    for (n, line) in content.lines().enumerate() {
        let l = line.trim();
        if l.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        let header = n < 20 && !content.lines().take(n).any(|x| x.starts_with("## "));
        if header {
            if let Some(i) = l.find("Files:") {
                for tok in l[i + 6..].split(|c: char| c == ',' || c.is_whitespace()) {
                    let t = tok.trim_matches(|c| c == '`' || c == '.' || c == ';' || c == '"');
                    if looks_like_path(t) {
                        push(Kind::File, t.to_string());
                    }
                }
                continue;
            }
            if let Some(t) = crate::chunk::supersedes(l) {
                push(Kind::Note, resolve_note_ref(note_path, &t));
                continue;
            }
        }
        // Mentions of other notes anywhere in the body.
        for tok in l.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',') {
            let t = tok.trim_matches(|c| c == '`' || c == '.' || c == ';' || c == '"' || c == '*');
            if t.ends_with(".md") && t.contains("journal/") {
                let r = resolve_note_ref(note_path, t);
                if r != note_path {
                    push(Kind::Note, r);
                }
            }
        }
        if in_fence || header {
            continue;
        }
        // Backticked spans in prose: a path becomes a file edge, an identifier a symbol edge.
        let mut rest = l;
        while let Some(a) = rest.find('`') {
            let after = &rest[a + 1..];
            let Some(b) = after.find('`') else { break };
            let span = after[..b].trim().trim_end_matches("()");
            rest = &after[b + 1..];
            if span.is_empty() || span.len() > 80 || span.contains(' ') {
                continue;
            }
            if looks_like_path(span) {
                push(Kind::File, span.to_string());
            } else if symbols < MAX_SYMBOLS && looks_like_symbol(span) {
                symbols += 1;
                push(Kind::Symbol, span.to_string());
            }
        }
    }
    out
}

/// A code symbol: identifier segments joined by `::` or `.`, starting with a
/// letter or underscore. Rejects assignments (`X=1`), addresses (`::1`),
/// numbers and dates.
/// Two spellings of one symbol: equal, or equal after dropping a `::` or
/// `.` path on either side (`spans::context` and `context`).
pub fn same_symbol(a: &str, b: &str) -> bool {
    let short = |t: &str| t.rsplit("::").next().unwrap_or(t).rsplit('.').next().unwrap_or(t).trim_end_matches("()").to_string();
    a == b || short(a) == short(b)
}

pub fn looks_like_symbol(t: &str) -> bool {
    if t.is_empty() || t.contains(['=', ' ', '/', '"', '\'']) {
        return false;
    }
    let first = t.chars().next().unwrap();
    if !(first.is_alphabetic() || first == '_') {
        return false;
    }
    let segs: Vec<&str> = t.split("::").flat_map(|p| p.split('.')).collect();
    if !segs.iter().all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_alphanumeric() || c == '_') && seg.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)) {
        return false;
    }
    t.contains("::") || t.contains('.') || fts::is_identifier(t)
}

/// File extensions a bare name (no directory) is trusted with. A path with a
/// directory part is trusted as written. URL routes (`/api/x`) are neither.
const FILE_EXTS: &[&str] = &[
    "rs", "kt", "kts", "java", "py", "ts", "tsx", "js", "jsx", "mjs", "go", "c", "h", "cc", "cpp", "hpp", "swift", "m", "mm", "rb", "php", "cs",
    "toml", "json", "yaml", "yml", "md", "txt", "sh", "zsh", "bash", "sql", "html", "css", "scss", "xml", "gradle", "proto", "env", "cfg", "ini",
    "lock", "csv", "tsv", "pb", "bin", "onnx", "plist", "gitignore", "dockerfile", "makefile", "cmake", "nix", "tf", "ipynb", "svg", "png",
];

pub fn looks_like_path(t: &str) -> bool {
    if t.len() < 3 || t.len() > 200 || t.contains([' ', '{', '}', '*', ',', '<', '>', '=']) || t.contains("::") {
        return false;
    }
    let name = t.rsplit('/').next().unwrap_or(t);
    let ext = name.rsplit('.').next().filter(|e| *e != name).map(|e| e.to_ascii_lowercase());
    let known_ext = ext.as_deref().map(|e| FILE_EXTS.contains(&e)).unwrap_or(false);
    if t.starts_with('/') {
        // Absolute: a real file system path has an extension or a known root; a URL route has neither.
        return known_ext || t.starts_with("/opt/") || t.starts_with("/etc/") || t.starts_with("/usr/") || t.starts_with("/var/") || t.starts_with("/home/") || t.starts_with("/Users/");
    }
    if t.contains('/') {
        // Relative path with a directory part: trust it (dirs like `ui/status/` included).
        return !t.ends_with('.');
    }
    known_ext
}

/// Replaces the links of `note_id` with those found in `content`.
pub fn store_links(tx: &Connection, note_id: i64, project_id: i64, note_path: &str, content: &str) -> Result<usize> {
    tx.execute("DELETE FROM links WHERE note_id = ?1", [note_id])?;
    let links = extract(note_path, content);
    let mut stmt = tx.prepare_cached("INSERT OR IGNORE INTO links(note_id, project_id, kind, target) VALUES (?1, ?2, ?3, ?4)")?;
    for (k, t) in &links {
        // A bare or partial file name becomes the one repo path it names,
        // when the code index knows exactly one; an ambiguous name is kept
        // as written and the saver is told.
        let target = if *k == Kind::File && crate::code::has_index(tx, project_id).unwrap_or(false) {
            match crate::code::paths_ending_with(tx, project_id, t)?.as_slice() {
                [one] => one.clone(),
                [] => t.clone(),
                many => {
                    eprintln!("memorycastle: {note_path}: `{t}` names {} files ({}); write the full path", many.len(), many.join(", "));
                    t.clone()
                }
            }
        } else {
            t.clone()
        };
        stmt.execute(params![note_id, project_id, *k as i64, target])?;
    }
    Ok(links.len())
}

/// Records the body hash of every definition a note names (its `Symbol`
/// links, up to two definitions each), so a later search can tell whether
/// the code the note describes has changed. Needs the checkout; without a
/// code index nothing is recorded and nothing is claimed.
pub fn refresh_note_code(conn: &Connection, project_id: i64, root: &Path, note_path: &str) -> Result<usize> {
    if crate::db::is_readonly(conn) {
        return Ok(0);
    }
    let Some(note_id): Option<i64> = conn.query_row("SELECT id FROM notes WHERE project_id = ?1 AND path = ?2 AND deleted_at IS NULL", params![project_id, note_path], |r| r.get(0)).optional()? else { return Ok(0) };
    conn.execute("DELETE FROM note_code WHERE note_id = ?1", [note_id])?;
    let mut names = conn.prepare_cached("SELECT target FROM links WHERE note_id = ?1 AND kind = ?2")?;
    let symbols: Vec<String> = names.query_map(params![note_id, Kind::Symbol as i64], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut ins = conn.prepare_cached("INSERT OR REPLACE INTO note_code(note_id, project_id, name, path, body_hash) VALUES (?1, ?2, ?3, ?4, ?5)")?;
    let mut n = 0;
    for sym in symbols {
        for d in crate::code::defs_named(conn, project_id, &sym)?.into_iter().take(2) {
            if let Some(h) = crate::code::body_hash(conn, project_id, root, &d)? {
                ins.execute(params![note_id, project_id, d.name, d.path, h])?;
                n += 1;
            }
        }
    }
    Ok(n)
}

/// For search: the symbols a note names whose definition body differs from
/// what it was when the note was saved, or is gone. Reads one file per
/// symbol, bounded by what the note declared.
pub fn code_changed(conn: &Connection, project_id: i64, root: &Path, note_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached("SELECT name, path, body_hash FROM note_code WHERE note_id = ?1")?;
    let rows: Vec<(String, String, Vec<u8>)> = stmt.query_map([note_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for (name, path, then) in rows {
        let now = crate::code::defs_named(conn, project_id, &name)?
            .into_iter()
            .find(|d| d.path == path)
            .map(|d| crate::code::body_hash(conn, project_id, root, &d))
            .transpose()?
            .flatten();
        if now.as_deref() != Some(then.as_slice()) && !out.contains(&name) {
            out.push(name);
        }
    }
    Ok(out)
}

/// Bump when `extract` changes what it finds; stores with older links are
/// rebuilt in one pass on the next graph command.
pub const LINKS_VERSION: &str = "4";

/// Builds links for live notes that have none (stores created before the
/// links table) or that were linked by an older extractor. One pass over
/// note text; nothing else is touched.
pub fn ensure_links(conn: &mut Connection, project_id: i64) -> Result<usize> {
    if crate::db::is_readonly(conn) {
        return Ok(0);
    }
    let key = format!("links_version:{project_id}");
    let stale = crate::db::meta_get(conn, &key)?.as_deref() != Some(LINKS_VERSION);
    let sql = if stale {
        "SELECT id, path, content FROM notes n WHERE project_id = ?1 AND deleted_at IS NULL"
    } else {
        "SELECT id, path, content FROM notes n WHERE project_id = ?1 AND deleted_at IS NULL
         AND NOT EXISTS (SELECT 1 FROM links l WHERE l.note_id = n.id)"
    };
    let todo: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([project_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if todo.is_empty() && !stale {
        return Ok(0);
    }
    let tx = conn.transaction()?;
    let mut n = 0;
    for (id, path, content) in &todo {
        n += store_links(&tx, *id, project_id, path, content)?;
    }
    crate::db::meta_set(&tx, &key, LINKS_VERSION)?;
    tx.commit()?;
    Ok(n)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NoteRef {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub saved_at: i64,
    pub dated: Option<i64>,
    pub superseded_by: Option<String>,
    pub author: String,
    /// The name the note used for the target (`StatusLayers.kt` or a full path).
    pub via: String,
}

/// Notes linked to `target` of `kind`, live ones first (newest first), then
/// superseded ones. Exact target first; if nothing matches, a path suffix
/// match (`store.rs` finds `src/store.rs`).
pub fn notes_for(conn: &Connection, project_id: i64, kind: Kind, target: &str) -> Result<Vec<NoteRef>> {
    let sql = |cmp: &str| {
        format!(
            "SELECT n.id, n.path, n.title, n.saved_at, n.superseded_by, n.source, l.target, n.dated FROM links l
             JOIN notes n ON n.id = l.note_id
             WHERE l.project_id = ?1 AND l.kind = ?2 AND {cmp} AND n.deleted_at IS NULL
             ORDER BY (n.superseded_by IS NOT NULL), n.saved_at DESC"
        )
    };
    let map = |r: &rusqlite::Row| {
        Ok(NoteRef { id: r.get(0)?, path: r.get(1)?, title: r.get(2)?, saved_at: r.get(3)?, dated: r.get(7)?, superseded_by: r.get(4)?, author: r.get(5)?, via: r.get(6)? })
    };
    if kind == Kind::Note {
        let mut stmt = conn.prepare(&sql("l.target = ?3"))?;
        let v: Vec<NoteRef> = stmt.query_map(params![project_id, kind as i64, target], map)?.collect::<rusqlite::Result<_>>()?;
        return Ok(v);
    }
    if kind == Kind::Symbol {
        // A note may write the symbol with its path (`spans::context`,
        // `store.embed_missing`); the bare name names the same thing.
        let short = target.rsplit("::").next().unwrap_or(target).rsplit('.').next().unwrap_or(target).trim_end_matches("()");
        let mut stmt = conn.prepare(&sql("(l.target = ?3 OR l.target LIKE '%::' || ?3 OR l.target LIKE '%.' || ?3)"))?;
        let mut out: Vec<NoteRef> = Vec::new();
        for r in stmt.query_map(params![project_id, kind as i64, short], map)? {
            let r = r?;
            if !out.iter().any(|o| o.id == r.id) {
                out.push(r);
            }
        }
        return Ok(out);
    }
    // A file is one thing however a note spelled it: bare name, partial or
    // full path. Match when either is a path suffix of the other on a `/`
    // boundary. Each note appears once, by its newest link.
    let t = target.trim_start_matches("./").trim_start_matches('/');
    let mut stmt = conn.prepare(&sql("(l.target = ?3 OR l.target LIKE ?4 OR ?3 LIKE '%/' || l.target)"))?;
    let suffix = format!("%/{t}");
    let mut out: Vec<NoteRef> = Vec::new();
    for r in stmt.query_map(params![project_id, kind as i64, t, suffix], map)? {
        let r = r?;
        if !out.iter().any(|o| o.id == r.id) {
            out.push(r);
        }
    }
    Ok(out)
}

/// The supersede chain through `path`: oldest first. Follows `supersedes`
/// backwards and `superseded_by` forwards, bounded.
pub fn history(conn: &Connection, project_id: i64, path: &str) -> Result<Vec<NoteRef>> {
    let one = |p: &str| -> Result<Option<(NoteRef, Option<String>)>> {
        Ok(conn
            .query_row(
                "SELECT id, path, title, saved_at, superseded_by, source, supersedes FROM notes WHERE project_id = ?1 AND path = ?2 AND deleted_at IS NULL",
                params![project_id, p],
                |r| {
                    Ok((
                        NoteRef { id: r.get(0)?, path: r.get(1)?, title: r.get(2)?, saved_at: r.get(3)?, dated: None, superseded_by: r.get(4)?, author: r.get(5)?, via: String::new() },
                        r.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()?)
    };
    use rusqlite::OptionalExtension;
    let Some((start, mut back)) = one(path)? else { return Ok(Vec::new()) };
    let mut older = Vec::new();
    let mut guard = 0;
    while let Some(p) = back.take() {
        guard += 1;
        if guard > 32 {
            break;
        }
        let Some((n, prev)) = one(&resolve_note_ref(path, &p))? else { break };
        if older.iter().any(|o: &NoteRef| o.id == n.id) || n.id == start.id {
            break;
        }
        back = prev;
        older.push(n);
    }
    older.reverse();
    let mut chain = older;
    let mut next = start.superseded_by.clone();
    chain.push(start);
    guard = 0;
    while let Some(p) = next.take() {
        guard += 1;
        if guard > 32 {
            break;
        }
        let Some((n, _)) = one(&p)? else { break };
        if chain.iter().any(|o| o.id == n.id) {
            break;
        }
        next = n.superseded_by.clone();
        chain.push(n);
    }
    Ok(chain)
}

/// `(note_id, kind, target)` for a set of notes, one indexed query. Used to
/// decorate search hits without touching the ranking path.
pub fn links_of(conn: &Connection, note_ids: &[i64]) -> Result<HashMap<i64, Vec<(Kind, String)>>> {
    let mut out: HashMap<i64, Vec<(Kind, String)>> = HashMap::new();
    if note_ids.is_empty() {
        return Ok(out);
    }
    let sql = format!("SELECT note_id, kind, target FROM links WHERE note_id IN {} ORDER BY note_id, kind, target", placeholders(1, note_ids.len()));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(note_ids.iter()), |r| {
        Ok((r.get::<_, i64>(0)?, Kind::from_i64(r.get(1)?), r.get::<_, String>(2)?))
    })?;
    for row in rows {
        let (id, k, t) = row?;
        out.entry(id).or_default().push((k, t));
    }
    Ok(out)
}

/// File targets that no longer exist under `root`: notes about files that
/// were deleted or renamed. The stale-memory detector.
pub fn dangling_files(conn: &Connection, project_id: i64, root: &Path) -> Result<Vec<(String, usize)>> {
    let mut stmt = conn.prepare(
        "SELECT l.target, count(*) FROM links l JOIN notes n ON n.id = l.note_id
         WHERE l.project_id = ?1 AND l.kind = 0 AND n.deleted_at IS NULL AND n.superseded_by IS NULL
         GROUP BY l.target ORDER BY l.target",
    )?;
    let mut out = Vec::new();
    for row in stmt.query_map([project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))? {
        let (t, n) = row?;
        if !root.join(&t).exists() {
            out.push((t, n));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- export --

#[derive(Debug, Clone, serde::Serialize)]
pub struct Node {
    pub id: u32,
    pub kind: &'static str,
    pub label: String,
    /// Notes: path. Files and symbols: the target.
    pub key: String,
    pub date: i64,
    pub superseded: bool,
    pub author: String,
    pub community: u32,
    pub degree: u32,
    /// Notes: first sentence of the Decision section (or the first body sentence), at most 160 chars.
    pub summary: String,
    /// Notes: which sections exist, bit-packed (see `SECTION_*`).
    pub sections: u8,
    /// Files and symbols: whether the checkout still has them. Notes: always true.
    pub exists: bool,
    /// Symbols: `path:line` of the definition (or first mention) in the checkout. Files: the path.
    pub loc: String,
    /// From the code index (a source file or a definition), not only from a note.
    pub code: bool,
}

pub const SECTION_CONTEXT: u8 = 1;
pub const SECTION_DECISION: u8 = 2;
pub const SECTION_REJECTED: u8 = 4;
pub const SECTION_MEASURED: u8 = 8;
pub const SECTION_OPEN: u8 = 16;

/// Section flags and the Decision summary of a note, one pass over the text.
pub fn summarise(content: &str) -> (u8, String) {
    let mut flags = 0u8;
    let mut decision = String::new();
    let mut first_body = String::new();
    let mut in_decision = false;
    let mut in_fence = false;
    let mut past_header = false;
    for line in content.lines() {
        let l = line.trim();
        if l.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(h) = l.strip_prefix("## ") {
            past_header = true;
            let h = h.trim().to_ascii_lowercase();
            in_decision = h.starts_with("decision");
            flags |= match h.split_whitespace().next().unwrap_or("") {
                "context" => SECTION_CONTEXT,
                "decision" => SECTION_DECISION,
                "rejected" => SECTION_REJECTED,
                "measured" => SECTION_MEASURED,
                "open" => SECTION_OPEN,
                _ => 0,
            };
            continue;
        }
        if !past_header || l.is_empty() || l.starts_with('#') || l.starts_with('|') {
            continue;
        }
        if in_decision && decision.is_empty() {
            decision = first_sentence(l);
        }
        if first_body.is_empty() {
            first_body = first_sentence(l);
        }
    }
    (flags, if decision.is_empty() { first_body } else { decision })
}

fn first_sentence(l: &str) -> String {
    let l = l.trim_start_matches(['-', '*', ' ']);
    let end = l
        .char_indices()
        .find(|&(i, c)| (c == '.' || c == ';') && l[i + c.len_utf8()..].starts_with([' ', '\n']) || i + c.len_utf8() == l.len() && c == '.')
        .map(|(i, _)| i)
        .unwrap_or(l.len());
    let mut s: String = l[..end].chars().take(160).collect();
    if end > 160 || l.len() > end && l[..end].chars().count() > 160 {
        s.push('…');
    }
    s
}

/// Most entries the checkout walk will record; beyond this bare names are not resolved.
const CHECKOUT_INDEX_MAX: usize = 50_000;

/// Relative paths of the checkout keyed by file name, so a note that says
/// `ChatScreen.kt` resolves to `app/src/.../ChatScreen.kt`. Skips VCS and
/// build output. One walk per `graph` run, bounded.
fn checkout_index(root: &Path) -> HashMap<String, Vec<String>> {
    const SKIP: &[&str] = &[".git", "target", "node_modules", "build", ".gradle", "dist", ".venv", "venv", "__pycache__", ".idea", ".next", "memorycastle"];
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut stack = vec![root.to_path_buf()];
    let mut count = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                if !SKIP.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push(p);
                }
            } else if ft.is_file() {
                count += 1;
                if count > CHECKOUT_INDEX_MAX {
                    return out;
                }
                if let Ok(rel) = p.strip_prefix(root) {
                    out.entry(name).or_default().push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    out
}

/// Longest file a symbol lookup will read.
const SYMBOL_SCAN_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Finds `symbol` in `text`: the line of a definition if one exists, else the
/// first mention. Definitions are matched by the keyword before the name in
/// the common languages, no parser.
fn locate_symbol(text: &str, symbol: &str) -> Option<(usize, bool)> {
    const DEFS: &[&str] = &["fn ", "pub fn ", "def ", "func ", "fun ", "class ", "struct ", "enum ", "trait ", "interface ", "type ", "function ", "const ", "static ", "val ", "var ", "let ", "object ", "impl "];
    let name = symbol.rsplit("::").next().unwrap_or(symbol).trim_end_matches("()");
    if name.is_empty() {
        return None;
    }
    let mut first: Option<usize> = None;
    for (n, line) in text.lines().enumerate() {
        let Some(i) = line.find(name) else { continue };
        let after_ok = line[i + name.len()..].chars().next().map(|c| !(c.is_alphanumeric() || c == '_')).unwrap_or(true);
        let before_ok = line[..i].chars().next_back().map(|c| !(c.is_alphanumeric() || c == '_')).unwrap_or(true);
        if !after_ok || !before_ok {
            continue;
        }
        if first.is_none() {
            first = Some(n + 1);
        }
        let head = line[..i].trim_start();
        let head = head.trim_start_matches("pub(crate) ").trim_start_matches("pub ").trim_start_matches("async ").trim_start_matches("export ").trim_start_matches("private ").trim_start_matches("public ").trim_start_matches("override ").trim_start_matches("suspend ");
        if DEFS.iter().any(|d| head.ends_with(d) || head == d.trim_end()) || line[i..].starts_with(&format!("{name}(")) && head.ends_with("fn ") {
            return Some((n + 1, true));
        }
    }
    first.map(|n| (n, false))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Edge {
    pub src: u32,
    pub dst: u32,
    pub kind: &'static str,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub communities: u32,
    /// Checkout root the file and symbol facts were checked against ("" if none).
    pub root: String,
    /// The code layer (files, definitions, imports, mentions) is present.
    pub has_code: bool,
    /// Community labels when areas are directories (code layer present).
    pub community_names: Vec<String>,
}

/// Builds the project graph from `links`. Index-based: nodes in one array,
/// edges as `(u32, u32)`, communities by union-find over all edges.
pub fn build(conn: &Connection, project_id: i64, root: Option<&Path>) -> Result<Graph> {
    let mut g = Graph { root: root.map(|r| r.to_string_lossy().into_owned()).unwrap_or_default(), ..Graph::default() };
    let mut index: HashMap<(u8, String), u32> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT id, path, title, saved_at, superseded_by IS NOT NULL, source, content FROM notes WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY saved_at",
    )?;
    let mut note_ids: HashMap<i64, u32> = HashMap::new();
    for row in stmt.query_map([project_id], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, bool>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?))
    })? {
        let (id, path, title, date, superseded, author, content) = row?;
        let nid = g.nodes.len() as u32;
        index.insert((1, path.clone()), nid);
        note_ids.insert(id, nid);
        let (sections, summary) = summarise(&content);
        g.nodes.push(Node { id: nid, kind: "note", label: title, key: path.clone(), date, superseded, author, community: 0, degree: 0, summary, sections, exists: true, loc: path, code: false });
    }
    let mut stmt = conn.prepare(
        "SELECT l.note_id, l.kind, l.target FROM links l JOIN notes n ON n.id = l.note_id WHERE l.project_id = ?1 AND n.deleted_at IS NULL",
    )?;
    for row in stmt.query_map([project_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))? {
        let (note_id, kind, target) = row?;
        let Some(&src) = note_ids.get(&note_id) else { continue };
        let kind = Kind::from_i64(kind);
        let key = (kind as u8, target.clone());
        let dst = match index.get(&key) {
            Some(&d) => d,
            None => {
                if kind == Kind::Note {
                    // Reference to a note that is not in the store: skip, no dangling node.
                    continue;
                }
                let nid = g.nodes.len() as u32;
                index.insert(key, nid);
                let loc = if kind == Kind::File { target.clone() } else { String::new() };
                g.nodes.push(Node { id: nid, kind: kind.name(), label: target.clone(), key: target, date: 0, superseded: false, author: String::new(), community: 0, degree: 0, summary: String::new(), sections: 0, exists: true, loc, code: false });
                nid
            }
        };
        g.nodes[src as usize].degree += 1;
        g.nodes[dst as usize].degree += 1;
        g.edges.push(Edge { src, dst, kind: kind.name() });
    }
    // Against the checkout: does each file still exist; where is each symbol
    // defined. A symbol is looked for only in the files named by the notes
    // that mention it, so the scan is bounded by the notes, not the repo.
    if let Some(root) = root {
        let n = g.nodes.len();
        let mut by_name: Option<HashMap<String, Vec<String>>> = None;
        for i in 0..n {
            if g.nodes[i].kind != "file" {
                continue;
            }
            let key = g.nodes[i].key.clone();
            if root.join(&key).exists() {
                g.nodes[i].exists = true;
                continue;
            }
            // A bare name, or a path written from memory: find it by file name.
            let name = key.rsplit('/').next().unwrap_or(&key).to_string();
            let idx = by_name.get_or_insert_with(|| checkout_index(root));
            match idx.get(&name) {
                Some(paths) if paths.len() == 1 || key.contains('/') => {
                    let pick = paths.iter().find(|p| p.ends_with(&key)).or_else(|| if paths.len() == 1 { paths.first() } else { None });
                    match pick {
                        Some(p) => {
                            g.nodes[i].exists = true;
                            g.nodes[i].loc = p.clone();
                        }
                        None => g.nodes[i].exists = false,
                    }
                }
                _ => g.nodes[i].exists = false,
            }
        }
        // note -> files, from the edges already built.
        let mut note_files: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut symbol_notes: HashMap<u32, Vec<u32>> = HashMap::new();
        for e in &g.edges {
            match e.kind {
                "file" => note_files.entry(e.src).or_default().push(e.dst),
                "symbol" => symbol_notes.entry(e.dst).or_default().push(e.src),
                _ => {}
            }
        }
        let mut texts: HashMap<u32, Option<String>> = HashMap::new();
        let mut in_edges: Vec<Edge> = Vec::new();
        for (&sym, notes) in &symbol_notes {
            let mut best: Option<(u32, usize, bool)> = None;
            for note in notes {
                for &f in note_files.get(note).map(|v| v.as_slice()).unwrap_or(&[]) {
                    if !g.nodes[f as usize].exists {
                        continue;
                    }
                    let text = texts.entry(f).or_insert_with(|| {
                        let p = root.join(&g.nodes[f as usize].loc);
                        match std::fs::metadata(&p) {
                            Ok(m) if m.is_file() && m.len() <= SYMBOL_SCAN_MAX_BYTES => std::fs::read_to_string(&p).ok(),
                            _ => None,
                        }
                    });
                    let Some(text) = text else { continue };
                    if let Some((line, is_def)) = locate_symbol(text, &g.nodes[sym as usize].key) {
                        let better = match best {
                            None => true,
                            Some((_, _, had_def)) => is_def && !had_def,
                        };
                        if better {
                            best = Some((f, line, is_def));
                        }
                    }
                }
            }
            match best {
                Some((f, line, _)) => {
                    let loc = format!("{}:{line}", g.nodes[f as usize].loc);
                    let node = &mut g.nodes[sym as usize];
                    node.exists = true;
                    node.loc = loc;
                    in_edges.push(Edge { src: sym, dst: f, kind: "in" });
                }
                None => g.nodes[sym as usize].exists = false,
            }
        }
        for e in in_edges {
            g.nodes[e.src as usize].degree += 1;
            g.nodes[e.dst as usize].degree += 1;
            g.edges.push(e);
        }
    }
    // Code layer, when the checkout has been indexed: every source file, every
    // definition, imports and mentions. Note symbols merge with definitions
    // of the same name (preferring a file the note names), so a note's
    // "what did we decide about X" sits beside "where is X and who uses it".
    if crate::code::has_index(conn, project_id)? {
        g.has_code = true;
        let mut stmt = conn.prepare("SELECT path FROM code_files WHERE project_id = ?1 ORDER BY path")?;
        for row in stmt.query_map([project_id], |r| r.get::<_, String>(0))? {
            let path = row?;
            let key = (Kind::File as u8, path.clone());
            if let Some(&existing) = index.get(&key) {
                g.nodes[existing as usize].code = true;
                continue;
            }
            // A note may have named this file by a resolved alias; map that node instead of duplicating.
            if let Some(alias) = g.nodes.iter().position(|n| n.kind == "file" && n.loc == path) {
                g.nodes[alias].code = true;
                index.insert(key, alias as u32);
                continue;
            }
            let nid = g.nodes.len() as u32;
            index.insert(key, nid);
            g.nodes.push(Node { id: nid, kind: "file", label: path.clone(), key: path.clone(), date: 0, superseded: false, author: String::new(), community: 0, degree: 0, summary: String::new(), sections: 0, exists: true, loc: path, code: true });
        }
        let file_node = |index: &HashMap<(u8, String), u32>, path: &str| index.get(&(Kind::File as u8, path.to_string())).copied();
        // Definitions: one node per (name, file). Note symbols with that name merge in.
        let mut def_nodes: HashMap<(String, String), u32> = HashMap::new();
        let mut stmt = conn.prepare("SELECT path, name, kind, line, end_line FROM code_defs WHERE project_id = ?1 ORDER BY path, line")?;
        for row in stmt.query_map([project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?)))? {
            let (path, name, kind, line, end) = row?;
            let Some(f) = file_node(&index, &path) else { continue };
            let loc = if end > line { format!("{path}:{line}-{end}") } else { format!("{path}:{line}") };
            // Merge with a note symbol of the same short name if it has no location or points at this file.
            let short_key = (Kind::Symbol as u8, name.clone());
            let merged = index.get(&short_key).copied().filter(|&sid| {
                let sn = &g.nodes[sid as usize];
                sn.loc.is_empty() || sn.loc.starts_with(&format!("{path}:"))
            });
            let nid = match merged {
                Some(sid) => {
                    let sn = &mut g.nodes[sid as usize];
                    sn.exists = true;
                    sn.loc = loc;
                    sn.code = true;
                    sn.summary = kind.clone();
                    sid
                }
                None => {
                    let nid = g.nodes.len() as u32;
                    g.nodes.push(Node { id: nid, kind: "symbol", label: name.clone(), key: name.clone(), date: 0, superseded: false, author: String::new(), community: 0, degree: 0, summary: kind.clone(), sections: 0, exists: true, loc, code: true });
                    nid
                }
            };
            def_nodes.insert((name, path), nid);
            if !g.edges.iter().any(|e| e.src == nid && e.dst == f && e.kind == "in") {
                g.nodes[nid as usize].degree += 1;
                g.nodes[f as usize].degree += 1;
                g.edges.push(Edge { src: nid, dst: f, kind: "in" });
            }
        }
        let mut stmt = conn.prepare("SELECT src, dst FROM code_imports WHERE project_id = ?1")?;
        for row in stmt.query_map([project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (src, dst) = row?;
            if let (Some(a), Some(b)) = (file_node(&index, &src), file_node(&index, &dst)) {
                g.nodes[a as usize].degree += 1;
                g.nodes[b as usize].degree += 1;
                g.edges.push(Edge { src: a, dst: b, kind: "imports" });
            }
        }
        // Mentions: file -> definition node(s) of that name. A name defined in
        // several files links to each; the viewer hides this layer by default.
        let mut by_name: HashMap<&str, Vec<u32>> = HashMap::new();
        for ((name, _), &nid) in &def_nodes {
            by_name.entry(name.as_str()).or_default().push(nid);
        }
        let mut stmt = conn.prepare("SELECT src, name FROM code_mentions WHERE project_id = ?1")?;
        for row in stmt.query_map([project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (src, name) = row?;
            let Some(a) = file_node(&index, &src) else { continue };
            if let Some(targets) = by_name.get(name.as_str()) {
                for &b in targets.iter().take(4) {
                    g.nodes[a as usize].degree += 1;
                    g.nodes[b as usize].degree += 1;
                    g.edges.push(Edge { src: a, dst: b, kind: "mentions" });
                }
            }
        }
    }
    // Communities: connected components over note-file and note-note edges.
    // Symbols are too promiscuous (`id`, `Result`) to define areas.
    let n = g.nodes.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    let mut comm: HashMap<u32, u32> = HashMap::new();
    if g.has_code {
        // With the code layer, areas are directories: deterministic, named,
        // and what a reader expects. Files take their first two path segments;
        // notes take the area most of their files are in; symbols their file's.
        let mut dirs: HashMap<String, u32> = HashMap::new();
        let mut names: Vec<String> = Vec::new();
        for i in 0..n {
            if g.nodes[i].kind != "file" {
                continue;
            }
            let p = &g.nodes[i].loc;
            let area: String = p.split('/').take(p.matches('/').count().min(2)).collect::<Vec<_>>().join("/");
            let area = if area.is_empty() { ".".to_string() } else { area };
            let next = dirs.len() as u32;
            let c = *dirs.entry(area.clone()).or_insert_with(|| {
                names.push(area);
                next
            });
            g.nodes[i].community = c;
            comm.insert(i as u32, c);
        }
        let mut votes: HashMap<u32, HashMap<u32, u32>> = HashMap::new();
        for e in &g.edges {
            if e.kind == "file" && g.nodes[e.dst as usize].kind == "file" {
                *votes.entry(e.src).or_default().entry(g.nodes[e.dst as usize].community).or_default() += 1;
            }
        }
        for (note, v) in votes {
            if let Some((&c, _)) = v.iter().max_by_key(|(c, n)| (**n, std::cmp::Reverse(**c))) {
                g.nodes[note as usize].community = c;
            }
        }
        g.community_names = names;
        g.communities = dirs.len() as u32;
    } else {
        for e in &g.edges {
            if e.kind == "symbol" || e.kind == "in" {
                continue;
            }
            let (a, b) = (find(&mut parent, e.src), find(&mut parent, e.dst));
            if a != b {
                parent[a as usize] = b;
            }
        }
        for i in 0..n {
            if g.nodes[i].kind == "symbol" {
                continue;
            }
            let root = find(&mut parent, i as u32);
            let next = comm.len() as u32;
            let c = *comm.entry(root).or_insert(next);
            g.nodes[i].community = c;
        }
        g.communities = comm.len() as u32;
    }
    // A symbol inherits the community of its file ("in" edge) or, failing
    // that, of the first note that names it.
    for e in &g.edges {
        if e.kind == "in" {
            let c = g.nodes[e.dst as usize].community;
            g.nodes[e.src as usize].community = c;
        }
    }
    for e in &g.edges {
        if e.kind == "symbol" && g.nodes[e.dst as usize].kind == "symbol" && g.nodes[e.dst as usize].loc.is_empty() {
            let c = g.nodes[e.src as usize].community;
            g.nodes[e.dst as usize].community = c;
        }
    }
    Ok(g)
}

fn tsv_escape(s: &str) -> String {
    s.replace(['\t', '\n'], " ")
}

/// Definitions the HTML keeps without `--full`: named by a note, or used from
/// at least this many other files. The TSVs always hold everything.
pub const HTML_MIN_USERS: usize = 3;
/// Mention edges kept per file in the HTML, most frequent first.
pub const HTML_MENTIONS_PER_FILE: usize = 12;

/// The navigation subset of a graph for the viewer: all notes and files, the
/// definitions that matter, and a bounded number of mention edges. Node ids
/// are renumbered to stay contiguous.
pub fn navigation_view(g: &Graph) -> Graph {
    let n = g.nodes.len();
    let mut users: Vec<u32> = vec![0; n];
    let mut note_touched: Vec<bool> = vec![false; n];
    for e in &g.edges {
        if e.kind == "mentions" {
            users[e.dst as usize] += 1;
        }
        if g.nodes[e.src as usize].kind == "note" {
            note_touched[e.dst as usize] = true;
        }
    }
    let keep: Vec<bool> = (0..n)
        .map(|i| {
            let nd = &g.nodes[i];
            nd.kind != "symbol" || !nd.code || note_touched[i] || users[i] as usize >= HTML_MIN_USERS
        })
        .collect();
    let mut remap: Vec<u32> = vec![u32::MAX; n];
    let mut out = Graph { root: g.root.clone(), has_code: g.has_code, community_names: g.community_names.clone(), communities: g.communities, ..Graph::default() };
    for i in 0..n {
        if keep[i] {
            remap[i] = out.nodes.len() as u32;
            let mut nd = g.nodes[i].clone();
            nd.id = remap[i];
            nd.degree = 0;
            out.nodes.push(nd);
        }
    }
    // Mention edges: per source file, the most useful few (by target popularity).
    let mut per_file: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    for e in &g.edges {
        let (a, b) = (remap[e.src as usize], remap[e.dst as usize]);
        if a == u32::MAX || b == u32::MAX {
            continue;
        }
        if e.kind == "mentions" {
            per_file.entry(a).or_default().push((users[e.dst as usize], b));
            continue;
        }
        out.nodes[a as usize].degree += 1;
        out.nodes[b as usize].degree += 1;
        out.edges.push(Edge { src: a, dst: b, kind: e.kind });
    }
    for (a, mut v) in per_file {
        v.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
        for (_, b) in v.into_iter().take(HTML_MENTIONS_PER_FILE) {
            out.nodes[a as usize].degree += 1;
            out.nodes[b as usize].degree += 1;
            out.edges.push(Edge { src: a, dst: b, kind: "mentions" });
        }
    }
    out
}

/// Writes `nodes.tsv`, `edges.tsv` (everything) and `graph.html` (the
/// navigation view, or everything with `full`) into `dir`. Returns bytes written.
pub fn write(g: &Graph, dir: &Path, full: bool) -> Result<usize> {
    std::fs::create_dir_all(dir)?;
    let mut nodes = String::with_capacity(g.nodes.len() * 64);
    nodes.push_str("id\tkind\tlabel\tkey\tdate\tsuperseded\tauthor\tcommunity\tdegree\tsections\texists\tloc\tcode\tsummary\n");
    for n in &g.nodes {
        let _ = writeln!(nodes, "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", n.id, n.kind, tsv_escape(&n.label), tsv_escape(&n.key), n.date, n.superseded as u8, tsv_escape(&n.author), n.community, n.degree, n.sections, n.exists as u8, tsv_escape(&n.loc), n.code as u8, tsv_escape(&n.summary));
    }
    let mut edges = String::with_capacity(g.edges.len() * 16);
    edges.push_str("src\tdst\tkind\n");
    for e in &g.edges {
        let _ = writeln!(edges, "{}\t{}\t{}", e.src, e.dst, e.kind);
    }
    let view;
    let for_html = if full || !g.has_code {
        g
    } else {
        view = navigation_view(g);
        &view
    };
    let html = HTML.replace("/*DATA*/", &serde_json::to_string(for_html)?);
    std::fs::write(dir.join("nodes.tsv"), &nodes)?;
    std::fs::write(dir.join("edges.tsv"), &edges)?;
    std::fs::write(dir.join("graph.html"), &html)?;
    Ok(nodes.len() + edges.len() + html.len())
}

/// One self-contained page: inline data, canvas force layout, no network.
const HTML: &str = include_str!("graph.html");

/// Files and symbols a question names: backticked or bare tokens that look
/// like a path or an identifier. The graph turns these into rerank signals.
pub fn query_entities(query: &str) -> (Vec<String>, Vec<String>) {
    let mut files = Vec::new();
    let mut symbols = Vec::new();
    for raw in query.split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '(' || c == ')' || c == '"' || c == '\'' || c == '`') {
        let t = raw.trim_matches(|c: char| c == '.' || c == ':' || c == '?' || c == '!');
        if t.is_empty() {
            continue;
        }
        if looks_like_path(t) {
            files.push(t.trim_start_matches("./").to_string());
        } else if looks_like_symbol(t) && t.len() >= 3 {
            symbols.push(t.to_string());
        }
    }
    (files, symbols)
}

/// True when `link` and `file` name the same file: equal, or one is a path
/// suffix of the other on a `/` boundary.
pub fn same_file(link: &str, file: &str) -> bool {
    link == file || link.ends_with(&format!("/{file}")) || file.ends_with(&format!("/{link}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_declared_edges_only() {
        let note = "# T\nAuthor: a+b | Date: 2026-10-18\nFiles: src/store.rs, src/chunk.rs\nSupersedes: journal/2026-10-01-old.md\n\n## Decision\nUse `embed_missing` and `Scope::single`; see memorycastle/journal/2026-10-02-x.md and `docs/plan.md`. The word `rotated` is prose.\n```\n`not_a_symbol_in_code`\n```\n";
        let l = extract("memorycastle/journal/2026-10-18-t.md", note);
        assert!(l.contains(&(Kind::File, "src/store.rs".into())));
        assert!(l.contains(&(Kind::File, "src/chunk.rs".into())));
        assert!(l.contains(&(Kind::File, "docs/plan.md".into())));
        assert!(l.contains(&(Kind::Note, "memorycastle/journal/2026-10-01-old.md".into())), "short form resolves");
        assert!(l.contains(&(Kind::Note, "memorycastle/journal/2026-10-02-x.md".into())));
        assert!(l.contains(&(Kind::Symbol, "embed_missing".into())));
        assert!(l.contains(&(Kind::Symbol, "Scope::single".into())));
        assert!(!l.iter().any(|(_, t)| t == "rotated"), "plain words are not symbols");
        assert!(!l.iter().any(|(_, t)| t == "not_a_symbol_in_code"), "fenced code is skipped");
    }

    #[test]
    fn summary_and_sections() {
        let (flags, summary) = summarise("# T\nFiles: a\n\n## Context\nWhy. More.\n## Decision\nOne chunk per call, longest first. Then more.\n## Measured\n| t | 1 |\n");
        assert_eq!(flags, SECTION_CONTEXT | SECTION_DECISION | SECTION_MEASURED);
        assert_eq!(summary, "One chunk per call, longest first");
        let (flags, summary) = summarise("# T\n## Open\n- what remains\n");
        assert_eq!(flags, SECTION_OPEN);
        assert_eq!(summary, "what remains");
    }

    #[test]
    fn symbol_location_prefers_definitions() {
        let text = "use x::embed_missing;\n\npub fn embed_missing(a: i32) {}\nfn other() { embed_missing(1) }\n";
        assert_eq!(locate_symbol(text, "embed_missing"), Some((3, true)));
        assert_eq!(locate_symbol(text, "other"), Some((4, true)));
        assert_eq!(locate_symbol(text, "Scope::single"), None);
        assert_eq!(locate_symbol("let a = embed_missingx;\ncall(embed_missing)\n", "embed_missing"), Some((2, false)));
        assert!(!looks_like_path("memorycastle/graph/{nodes,edges}.tsv"));
        assert!(looks_like_path("ChatScreen.kt") && looks_like_path("Cargo.toml") && looks_like_path("ui/status/") && looks_like_path("/opt/app/config/app.env"));
        assert!(!looks_like_path("/api/debug/supervision") && !looks_like_path("FetchRequest.after") && !looks_like_path("RecentChatMemoryCache.put"));
        assert!(looks_like_symbol("FetchRequest.after"));
        assert!(looks_like_symbol("embed_missing") && looks_like_symbol("Scope::single") && looks_like_symbol("RecentChatMemoryCache.put"));
        assert!(!looks_like_symbol("MEMORYCASTLE_MODEL=q4") && !looks_like_symbol("::1") && !looks_like_symbol("2400:d321::dc9") && !looks_like_symbol("2026-10-18"));
        assert!(looks_like_path("src/store.rs"));
    }

    #[test]
    fn note_refs_resolve_relative_to_the_note() {
        assert_eq!(resolve_note_ref("memorycastle/journal/a.md", "b.md"), "memorycastle/journal/b.md");
        assert_eq!(resolve_note_ref("memorycastle/journal/a.md", "journal/b.md"), "memorycastle/journal/b.md");
        assert_eq!(resolve_note_ref("journal/a.md", "journal/b.md"), "journal/b.md");
        assert_eq!(resolve_note_ref("journal/a.md", "`journal/b.md`."), "journal/b.md");
    }
}
