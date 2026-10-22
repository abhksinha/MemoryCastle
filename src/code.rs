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
pub const CODE_VERSION: &str = "13";
/// Callees stored per definition. Printing is bounded by `CONTEXT_ITEMS`;
/// storage must not be, or a dispatcher's later callees lose their caller
/// edge (`run` in `main.rs` passed 64 and `narrow_span` showed no caller).
pub const MAX_CALLS_PER_DEF: usize = 512;
/// Shortest name that can be a call edge: the `name(` shape is evidence
/// enough for a three-letter name (`run(`), where a bare mention is not.
pub const MIN_CALL_LEN: usize = 3;
/// Method names every language's standard library has: `x.len()`,
/// `s.find(`. A `.name(` call to one of these is never an edge to a
/// project function of the same name.
pub const METHOD_STOPLIST: &[&str] = &[
    "is_empty", "len", "push", "pop", "get", "insert", "remove", "contains", "iter", "map", "filter", "collect", "unwrap", "clone", "default", "new", "from", "into", "as_ref", "as_str",
    "to_string", "join", "split", "trim", "lines", "chars", "bytes", "find", "starts_with", "ends_with", "first", "last", "next", "take", "skip", "min", "max", "sum", "count", "sort",
    "dedup", "extend", "write", "read", "parse", "format", "print", "ok", "err", "and", "or", "then", "eq", "cmp", "hash", "drop", "send", "recv", "lock", "borrow", "append", "add",
    "set", "put", "has", "keys", "values", "items", "apply", "call", "bind", "open", "close", "flush", "exists", "size", "clear", "reset", "update", "delete", "save", "load", "run",
];
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
    pub calls: usize,
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
    // Pass 3: calls. A `name(` inside a definition's span is a call from
    // that definition to the name, when the name is a function somewhere.
    {
        let fn_names: HashMap<String, usize> = {
            let mut stmt = tx.prepare("SELECT name, count(DISTINCT path) FROM code_defs WHERE project_id = ?1 AND kind = 'fn' GROUP BY name HAVING count(DISTINCT path) <= ?2")?;
            let rows = stmt.query_map(params![project_id, PROMISCUOUS_DEFS as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO code_calls(project_id, src, caller, callee, line, qual) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
        for (path, text) in &changed {
            for (caller, callee, line, qual) in calls_in(path, text, &fn_names) {
                ins.execute(params![project_id, path, caller, callee, line, qual])?;
                st.calls += 1;
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

/// What the code index knows about a file, for `links <file>` when no note
/// names it: its definitions, the files that import it and the files that
/// use its names. Lines in the `links` grammar; empty when the file is not
/// indexed. Bounded by `limits::CONTEXT_ITEMS` names per line.
pub fn file_facts(conn: &Connection, project_id: i64, path: &str) -> Result<Vec<String>> {
    let paths = paths_ending_with(conn, project_id, path)?;
    let Some(p) = paths.first() else { return Ok(Vec::new()) };
    let cap = crate::limits::CONTEXT_ITEMS;
    let mut out = Vec::new();
    let mut stmt = conn.prepare_cached("SELECT name, kind, line, end_line FROM code_defs WHERE project_id = ?1 AND path = ?2 ORDER BY line")?;
    let defs: Vec<(String, String, i64, i64)> = stmt
        .query_map(params![project_id, p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if !defs.is_empty() {
        let shown: Vec<String> = defs.iter().take(cap).map(|(n, k, a, b)| if b > a { format!("{k} {n}:{a}-{b}") } else { format!("{k} {n}:{a}") }).collect();
        let more = if defs.len() > cap { format!(" +{} more", defs.len() - cap) } else { String::new() };
        out.push(format!("defines: {} in {p}  {}{more}", defs.len(), shown.join(", ")));
    }
    let mut imp = conn.prepare_cached("SELECT src FROM code_imports WHERE project_id = ?1 AND dst = ?2 ORDER BY src")?;
    let importers: Vec<String> = imp.query_map(params![project_id, p], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    if !importers.is_empty() {
        let more = if importers.len() > cap { format!(" +{} more", importers.len() - cap) } else { String::new() };
        out.push(format!("imported by: {} files  {}{more}", importers.len(), importers.iter().take(cap).cloned().collect::<Vec<_>>().join(", ")));
    }
    let mut users = conn.prepare_cached(
        "SELECT m.src, SUM(m.n) AS n FROM code_mentions m JOIN code_defs d ON d.project_id = m.project_id AND d.name = m.name \
         WHERE d.project_id = ?1 AND d.path = ?2 AND m.src != ?2 GROUP BY m.src ORDER BY n DESC",
    )?;
    let using: Vec<String> = users.query_map(params![project_id, p], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    if !using.is_empty() {
        let more = if using.len() > cap { format!(" +{} more", using.len() - cap) } else { String::new() };
        out.push(format!("used from: {} files  {}{more}", using.len(), using.iter().take(cap).cloned().collect::<Vec<_>>().join(", ")));
    }
    Ok(out)
}

/// The files that use a name, most mentions first, as one `links` line.
pub fn users_line(conn: &Connection, project_id: i64, name: &str, defining: &str) -> Result<Option<String>> {
    let short = name.rsplit("::").next().unwrap_or(name).rsplit('.').next().unwrap_or(name).trim_end_matches("()");
    let cap = crate::limits::CONTEXT_ITEMS;
    let mut stmt = conn.prepare_cached("SELECT src FROM code_mentions WHERE project_id = ?1 AND name = ?2 AND src != ?3 ORDER BY n DESC")?;
    let using: Vec<String> = stmt.query_map(params![project_id, short, defining], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    if using.is_empty() {
        return Ok(None);
    }
    let more = if using.len() > cap { format!(" +{} more", using.len() - cap) } else { String::new() };
    Ok(Some(format!("used from: {} files  {}{more}  (--context for the lines)", using.len(), using.iter().take(cap).cloned().collect::<Vec<_>>().join(", "))))
}

/// The innermost block of a wide definition that holds `line`: a match arm,
/// an `if`, a loop, a closure (brace depth), or an indented suite in
/// Python. Narrows level by level while the span is wider than
/// `limits::SPAN_WIDE`; returns the header text and the span, or None when
/// the definition is not wide or no inner block holds the line.
pub fn narrow_span(root: &Path, d: &Def, line: u32) -> Option<(String, u32, u32)> {
    let wide = crate::limits::SPAN_WIDE;
    if d.end.saturating_sub(d.line) < wide || line < d.line || line > d.end {
        return None;
    }
    let text = std::fs::read_to_string(root.join(&d.path)).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let py = ext_of(&d.path) == "py";
    let target = (line as usize).saturating_sub(1);
    let (mut a, mut b) = ((d.line as usize).saturating_sub(1), (d.end as usize).saturating_sub(1).min(lines.len().saturating_sub(1)));
    let mut found: Option<(String, u32, u32)> = None;
    for _ in 0..8 {
        if b.saturating_sub(a) < wide as usize {
            break;
        }
        let mut best: Option<(usize, usize)> = None;
        if py {
            let indent = |l: &str| l.len() - l.trim_start().len();
            let base = indent(lines[a]);
            // the shallowest indent inside the block
            let inner = lines[a + 1..=b].iter().filter(|l| !l.trim().is_empty()).map(|l| indent(l)).filter(|i| *i > base).min();
            if let Some(inner) = inner {
                for i in (a + 1..=target).rev() {
                    let l = lines[i];
                    if !l.trim().is_empty() && indent(l) == inner && l.trim_end().ends_with(':') {
                        let e = span_end(&lines, i, true);
                        if e >= target {
                            best = Some((i, e));
                        }
                        break;
                    }
                }
            }
        } else {
            // direct children of the block: lines met at depth 1 that open a block
            let mut depth: i32 = 0;
            for i in a..=target {
                let code = strip_strings_and_comments(lines[i]);
                let before = depth;
                let opens = code.matches('{').count() as i32;
                let closes = code.matches('}').count() as i32;
                depth += opens - closes;
                if i > a && before == 1 && opens > closes {
                    let e = span_end(&lines, i, false);
                    if e >= target {
                        best = Some((i, e));
                    }
                }
            }
        }
        match best {
            Some((i, e)) if (i, e) != (a, b) && e.saturating_sub(i) < b.saturating_sub(a) => {
                let mut header = lines[i].trim().to_string();
                if header.len() > 60 {
                    header.truncate(57);
                    header.push_str("...");
                }
                found = Some((header, i as u32 + 1, e as u32 + 1));
                a = i;
                b = e;
            }
            _ => break,
        }
    }
    found
}

/// Definitions wider than `limits::SPAN_WIDE`, widest first: the ones a
/// note cannot point at readably.
pub fn wide_defs(conn: &Connection, project_id: i64, limit: usize) -> Result<Vec<(String, String, u32)>> {
    let mut stmt = conn.prepare_cached("SELECT path, name, end_line - line + 1 AS w FROM code_defs WHERE project_id = ?1 AND kind = 'fn' AND w > ?2 ORDER BY w DESC LIMIT ?3")?;
    let rows = stmt.query_map(params![project_id, crate::limits::SPAN_WIDE as i64, limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u32)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod narrow_tests {
    use super::*;
    #[test]
    fn narrows_a_wide_function_to_the_arm_holding_the_line() {
        let dir = std::env::temp_dir().join(format!("mc-narrow-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut src = String::from("fn run() {\n    match cmd {\n");
        for arm in 0..4 {
            src.push_str(&format!("        Cmd::A{arm} {{ x }} => {{\n"));
            for i in 0..60 {
                src.push_str(&format!("            let v{i} = {arm};\n"));
            }
            src.push_str("        }\n");
        }
        src.push_str("    }\n}\n");
        std::fs::write(dir.join("w.rs"), &src).unwrap();
        let lines: Vec<&str> = src.lines().collect();
        let d = Def { path: "w.rs".into(), name: "run".into(), kind: "fn", line: 1, end: lines.len() as u32, anchor: "fn run() {".into() };
        // line inside the third arm: 2 header lines + 2 arms of 62 + header + 10
        let line = 2 + 62 * 2 + 1 + 10;
        let (h, a, b) = narrow_span(&dir, &d, line as u32).expect("wide function narrows");
        assert!(h.starts_with("Cmd::A2"), "{h}");
        assert_eq!(a, 2 + 62 * 2 + 1);
        assert_eq!(b, a + 61);
        assert!(narrow_span(&dir, &Def { end: 100, ..d.clone() }, 50).is_none(), "a narrow definition is left alone");
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Call edges in one file: (caller, callee, first line). A callee is a name
/// the index defines (`names`), at least `MIN_CALL_LEN` long, in call shape
/// on a line inside a definition's span; the innermost definition holding
/// the line is the caller. Self-calls are skipped; `MAX_CALLS_PER_DEF`
/// callees per caller, first seen first. The fourth field is how the call
/// is written (`qualifier`), which `call_confidence` turns into a score.
pub fn calls_in(path: &str, text: &str, names: &HashMap<String, usize>) -> Vec<(String, String, u32, String)> {
    let defs = definitions(path, text);
    if defs.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<(String, String, u32, String)> = Vec::new();
    let mut per: HashMap<String, usize> = HashMap::new();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    for (i, raw) in text.lines().enumerate() {
        let ln = i as u32 + 1;
        if !raw.contains('(') {
            continue;
        }
        let Some(caller) = defs.iter().filter(|d| d.line <= ln && ln <= d.end && d.kind == "fn").max_by_key(|d| d.line) else { continue };
        if ln == caller.line {
            continue;
        }
        let code = strip_strings_and_comments(raw);
        for tok in identifiers(&code) {
            if tok.len() < MIN_CALL_LEN || !names.contains_key(tok) {
                continue;
            }
            let Some(q) = qualifier(&code, tok) else { continue };
            // A call to its own name is recursion unless it names another
            // module or type (`eval::run` inside `main.rs`'s `run`).
            if tok == caller.name && (q.is_empty() || q == "self" || q == ".") {
                continue;
            }
            // `x.len()` is the standard library, not the project's `len`.
            if METHOD_STOPLIST.contains(&tok) && code.find(&format!(".{tok}(")).is_some() && !code.contains(&format!(" {tok}(")) && !code.contains(&format!("::{tok}(")) {
                continue;
            }
            let key = (caller.name.clone(), tok.to_string(), q);
            if seen.contains(&key) {
                continue;
            }
            let n = per.entry(caller.name.clone()).or_default();
            if *n >= MAX_CALLS_PER_DEF {
                continue;
            }
            *n += 1;
            seen.insert(key.clone());
            out.push((key.0, key.1, ln, key.2));
        }
    }
    out
}

/// How the first call of `name` in `code` is written: `self` (`self.f(`,
/// `this.f(`, `Self::f(`, `super::`, `crate::`), `X::` or `X.` (a module or
/// type, or a capitalised receiver), `.` (a method on a value whose type the
/// line does not say), or empty (a bare call).
pub fn qualifier(code: &str, name: &str) -> Option<String> {
    let bytes = code.as_bytes();
    let mut from = 0;
    while let Some(i) = code[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        from = end;
        if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            continue;
        }
        // `name::<T>(` is a call; `name::x` is a module path.
        let rest = &code[end..];
        if !(rest.starts_with('(') || rest.starts_with("!(") || rest.starts_with("::<")) {
            continue;
        }
        let before = code[..start].trim_end();
        let (sep, head) = if let Some(h) = before.strip_suffix("::") {
            ("::", h)
        } else if let Some(h) = before.strip_suffix('.') {
            (".", h)
        } else {
            return Some(String::new());
        };
        let head = head.trim_end();
        let word: String = head.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<Vec<_>>().into_iter().rev().collect();
        if matches!(word.as_str(), "self" | "this" | "Self" | "super" | "crate") {
            return Some("self".to_string());
        }
        if sep == "::" && !word.is_empty() {
            return Some(format!("{word}::"));
        }
        if word.chars().next().is_some_and(char::is_uppercase) {
            return Some(format!("{word}."));
        }
        return Some(".".to_string());
    }
    None
}

/// What an edge's way of being written says about whether it reaches `d`,
/// as a measured share (see `limits::CALL_CONF_*`): a `self` call or a name
/// with one definition is near certain; a qualifier naming `d`'s file or a
/// type defined there points at `d`, one naming anything else points away;
/// a method call on an unknown value to a name defined in several files is
/// mostly another function.
/// `same_file`: the call is in `d`'s own file, where a bare name resolves
/// to the local definition first.
pub fn call_confidence(qual: &str, unique: bool, same_file: bool, own: &HashSet<String>) -> f32 {
    use crate::limits::*;
    match qual {
        "self" => CALL_CONF_SELF,
        "" if unique => CALL_CONF_BARE_UNIQUE,
        "" if same_file => CALL_CONF_SAME_FILE,
        "" => CALL_CONF_OTHER,
        "." if unique => CALL_CONF_METHOD_UNIQUE,
        "." => CALL_CONF_OTHER,
        q => {
            let (word, path) = q.strip_suffix("::").map(|w| (w, true)).unwrap_or_else(|| (q.trim_end_matches('.'), false));
            if own.contains(word) {
                if path { CALL_CONF_QUALIFIED } else { CALL_CONF_TYPE }
            } else {
                CALL_CONF_OTHER
            }
        }
    }
}

/// The names that point at `d`'s file: its stem (with and without `_`) and
/// every non-function definition in it (types, traits, objects).
pub fn own_names(conn: &Connection, project_id: i64, path: &str) -> Result<HashSet<String>> {
    let stem = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut own: HashSet<String> = [stem.to_string(), stem.replace('_', "")].into_iter().collect();
    let mut stmt = conn.prepare_cached("SELECT name FROM code_defs WHERE project_id = ?1 AND path = ?2 AND kind != 'fn'")?;
    for n in stmt.query_map(params![project_id, path], |r| r.get::<_, String>(0))? {
        own.insert(n?);
    }
    Ok(own)
}

fn defined_once(conn: &Connection, project_id: i64, name: &str) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT count(DISTINCT path) FROM code_defs WHERE project_id = ?1 AND name = ?2 AND kind = 'fn'", params![project_id, name], |r| r.get(0))?;
    Ok(n <= 1)
}

/// Who calls the definition `d`: (caller, file, line, confidence), most
/// confident first, then by file and line.
pub fn callers_for(conn: &Connection, project_id: i64, d: &Def) -> Result<Vec<(String, String, u32, f32)>> {
    let unique = defined_once(conn, project_id, &d.name)?;
    let own = own_names(conn, project_id, &d.path)?;
    let mut stmt = conn.prepare_cached("SELECT caller, src, line, qual FROM code_calls WHERE project_id = ?1 AND callee = ?2 ORDER BY src, line")?;
    let rows: Vec<(String, String, u32, String)> = stmt
        .query_map(params![project_id, d.name], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u32, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // One line per calling function: its most confident way of calling.
    let mut out: Vec<(String, String, u32, f32)> = Vec::with_capacity(rows.len());
    for (c, f, l, q) in rows {
        let conf = call_confidence(&q, unique, f == d.path, &own);
        match out.iter_mut().find(|o| o.0 == c && o.1 == f) {
            Some(o) if o.3 < conf => *o = (c, f, l, conf),
            Some(_) => {}
            None => out.push((c, f, l, conf)),
        }
    }
    out.sort_by(|a, b| b.3.total_cmp(&a.3));
    Ok(out)
}

/// What `d` calls: (callee, confidence) in order of first call, the
/// confidence being the best over the callee's definitions.
pub fn callees_for(conn: &Connection, project_id: i64, d: &Def) -> Result<Vec<(String, f32)>> {
    let mut stmt = conn.prepare_cached("SELECT callee, qual FROM code_calls WHERE project_id = ?1 AND caller = ?2 AND src = ?3 ORDER BY line")?;
    let rows: Vec<(String, String)> = stmt.query_map(params![project_id, d.name, d.path], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<(String, f32)> = Vec::with_capacity(rows.len());
    for (callee, qual) in rows {
        let unique = defined_once(conn, project_id, &callee)?;
        let mut best = 0f32;
        for t in defs_named(conn, project_id, &callee)? {
            best = best.max(call_confidence(&qual, unique, t.path == d.path, &own_names(conn, project_id, &t.path)?));
        }
        match out.iter_mut().find(|o| o.0 == callee) {
            Some(o) => o.1 = o.1.max(best),
            None => out.push((callee, best)),
        }
    }
    Ok(out)
}

/// Who calls `name`: (caller, file, line), by file then line.
pub fn callers_of(conn: &Connection, project_id: i64, name: &str) -> Result<Vec<(String, String, u32)>> {
    let short = name.rsplit("::").next().unwrap_or(name).rsplit('.').next().unwrap_or(name).trim_end_matches("()");
    let mut stmt = conn.prepare_cached("SELECT caller, src, line FROM code_calls WHERE project_id = ?1 AND callee = ?2 ORDER BY src, line")?;
    let rows = stmt.query_map(params![project_id, short], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u32)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What `name` in `path` calls, in order of first call.
pub fn callees_of(conn: &Connection, project_id: i64, name: &str, path: &str) -> Result<Vec<String>> {
    let short = name.rsplit("::").next().unwrap_or(name).rsplit('.').next().unwrap_or(name).trim_end_matches("()");
    let mut stmt = conn.prepare_cached("SELECT callee FROM code_calls WHERE project_id = ?1 AND caller = ?2 AND src = ?3 ORDER BY line")?;
    let rows = stmt.query_map(params![project_id, short, path], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// What `d` calls and what calls it, without utilities called from more
/// than `NEIGHBOUR_MAX_CALLERS` places: the names a note about the code
/// around `d` would use.
pub fn neighbours(conn: &Connection, project_id: i64, d: &Def) -> Result<Vec<String>> {
    let min = crate::limits::CALL_MIN_CONF;
    let mut names: Vec<String> = callees_for(conn, project_id, d)?.into_iter().filter(|c| c.1 >= min).map(|c| c.0).collect();
    for (caller, _, _, _) in callers_for(conn, project_id, d)?.into_iter().filter(|c| c.3 >= min) {
        if !names.contains(&caller) {
            names.push(caller);
        }
    }
    let mut out = Vec::with_capacity(names.len());
    for n in names {
        if callers_of(conn, project_id, &n)?.len() <= crate::limits::NEIGHBOUR_MAX_CALLERS {
            out.push(n);
        }
    }
    Ok(out)
}

/// The two call lines `links <symbol>` prints under `defined:`, bounded by
/// `limits::CONTEXT_ITEMS` names each: who calls it, with file and line,
/// and what it calls. Empty for a name with no call edges (a constant, a
/// type), where `users_line` applies instead.
pub fn call_lines(conn: &Connection, project_id: i64, d: &Def, with_callers: bool) -> Result<Vec<String>> {
    let cap = crate::limits::CONTEXT_ITEMS;
    let mut out = Vec::new();
    let min = crate::limits::CALL_MIN_CONF;
    let all = if with_callers { callers_for(conn, project_id, d)? } else { Vec::new() };
    let unlikely = all.iter().filter(|c| c.3 < min).count();
    let callers: Vec<_> = all.into_iter().filter(|c| c.3 >= min).collect();
    let tail = if unlikely > 0 { format!(" (+{unlikely} by name only, under {min:.1}: another `{}` or a call on a value of unknown type)", d.name) } else { String::new() };
    if !callers.is_empty() {
        let shown: Vec<String> = callers.iter().take(cap).map(|(c, f, l, p)| format!("{c} {f}:{l} {p:.1}")).collect();
        let more = if callers.len() > cap { format!(" +{} more", callers.len() - cap) } else { String::new() };
        out.push(format!("called by: {}  {}{more}{tail}", callers.len(), shown.join(", ")));
    } else if unlikely > 0 {
        out.push(format!("called by: 0{tail}"));
    }
    let callees: Vec<String> = callees_for(conn, project_id, d)?.into_iter().filter(|c| c.1 >= min).map(|c| c.0).collect();
    if !callees.is_empty() {
        let more = if callees.len() > cap { format!(" +{} more", callees.len() - cap) } else { String::new() };
        out.push(format!("calls: {}  {}{more}", callees.len(), callees.iter().take(cap).cloned().collect::<Vec<_>>().join(", ")));
    }
    Ok(out)
}

#[cfg(test)]
mod call_edge_tests {
    use super::*;
    #[test]
    fn calls_are_name_in_call_shape_inside_a_function() {
        let src = "fn outer() {\n    let x = helper(1);\n    // helper(2) in a comment\n    let s = \"helper(3)\";\n    other::thing();\n    outer();\n}\nfn helper(n: i32) -> i32 { n }\nfn thing() {}\nconst K: i32 = helper(0);\n";
        let names: HashMap<String, usize> = ["outer", "helper", "thing", "find"].iter().map(|s| (s.to_string(), 1)).collect();
        let strip = |v: Vec<(String, String, u32, String)>| v.into_iter().map(|(a, b, l, _)| (a, b, l)).collect::<Vec<_>>();
        let c = strip(calls_in("a.rs", &format!("{src}fn f2() {{\n    let i = s.find('x');\n    find(1);\n}}\nfn find(n: i32) {{}}\n"), &names));
        assert!(c.contains(&("f2".to_string(), "find".to_string(), 13)), "a free call to the project's find counts: {c:?}");
        assert!(!c.contains(&("f2".to_string(), "find".to_string(), 12)), "the method call line is not the edge: {c:?}");
        let c2 = calls_in("b.rs", "fn g() {\n    let i = s.find('x');\n}\nfn find(n: i32) {}\n", &names);
        assert!(c2.is_empty(), "a method call to a standard name is not an edge: {c2:?}");
        let full = calls_in("a.rs", src, &names);
        assert_eq!(full[1].3, "other::", "the qualifier is kept: {full:?}");
        let m = calls_in("m.rs", "fn outer() {\n    crate::helper::thing(1);\n}\n", &names);
        assert!(m.iter().all(|c| c.1 != "helper"), "a module path is not a call: {m:?}");
        let c = strip(full);
        assert_eq!(c, vec![("outer".to_string(), "helper".to_string(), 2), ("outer".to_string(), "thing".to_string(), 5)], "{c:?}");
    }

    #[test]
    fn qualifier_says_how_a_call_is_written() {
        assert_eq!(qualifier("let x = helper(1);", "helper").as_deref(), Some(""));
        assert_eq!(qualifier("self.helper(1)", "helper").as_deref(), Some("self"));
        assert_eq!(qualifier("Self::helper(1)", "helper").as_deref(), Some("self"));
        assert_eq!(qualifier("store::helper(1)", "helper").as_deref(), Some("store::"));
        assert_eq!(qualifier("Store.helper(1)", "helper").as_deref(), Some("Store."));
        assert_eq!(qualifier("x.y.helper(1)", "helper").as_deref(), Some("."));
        assert_eq!(qualifier("my_helper(1); helper(2)", "helper").as_deref(), Some(""));
        assert_eq!(qualifier("search::stem(x)", "search"), None, "a module path");
        assert_eq!(qualifier("parse::<u32>(x)", "parse").as_deref(), Some(""));
        let own: HashSet<String> = ["store".to_string(), "Store".to_string()].into_iter().collect();
        assert!(call_confidence("store::", false, false, &own) > call_confidence("other::", false, false, &own));
        assert!(call_confidence("", false, true, &own) > call_confidence("", false, false, &own));
        assert!(call_confidence(".", false, false, &own) < crate::limits::CALL_MIN_CONF);
    }
}
