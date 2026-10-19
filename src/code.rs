//! A graph of the checkout itself: files, definitions, imports and mentions,
//! found by line scanning with a keyword list, no parser and no LLM. Derived,
//! incremental (files are keyed by content hash), bounded, and never on the
//! search path. Edges are labelled for what they are: `imports` is resolved
//! from import lines, `mentions` is a name used in a file that defines it
//! elsewhere; neither claims to be a call graph.

use crate::error::Result;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Source extensions indexed. Everything else is a file node only if a note names it.
pub const CODE_EXTS: &[&str] = &["rs", "kt", "kts", "java", "py", "ts", "tsx", "js", "jsx", "mjs", "go", "c", "h", "cc", "cpp", "hpp", "swift", "rb", "php", "cs"];
/// Largest source file read.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Most files indexed per checkout.
pub const MAX_FILES: usize = 20_000;
/// Most mention rows kept per file (the most frequent names win).
pub const MAX_MENTIONS_PER_FILE: usize = 200;
/// Shortest name that can be a mention; shorter ones (`id`, `run`) are noise.
pub const MIN_MENTION_LEN: usize = 4;
/// A name defined in more files than this (`from`, `new`, `state`) says
/// nothing about structure and is left out of mention edges.
pub const PROMISCUOUS_DEFS: usize = 20;
/// A name used from more files than this (`Result`, `error`, `value`) is
/// vocabulary, not structure; its mention rows are dropped after indexing.
pub const MAX_MENTION_USERS: usize = 100;
/// Literals and self-references that a keyword scan can mistake for names.
pub const NAME_STOPLIST: &[&str] = &["true", "false", "null", "nil", "None", "self", "this", "super", "Self"];
/// Bump when the scanners change; stores indexed by an older scanner are rebuilt once.
pub const CODE_VERSION: &str = "9";
/// Variants indexed per enum; a bigger enum is a table, not navigation.
pub const MAX_VARIANTS_PER_ENUM: usize = 64;
/// Longest definition span followed when finding an end line.
pub const MAX_SPAN_LINES: usize = 2000;

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", "build", ".gradle", "dist", ".venv", "venv", "__pycache__", ".idea", ".next", "memorycastle", "vendor", "third_party", "archive"];

#[derive(Debug, Default, serde::Serialize)]
pub struct Stats {
    pub files_seen: usize,
    pub files_indexed: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
    pub defs: usize,
    pub imports: usize,
    pub mentions: usize,
    pub ms: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Def {
    pub path: String,
    pub name: String,
    pub kind: &'static str,
    pub line: u32,
    /// Last line of the definition (brace depth, or indentation for Python).
    pub end: u32,
    /// The definition line's text, trimmed, to re-find the span after edits.
    pub anchor: String,
}

/// Walks the checkout and brings `code_*` tables up to date. Unchanged files
/// (same content hash) are skipped; files that vanished are removed.
pub fn index(conn: &mut Connection, project_id: i64, root: &Path) -> Result<Stats> {
    let t = std::time::Instant::now();
    let mut st = Stats::default();
    let vkey = format!("code_version:{project_id}");
    if crate::db::meta_get(conn, &vkey)?.as_deref() != Some(CODE_VERSION) {
        conn.execute("DELETE FROM code_files WHERE project_id = ?1", [project_id])?;
        crate::db::meta_set(conn, &vkey, CODE_VERSION)?;
    }
    let files = walk(root);
    st.files_seen = files.len();
    let known: HashMap<String, (Vec<u8>, i64, i64)> = {
        let mut stmt = conn.prepare("SELECT path, hash, size, mtime FROM code_files WHERE project_id = ?1")?;
        let rows = stmt.query_map([project_id], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, Vec<u8>>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let present: HashSet<&String> = files.iter().collect();
    // Pass 1: hashes and definitions. Pass 2 needs the complete set of names.
    let mut changed: Vec<(String, String)> = Vec::new(); // (path, text)
    let mut hashes: Vec<(String, Vec<u8>, i64, i64)> = Vec::new();
    for rel in &files {
        let full = root.join(rel);
        let Ok(md) = std::fs::metadata(&full) else { continue };
        if md.len() > MAX_FILE_BYTES {
            continue;
        }
        let size = md.len() as i64;
        let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(-1);
        // Same size and mtime as last time: unchanged without reading it.
        if known.get(rel).is_some_and(|(_, s, m)| *s == size && *m == mtime && mtime >= 0) {
            st.files_unchanged += 1;
            continue;
        }
        let Ok(bytes) = std::fs::read(&full) else { continue };
        let hash = Sha256::digest(&bytes).to_vec();
        if known.get(rel).map(|(h, _, _)| *h == hash).unwrap_or(false) {
            st.files_unchanged += 1;
            // Record the stat so the next refresh skips the read.
            hashes.push((rel.clone(), hash, size, mtime));
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else { continue };
        hashes.push((rel.clone(), hash, size, mtime));
        changed.push((rel.clone(), text));
    }
    let tx = conn.transaction()?;
    for path in known.keys() {
        if !present.contains(path) {
            tx.execute("DELETE FROM code_files WHERE project_id = ?1 AND path = ?2", params![project_id, path])?;
            st.files_removed += 1;
        }
    }
    // Basename index for import resolution, over all present files.
    let mut by_name: HashMap<&str, Vec<&String>> = HashMap::new();
    for f in &files {
        by_name.entry(f.rsplit('/').next().unwrap_or(f)).or_default().push(f);
    }
    // Files whose content was unchanged but whose stat was unknown: remember the stat.
    for (path, _, size, mtime) in hashes.iter().filter(|(p, ..)| !changed.iter().any(|(c, _)| c == p)) {
        tx.execute("UPDATE code_files SET size = ?3, mtime = ?4 WHERE project_id = ?1 AND path = ?2", params![project_id, path, size, mtime])?;
    }
    for (path, text) in &changed {
        tx.execute("DELETE FROM code_files WHERE project_id = ?1 AND path = ?2", params![project_id, path])?;
        let (_, hash, size, mtime) = hashes.iter().find(|(p, ..)| p == path).unwrap();
        tx.execute(
            "INSERT INTO code_files(project_id, path, hash, lang, size, mtime) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![project_id, path, hash, ext_of(path), size, mtime],
        )?;
        let mut ins = tx.prepare_cached("INSERT INTO code_defs(project_id, path, name, kind, line, end_line, anchor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")?;
        for d in definitions(path, text) {
            ins.execute(params![project_id, path, d.name, d.kind, d.line, d.end, d.anchor])?;
            st.defs += 1;
        }
        let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO code_imports(project_id, src, dst) VALUES (?1, ?2, ?3)")?;
        for dst in imports(path, text, &by_name) {
            ins.execute(params![project_id, path, dst])?;
            st.imports += 1;
        }
        st.files_indexed += 1;
    }
    // Pass 2: mentions, for changed files, against every known definition name.
    // Names worth a mention edge: defined somewhere, in few enough files to mean something.
    let names: HashMap<String, usize> = {
        let mut stmt = tx.prepare("SELECT name, count(DISTINCT path) FROM code_defs WHERE project_id = ?1 GROUP BY name HAVING count(DISTINCT path) <= ?2")?;
        let rows = stmt.query_map(params![project_id, PROMISCUOUS_DEFS as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    {
        let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO code_mentions(project_id, src, name, n) VALUES (?1, ?2, ?3, ?4)")?;
        for (path, text) in &changed {
            let own: HashSet<String> = definitions(path, text).into_iter().map(|d| d.name).collect();
            let mut counts: HashMap<&str, u32> = HashMap::new();
            for tok in identifiers(text) {
                if tok.len() >= MIN_MENTION_LEN && names.contains_key(tok) && !own.contains(tok) {
                    *counts.entry(tok).or_default() += 1;
                }
            }
            let mut v: Vec<(&str, u32)> = counts.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            for (name, n) in v.into_iter().take(MAX_MENTIONS_PER_FILE) {
                ins.execute(params![project_id, path, name, n])?;
                st.mentions += 1;
            }
        }
    }
    if !changed.is_empty() {
        st.mentions -= tx.execute(
            "DELETE FROM code_mentions WHERE project_id = ?1 AND name IN (SELECT name FROM code_mentions WHERE project_id = ?1 GROUP BY name HAVING count(*) > ?2)",
            params![project_id, MAX_MENTION_USERS as i64],
        )?.min(st.mentions);
    }
    tx.commit()?;
    st.ms = t.elapsed().as_secs_f64() * 1000.0;
    Ok(st)
}

fn ext_of(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or("")
}

/// One `.gitignore` line as this walker understands it: a bare name
/// (any depth), a rooted path (`/foo/bar`), a trailing `/` (directories
/// only) and a leading `*.` extension glob. Negations and other globs are
/// ignored: a vendored tree that git ignores is the case this exists for.
#[derive(Debug, Clone)]
struct Ignore {
    pat: String,
    rooted: bool,
    dir_only: bool,
    ext: bool,
}

fn parse_ignores(dir: &Path, rel_dir: &str) -> Vec<Ignore> {
    let Ok(text) = std::fs::read_to_string(dir.join(".gitignore")) else { return Vec::new() };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .filter_map(|l| {
            let dir_only = l.ends_with('/');
            let l = l.trim_end_matches('/');
            if let Some(e) = l.strip_prefix("*.") {
                return (!e.contains(['*', '/', '?'])).then(|| Ignore { pat: e.to_string(), rooted: false, dir_only, ext: true });
            }
            if l.contains(['*', '?', '[']) {
                return None;
            }
            let rooted = l.starts_with('/') || l.contains('/');
            let pat = if rooted {
                let p = l.trim_start_matches('/');
                if rel_dir.is_empty() { p.to_string() } else { format!("{rel_dir}/{p}") }
            } else {
                l.to_string()
            };
            Some(Ignore { pat, rooted, dir_only, ext: false })
        })
        .collect()
}

fn ignored(rules: &[Ignore], rel: &str, name: &str, is_dir: bool) -> bool {
    rules.iter().any(|r| {
        if r.dir_only && !is_dir {
            return false;
        }
        if r.ext {
            return !is_dir && ext_of(name) == r.pat;
        }
        if r.rooted { rel == r.pat } else { name == r.pat }
    })
}

/// Source files under `root`, relative, bounded, skipping VCS and build
/// output and whatever `.gitignore` files (root and nested) say to skip.
pub fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), parse_ignores(root, ""))];
    while let Some((dir, rules)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(ft) = entry.file_type() else { continue };
            let rel = p.strip_prefix(root).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
            if ignored(&rules, &rel, &name, ft.is_dir()) {
                continue;
            }
            if ft.is_dir() {
                if !SKIP_DIRS.contains(&name.as_str()) && !name.starts_with('.') {
                    let mut inner = rules.clone();
                    inner.extend(parse_ignores(&p, &rel));
                    stack.push((p, inner));
                }
            } else if ft.is_file() && CODE_EXTS.contains(&ext_of(&name)) {
                if out.len() >= MAX_FILES {
                    return out;
                }
                if let Ok(rel) = p.strip_prefix(root) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    out.sort();
    out
}

const DEF_KEYWORDS: &[(&str, &str)] = &[
    ("fn", "fn"), ("def", "fn"), ("func", "fn"), ("fun", "fn"), ("function", "fn"),
    ("class", "type"), ("struct", "type"), ("enum", "type"), ("trait", "type"), ("interface", "type"), ("object", "type"), ("type", "type"), ("protocol", "type"), ("typealias", "type"),
    ("const", "const"), ("static", "const"), ("val", "val"), ("macro_rules!", "fn"),
];
const MODIFIERS: &[&str] = &["pub", "pub(crate)", "pub(super)", "async", "unsafe", "extern", "export", "default", "private", "public", "protected", "internal", "override", "open", "abstract", "final", "suspend", "inline", "static", "data", "sealed", "const", "lateinit", "companion", "declare", "readonly"];

/// Definitions in one file: keyword-before-name on each line. Top-level or
/// not makes no difference; a method is a definition too.
pub fn definitions(path: &str, text: &str) -> Vec<Def> {
    let mut out = Vec::new();
    let mut in_block_comment = false;
    for (n, raw) in text.lines().enumerate() {
        let l = raw.trim_start();
        if in_block_comment {
            if l.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if l.starts_with("/*") && !l.contains("*/") {
            in_block_comment = true;
            continue;
        }
        if l.is_empty() || l.starts_with("//") || l.starts_with('#') && !l.starts_with("#[") || l.starts_with('*') {
            continue;
        }
        // Keyword after modifiers only, or right after an opening brace on
        // the same line (`impl Foo { fn method`). Elsewhere a keyword is prose.
        let words: Vec<&str> = l.split_whitespace().collect();
        let mut kw = None;
        let mut name = "";
        let mut ok = true;
        for (i, w) in words.iter().enumerate().take(8) {
            let bare = w.trim_end_matches([':', '(', '{']);
            // `const` and `static` are keywords when a name follows, modifiers when another keyword does.
            let next_bare = words.get(i + 1).map(|n| n.trim_end_matches([':', '(', '{'])).unwrap_or("");
            let next_is_kw = DEF_KEYWORDS.iter().any(|(k, _)| *k == next_bare) || MODIFIERS.contains(&next_bare);
            if w.starts_with('@') || w.starts_with("#[") || (MODIFIERS.contains(&bare) && (next_is_kw || !DEF_KEYWORDS.iter().any(|(k, _)| *k == bare))) {
                continue;
            }
            if let Some((_, kind)) = DEF_KEYWORDS.iter().find(|(k, _)| *k == bare) {
                if ok {
                    kw = Some(*kind);
                    name = words.get(i + 1).copied().unwrap_or("");
                }
                break;
            }
            // Something else before the keyword: allowed only if it opened a brace.
            ok = w.ends_with('{') || w.ends_with(';');
        }
        let Some(kind) = kw else { continue };
        let name = def_name(name);
        let name = name.as_str();
        if name.is_empty() || !name.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false) || NAME_STOPLIST.contains(&name) {
            continue;
        }
        // `val x =` in Kotlin and `const X =` are definitions; `let` is not (locals).
        out.push(Def { path: path.to_string(), name: name.to_string(), kind, line: n as u32 + 1, end: 0, anchor: l.chars().take(160).collect() });
    }
    let lines: Vec<&str> = text.lines().collect();
    let py = ext_of(path) == "py";
    for d in out.iter_mut() {
        d.end = span_end(&lines, d.line as usize - 1, py) as u32 + 1;
    }
    // A `val`/`const` indented inside a function body is a local, not a
    // definition: a note naming `needsFrost` means the top-level one.
    let indent = |i: usize| lines.get(i).map(|l| l.len() - l.trim_start().len()).unwrap_or(0);
    let fns: Vec<(u32, u32, usize)> = out.iter().filter(|d| d.kind == "fn").map(|d| (d.line, d.end, indent(d.line as usize - 1))).collect();
    out.retain(|d| {
        (d.kind != "const" && d.kind != "val") || {
            let di = indent(d.line as usize - 1);
            !fns.iter().any(|(a, b, fi)| *a < d.line && d.line <= *b && *fi < di)
        }
    });
    // Enum variants: inside an `enum` body, a line (or comma-separated run)
    // that starts with a capitalised identifier. `Kind::Note` in a note then
    // resolves to the variant's own line.
    let mut variants = Vec::new();
    for d in out.iter().filter(|d| d.kind == "type" && d.anchor.split_whitespace().any(|w| w == "enum")) {
        let mut count = 0;
        for (i, raw) in lines.iter().enumerate().take(d.end as usize).skip(d.line as usize) {
            let l = raw.trim();
            if l.is_empty() || l.starts_with("//") || l.starts_with('#') || l.starts_with('*') || l.starts_with("/*") {
                continue;
            }
            for piece in l.split(',') {
                let name = piece.trim().split(['(', '{', '=', ' ', ';', ':', '<']).next().unwrap_or("").trim();
                let starts_upper = name.chars().next().is_some_and(|c| c.is_ascii_uppercase());
                if !starts_upper || !name.chars().all(|c| c.is_alphanumeric() || c == '_') || NAME_STOPLIST.contains(&name) {
                    continue;
                }
                let end = if piece.contains('{') { span_end(&lines, i, py) as u32 + 1 } else { i as u32 + 1 };
                variants.push(Def { path: path.to_string(), name: name.to_string(), kind: "variant", line: i as u32 + 1, end, anchor: l.chars().take(160).collect() });
                count += 1;
                if count >= MAX_VARIANTS_PER_ENUM {
                    break;
                }
            }
            if count >= MAX_VARIANTS_PER_ENUM {
                break;
            }
        }
    }
    out.extend(variants);
    out
}

/// Last line (0-based) of the definition starting at `start`. Brace
/// languages: the line where the brace opened on or after the start line
/// closes; a declaration that ends in `;` before any brace ends there.
/// Python: the last line indented deeper than the `def`/`class` line.
pub fn span_end(lines: &[&str], start: usize, py: bool) -> usize {
    let last = lines.len().saturating_sub(1);
    if start >= lines.len() {
        return start;
    }
    let limit = (start + MAX_SPAN_LINES).min(last);
    if py {
        let indent = |l: &str| l.len() - l.trim_start().len();
        let base = indent(lines[start]);
        let mut end = start;
        for (i, l) in lines.iter().enumerate().take(limit + 1).skip(start + 1) {
            if l.trim().is_empty() {
                continue;
            }
            if indent(l) <= base {
                break;
            }
            end = i;
        }
        return end;
    }
    let mut depth: i32 = 0;
    let mut opened = false;
    for (i, l) in lines.iter().enumerate().take(limit + 1).skip(start) {
        let code = strip_strings_and_comments(l);
        for c in code.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if opened && depth <= 0 {
            return i;
        }
        if !opened && code.trim_end().ends_with(';') {
            return i;
        }
        // Kotlin and Scala expression bodies: `fun f() = expr` with no brace.
        if !opened && i == start && code.contains('=') && !code.trim_end().ends_with('=') && !code.trim_end().ends_with(',') && !code.trim_end().ends_with('(') {
            return i;
        }
    }
    limit.min(last)
}

/// A line with string literals and trailing comments blanked, so braces
/// inside them do not count. Approximate: one line at a time.
pub fn strip_strings_and_comments(l: &str) -> String {
    let mut out = String::with_capacity(l.len());
    let mut chars = l.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    quote = None;
                }
                out.push(' ');
            }
            None => {
                if c == '/' && chars.peek() == Some(&'/') {
                    break;
                }
                if c == '#' && !out.contains(|x: char| !x.is_whitespace()) && !l.trim_start().starts_with("#[") && !l.trim_start().starts_with("#include") {
                    break;
                }
                // A single quote is a char literal only when it closes within
                // a few characters ('a', '\n', '\u{7f}' is the longest common
                // case at 8); otherwise it is a Rust lifetime or a label
                // (`Page<'_>`, `'outer: loop`), which must not blank the line.
                let char_literal = c == '\'' && {
                    let rest: String = chars.clone().take(9).collect();
                    rest.find('\'').is_some_and(|i| i > 0 && (i <= 2 || rest.starts_with('\\')))
                };
                if c == '"' || char_literal || c == '`' {
                    quote = Some(c);
                    out.push(' ');
                } else {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// The definition's current span in the checkout: the stored one when the
/// file is unchanged, else re-found by its anchor line (then by name), else
/// None. Reads one file.
/// SHA-256 of a definition's current body (its span re-found if the file
/// moved), or None when the definition is gone. The unit of "has the code
/// this note describes changed".
pub fn body_hash(conn: &Connection, project_id: i64, root: &Path, d: &Def) -> Result<Option<Vec<u8>>> {
    let Some((a, b)) = current_span(conn, project_id, root, d)? else { return Ok(None) };
    let Ok(text) = std::fs::read_to_string(root.join(&d.path)) else { return Ok(None) };
    let body: Vec<&str> = text.lines().skip(a as usize - 1).take((b - a + 1) as usize).map(str::trim_end).collect();
    Ok(Some(Sha256::digest(body.join("\n").as_bytes()).to_vec()))
}

/// The innermost definition in `path` whose span holds `line`.
pub fn def_enclosing(conn: &Connection, project_id: i64, path: &str, line: u32) -> Result<Option<Def>> {
    let mut stmt = conn.prepare_cached(
        "SELECT path, name, kind, line, end_line, anchor FROM code_defs WHERE project_id = ?1 AND path = ?2 AND line <= ?3 AND end_line >= ?3 ORDER BY line DESC LIMIT 1",
    )?;
    let d = stmt
        .query_row(params![project_id, path, line as i64], |r| {
            let kind: String = r.get(2)?;
            Ok(Def { path: r.get(0)?, name: r.get(1)?, kind: kind_str(&kind), line: r.get::<_, i64>(3)? as u32, end: r.get::<_, i64>(4)? as u32, anchor: r.get(5)? })
        })
        .optional()?;
    Ok(d)
}

pub fn current_span(conn: &Connection, project_id: i64, root: &Path, d: &Def) -> Result<Option<(u32, u32)>> {
    let full = root.join(&d.path);
    let Ok(bytes) = std::fs::read(&full) else { return Ok(None) };
    let stored: Option<Vec<u8>> = conn
        .query_row("SELECT hash FROM code_files WHERE project_id = ?1 AND path = ?2", params![project_id, d.path], |r| r.get(0))
        .optional()?;
    let unchanged = stored.as_deref() == Some(Sha256::digest(&bytes).as_slice()) && d.end >= d.line;
    let Ok(text) = String::from_utf8(bytes) else { return Ok(None) };
    let lines: Vec<&str> = text.lines().collect();
    if unchanged {
        let start = if d.kind == "use" { d.line as usize - 1 } else { doc_start(&lines, d.line as usize - 1) };
        return Ok(Some((start as u32 + 1, d.end)));
    }
    let py = ext_of(&d.path) == "py";
    let anchor = d.anchor.trim();
    let found = (!anchor.is_empty()).then(|| lines.iter().position(|l| l.trim() == anchor)).flatten().or_else(|| {
        definitions(&d.path, &text).into_iter().find(|x| x.name == d.name).map(|x| x.line as usize - 1)
    });
    Ok(found.map(|s| (doc_start(&lines, s) as u32 + 1, span_end(&lines, s, py) as u32 + 1)))
}

/// Files this file imports, resolved against the checkout; unresolved imports
/// (std, third party) are dropped.
pub fn imports(path: &str, text: &str, by_name: &HashMap<&str, Vec<&String>>) -> Vec<String> {
    let dir = path.rfind('/').map(|i| &path[..i]).unwrap_or("");
    let ext = ext_of(path);
    let mut out: Vec<String> = Vec::new();
    let mut push = |p: String| {
        if p != path && !out.contains(&p) {
            out.push(p);
        }
    };
    let resolve_name = |stem: &str, exts: &[&str]| -> Option<String> {
        for e in exts {
            let name = format!("{stem}.{e}");
            if let Some(v) = by_name.get(name.as_str()) {
                if v.len() == 1 {
                    return Some(v[0].clone());
                }
                // Prefer the candidate sharing the longest directory prefix with us.
                let best = v.iter().max_by_key(|c| common_prefix(c, path)).unwrap();
                return Some((*best).clone());
            }
        }
        None
    };
    for raw in text.lines().take(400) {
        let l = raw.trim();
        match ext {
            "rs" => {
                if let Some(rest) = l.strip_prefix("mod ").or_else(|| l.strip_prefix("pub mod ")) {
                    let m = rest.trim_end_matches(';').trim();
                    if m.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        for cand in [format!("{dir}/{m}.rs"), format!("{dir}/{m}/mod.rs")] {
                            let cand = cand.trim_start_matches('/').to_string();
                            if by_name.get(format!("{m}.rs").as_str()).map(|v| v.iter().any(|p| **p == cand)).unwrap_or(false) || by_name.get("mod.rs").map(|v| v.iter().any(|p| **p == cand)).unwrap_or(false) {
                                push(cand);
                                break;
                            }
                        }
                    }
                } else if let Some(rest) = l.strip_prefix("use crate::").or_else(|| l.strip_prefix("use super::")).or_else(|| l.strip_prefix("use self::")) {
                    let seg = rest.split("::").next().unwrap_or("").trim_matches(|c| c == '{' || c == '}' || c == ';' || c == ' ');
                    if !seg.is_empty() && seg.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        if let Some(p) = resolve_name(seg, &["rs"]) {
                            push(p);
                        }
                    }
                }
            }
            "kt" | "kts" | "java" => {
                if let Some(rest) = l.strip_prefix("import ") {
                    let last = rest.trim_end_matches(';').rsplit('.').next().unwrap_or("");
                    if !last.is_empty() && last != "*" && last.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                        if let Some(p) = resolve_name(last, &["kt", "java"]) {
                            push(p);
                        }
                    }
                }
            }
            "py" => {
                let target = l.strip_prefix("from ").map(|r| r.split_whitespace().next().unwrap_or("")).or_else(|| l.strip_prefix("import ").map(|r| r.split([' ', ',']).next().unwrap_or("")));
                if let Some(t) = target {
                    let last = t.trim_start_matches('.').rsplit('.').next().unwrap_or("");
                    if !last.is_empty() {
                        if let Some(p) = resolve_name(last, &["py"]) {
                            push(p);
                        }
                    }
                }
            }
            "ts" | "tsx" | "js" | "jsx" | "mjs" => {
                if let Some(i) = l.find(" from ").or_else(|| l.find("require(")) {
                    let q = &l[i..];
                    let spec = q.split(['\'', '"']).nth(1).unwrap_or("");
                    if spec.starts_with('.') {
                        let stem = spec.rsplit('/').next().unwrap_or("").trim_end_matches(".js");
                        if let Some(p) = resolve_name(stem, &["ts", "tsx", "js", "jsx", "mjs"]) {
                            push(p);
                        }
                    }
                }
            }
            "c" | "h" | "cc" | "cpp" | "hpp" => {
                if let Some(rest) = l.strip_prefix("#include \"") {
                    let f = rest.split('"').next().unwrap_or("");
                    let name = f.rsplit('/').next().unwrap_or("");
                    if let Some(v) = by_name.get(name) {
                        push(v[0].clone());
                    }
                }
            }
            "go" => {
                // Package imports resolve to directories, not files; skipped.
            }
            _ => {}
        }
    }
    out
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

/// Identifier tokens of a source text (letters, digits, underscore runs that start with a letter or underscore).
pub fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|t| !t.is_empty() && t.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false))
}

/// Where `name` is defined, if the code index knows. Several hits when the
/// name is defined in more than one file.
/// The name a definition keyword introduces: receiver and generics stripped
/// (`PointerInputScope.layerGestures(` and `List<StatusLayer>.needsFrost:`
/// are `layerGestures` and `needsFrost`), trailing punctuation dropped.
fn def_name(tok: &str) -> String {
    let mut depth = 0usize;
    let mut no_generics = String::with_capacity(tok.len());
    for c in tok.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => no_generics.push(c),
            _ => {}
        }
    }
    let head = no_generics.split(['(', ':', '{', '=', ';', ',']).next().unwrap_or("");
    let last = head.rsplit('.').next().unwrap_or(head);
    last.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_')).to_string()
}

/// First line (0-based) of the doc comment and annotations that sit directly
/// above `start`: `///`, `//!`, `/** … */`, `#[…]`, `@Composable`, KDoc. A
/// span that starts here carries the contract, not just the signature.
pub fn doc_start(lines: &[&str], start: usize) -> usize {
    let mut s = start;
    while s > 0 {
        let l = lines[s - 1].trim();
        let doc = l.starts_with("///") || l.starts_with("//!") || l.starts_with("/**") || l.starts_with("* ") || l == "*" || l.starts_with("*/") || l.starts_with("#[") || l.starts_with('@') || l.starts_with("/// ") || l.starts_with("\"\"\"");
        if !doc {
            break;
        }
        s -= 1;
    }
    s
}

/// Re-indexes changed files when the project has a code index. Content
/// hashes make an unchanged checkout a walk and nothing more.
pub fn refresh(conn: &mut Connection, project_id: i64, root: &Path) -> Result<()> {
    if has_index(conn, project_id)? {
        index(conn, project_id, root)?;
    }
    Ok(())
}

/// Indexed files whose path equals `name` or ends with `/name`.
pub fn paths_ending_with(conn: &Connection, project_id: i64, name: &str) -> Result<Vec<String>> {
    let t = name.trim_start_matches("./");
    let mut stmt = conn.prepare_cached("SELECT path FROM code_files WHERE project_id = ?1 AND (path = ?2 OR path LIKE ?3) ORDER BY path LIMIT 8")?;
    let rows = stmt.query_map(params![project_id, t, format!("%/{t}")], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Interns a stored kind into the static names `Def` carries.
pub fn kind_str(kind: &str) -> &'static str {
    match kind {
        "fn" => "fn",
        "type" => "type",
        "variant" => "variant",
        "use" => "use",
        "val" => "val",
        _ => "const",
    }
}

pub fn defs_named(conn: &Connection, project_id: i64, name: &str) -> Result<Vec<Def>> {
    let short = name.rsplit("::").next().unwrap_or(name).rsplit('.').next().unwrap_or(name).trim_end_matches("()");
    let mut stmt = conn.prepare_cached("SELECT path, name, kind, line, end_line, anchor FROM code_defs WHERE project_id = ?1 AND name = ?2 ORDER BY path")?;
    let rows = stmt.query_map(params![project_id, short], |r| {
        let kind: String = r.get(2)?;
        Ok(Def {
            path: r.get(0)?,
            name: r.get(1)?,
            kind: kind_str(&kind),
            line: r.get::<_, i64>(3)? as u32,
            end: r.get::<_, i64>(4)? as u32,
            anchor: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// True when the project has a code index.
pub fn has_index(conn: &Connection, project_id: i64) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM code_files WHERE project_id = ?1)", [project_id], |r| r.get(0))?)
}

#[cfg(test)]
mod span_tests {
    use super::*;
    #[test]
    fn lifetimes_do_not_open_strings() {
        let src = "fn page_key(page: &Page<'_>, compressed: bool) -> u128 {\n    let c = 'x';\n    let n = '\\n';\n    'outer: loop {\n        break 'outer;\n    }\n}\nfn next() {}\n";
        let lines: Vec<&str> = src.lines().collect();
        assert_eq!(span_end(&lines, 0, false), 6);
        assert_eq!(strip_strings_and_comments("let c = 'x'; {"), "let c =    ; {");
    }
}

#[cfg(test)]
mod ignore_tests {
    use super::*;
    #[test]
    fn gitignore_patterns_skip_vendored_trees() {
        let d = std::env::temp_dir().join(format!("mc-ignore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("vendored/src")).unwrap();
        std::fs::create_dir_all(d.join("src/gen")).unwrap();
        std::fs::write(d.join(".gitignore"), "vendored\n/src/gen/\n*.min.js\n").unwrap();
        std::fs::write(d.join("vendored/src/a.c"), "int f(){}").unwrap();
        std::fs::write(d.join("src/gen/b.rs"), "fn g(){}").unwrap();
        std::fs::write(d.join("src/c.rs"), "fn h(){}").unwrap();
        let files = walk(&d);
        assert_eq!(files, vec!["src/c.rs".to_string()], "{files:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_definitions_across_languages() {
        let rs = "use a;\n/* block\n fn not_me() */\npub(crate) async fn embed_missing(x: i32) {}\npub struct Scope { }\nconst MAX: usize = 1;\n// fn commented() {}\nimpl Foo { fn method(&self) {} }\n";
        let d = definitions("a.rs", rs);
        let names: Vec<&str> = d.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["embed_missing", "Scope", "MAX", "method"], "{names:?}");
        assert_eq!(d[0].line, 4);
        let kt = "data class CropRect(val x: Int)\noverride fun onDraw() {}\nprivate val cache = 1\nobject Foo {\n";
        let names: Vec<String> = definitions("a.kt", kt).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["CropRect", "onDraw", "cache", "Foo"]);
        let py = "class Foo:\n    def bar(self):\n        pass\ndef baz(): ...\n";
        let names: Vec<String> = definitions("a.py", py).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["Foo", "bar", "baz"]);
    }

    #[test]
    fn resolves_imports_against_the_checkout() {
        let files: Vec<String> = ["src/main.rs", "src/store.rs", "src/embed/mod.rs", "app/ui/ChatScreen.kt", "app/data/Repo.kt", "lib/util.py"].iter().map(|s| s.to_string()).collect();
        let mut by_name: HashMap<&str, Vec<&String>> = HashMap::new();
        for f in &files {
            by_name.entry(f.rsplit('/').next().unwrap()).or_default().push(f);
        }
        assert_eq!(imports("src/main.rs", "mod store;\nmod embed;\nuse crate::store::save;\nuse std::io;\n", &by_name), ["src/store.rs", "src/embed/mod.rs"]);
        assert_eq!(imports("app/ui/ChatScreen.kt", "import com.x.data.Repo\nimport android.os.Bundle\n", &by_name), ["app/data/Repo.kt"]);
        assert_eq!(imports("lib/main.py", "from lib.util import x\nimport os\n", &by_name), ["lib/util.py"]);
    }

    #[test]
    fn spans_end_where_the_brace_closes_or_the_indent_returns() {
        let rs = "pub fn a(x: i32) -> i32 {\n    if x > 0 {\n        1\n    } else {\n        2\n    }\n}\n\npub struct S;\nfn b() { let s = \"{\"; }\n";
        let d = definitions("a.rs", rs);
        let spans: Vec<(u32, u32)> = d.iter().map(|d| (d.line, d.end)).collect();
        assert_eq!(spans, [(1, 7), (9, 9), (10, 10)], "{spans:?}");
        assert_eq!(d[0].anchor, "pub fn a(x: i32) -> i32 {");
        let py = "class A:\n    def m(self):\n        return 1\n\n    x = 2\n\ndef top():\n    pass\nz = 1\n";
        let d = definitions("a.py", py);
        let spans: Vec<(&str, u32, u32)> = d.iter().map(|d| (d.name.as_str(), d.line, d.end)).collect();
        assert_eq!(spans, [("A", 1, 5), ("m", 2, 3), ("top", 7, 8)], "{spans:?}");
        let kt = "override fun onDraw() = draw(1)\nfun big(\n    a: Int\n) {\n    x()\n}\n";
        let d = definitions("a.kt", kt);
        let spans: Vec<(u32, u32)> = d.iter().map(|d| (d.line, d.end)).collect();
        assert_eq!(spans, [(1, 1), (2, 6)], "{spans:?}");
    }
}
