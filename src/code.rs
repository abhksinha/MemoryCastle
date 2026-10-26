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
use std::path::{Path, PathBuf};

/// Source extensions indexed. Everything else is a file node only if a note names it.
pub const CODE_EXTS: &[&str] = &["rs", "kt", "kts", "java", "py", "ts", "tsx", "js", "jsx", "mjs", "go", "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "inl", "ipp", "tpp", "cu", "cuh", "swift", "rb", "php", "cs", "sh", "bash"];
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
pub const CODE_VERSION: &str = "26";
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
        let head = header(text);
        tx.execute(
            "INSERT INTO code_files(project_id, path, hash, lang, size, mtime, head, head_end) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![project_id, path, hash, ext_of(path), size, mtime, head.0, head.1],
        )?;
        let mut ins = tx.prepare_cached("INSERT INTO code_defs(project_id, path, name, kind, line, end_line, anchor, name_words, words, owner) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)")?;
        let lines: Vec<&str> = text.lines().collect();
        let defs = definitions(path, text);
        for (d, owner) in defs.iter().zip(owners(path, &lines, &defs)) {
            let (mut name_words, words) = find_words(&lines, d);
            // The owner's words weigh like the name's: "the codec's encoder".
            for w in ident_words(&owner) {
                name_words.push(' ');
                name_words.push_str(&w);
            }
            ins.execute(params![project_id, path, d.name, d.kind, d.line, d.end, d.anchor, name_words, words, owner])?;
            st.defs += 1;
        }
        let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO code_fields(project_id, src, name, type) VALUES (?1, ?2, ?3, ?4)")?;
        for (name, ty) in fields(text) {
            ins.execute(params![project_id, path, name, ty])?;
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
    // Pass 4: JNI. Kotlin or Java `external fun nativeX` is implemented by
    // a native `Java_<package>_<Class>_nativeX`; no call is written between
    // them, so the edge comes from the name, rebuilt whole when anything
    // changed (both sides can move).
    if !changed.is_empty() {
        tx.execute("DELETE FROM code_calls WHERE project_id = ?1 AND qual = 'jni'", params![project_id])?;
        let natives: Vec<String> = {
            let mut stmt = tx.prepare("SELECT DISTINCT name FROM code_defs WHERE project_id = ?1 AND kind = 'fn' AND name LIKE 'Java\\_%' ESCAPE '\\'")?;
            let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut find = tx.prepare_cached("SELECT path, line FROM code_defs WHERE project_id = ?1 AND name = ?2 AND kind = 'fn' AND (path LIKE ?3 OR path LIKE ?4)")?;
        let mut ins = tx.prepare_cached("INSERT OR IGNORE INTO code_calls(project_id, src, caller, callee, line, qual) VALUES (?1, ?2, ?3, ?4, ?5, 'jni')")?;
        for native in natives {
            let Some((class_path, method)) = jni_target(&native) else { continue };
            let rows: Vec<(String, i64)> = find
                .query_map(params![project_id, method, format!("%{class_path}.kt"), format!("%{class_path}.java")], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (path, line) in rows {
                st.calls += ins.execute(params![project_id, path, method, native, line])?;
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

/// `Java_com_app_bridge_RustBridge_nativeX` as (`com/app/bridge/RustBridge`,
/// `nativeX`): JNI's mangling, `_1` an underscore in a name, `__` the start
/// of an overload's signature. Unicode escapes (`_0xxxx`) give up.
pub fn jni_target(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("Java_")?;
    let rest = rest.split("__").next()?;
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut it = rest.chars().peekable();
    while let Some(c) = it.next() {
        if c != '_' {
            cur.push(c);
            continue;
        }
        match it.peek() {
            Some('1') => {
                it.next();
                cur.push('_');
            }
            Some('0') | Some('2') | Some('3') => return None,
            _ => parts.push(std::mem::take(&mut cur)),
        }
    }
    parts.push(cur);
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let method = parts.pop()?;
    Some((parts.join("/"), method))
}

/// A file's header comment: the first comment block, after a shebang and
/// blank lines, in any of `#`, `//`, `//!`, `--`, `%`, `;` or a `/* */` or
/// `"""` block, up to `limits::CODE_FIND_DOC_BYTES`, with the line it
/// ends on. A script says what it does there (`# Every blocker of the
/// native job per document`), and no definition holds it.
pub fn header(text: &str) -> (String, u32) {
    let mut out = String::new();
    let mut end = 0u32;
    let mut block: Option<&str> = None;
    for (i, raw) in text.lines().enumerate().take(60) {
        let l = raw.trim();
        if i == 0 && l.starts_with("#!") {
            continue;
        }
        let body = if let Some(close) = block {
            match l.find(close) {
                Some(k) => {
                    block = None;
                    &l[..k]
                }
                None => l,
            }
        } else if l.is_empty() {
            if out.is_empty() {
                continue;
            }
            break;
        } else if let Some(r) = l.strip_prefix("/*").or_else(|| l.strip_prefix("\"\"\"")).or_else(|| l.strip_prefix("'''")) {
            let close = if l.starts_with("/*") { "*/" } else if l.starts_with("'''") { "'''" } else { "\"\"\"" };
            match r.find(close) {
                Some(k) => &r[..k],
                None => {
                    block = Some(close);
                    r
                }
            }
        } else if let Some(r) = ["//!", "///", "//", "#", "--", "%", ";;", ";"].iter().find_map(|p| l.strip_prefix(p)) {
            if r.starts_with('!') || r.contains("-*-") {
                continue;
            }
            r
        } else {
            break;
        };
        let body = body.trim_start_matches(['*', '%', '#', '/', ' ']).trim();
        if !body.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(body);
        }
        end = i as u32 + 1;
        if out.len() >= crate::limits::CODE_FIND_DOC_BYTES {
            break;
        }
    }
    let mut cut = out.len().min(crate::limits::CODE_FIND_DOC_BYTES);
    while !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    (out, end)
}

/// Files whose header comment holds at least `min` of the question's
/// stemmed words, generic ones left out, and half of them, most words first: `(path,
/// header end line, words matched)`.
pub fn headers_matching(conn: &Connection, project_id: i64, words: &[String], min: usize) -> Result<Vec<(String, u32, Vec<String>)>> {
    // Quantifiers and verbs ("every", "check") say nothing about a file.
    let words: Vec<&String> = words.iter().filter(|w| !crate::search::is_generic(w)).collect();
    let need = min.max(words.len().div_ceil(2));
    if words.len() < min {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached("SELECT path, head, head_end FROM code_files WHERE project_id = ?1 AND head != ''")?;
    let mut out: Vec<(String, u32, Vec<String>)> = stmt
        .query_map([project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u32>(2)?)))?
        .filter_map(|r| r.ok())
        .filter_map(|(p, h, e)| {
            let hw: Vec<String> = h.split(|c: char| !c.is_alphanumeric() && c != '_').flat_map(ident_words).collect();
            let got: Vec<String> = words.iter().filter(|q| hw.iter().any(|w| w.starts_with(q.as_str()))).map(|q| q.to_string()).collect();
            (got.len() >= need).then_some((p, e, got))
        })
        .collect();
    out.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then(a.0.len().cmp(&b.0.len())));
    Ok(out)
}

pub(crate) fn ext_of(path: &str) -> &str {
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

/// Paths git tracks, read from the index file (versions 2 to 4), and every
/// directory holding one. `.gitignore` never applies to a tracked file, and
/// Courier tracks 98 files under an ignored `web/`. No `git` process.
fn tracked(root: &Path) -> Option<(HashSet<String>, HashSet<String>)> {
    let dot = root.join(".git");
    let dir = if dot.is_file() {
        let t = std::fs::read_to_string(&dot).ok()?;
        let g = t.trim().strip_prefix("gitdir:")?.trim().to_string();
        let g = PathBuf::from(g);
        if g.is_absolute() { g } else { root.join(g) }
    } else {
        dot
    };
    let b = std::fs::read(dir.join("index")).ok()?;
    if b.len() < 12 || &b[..4] != b"DIRC" {
        return None;
    }
    let be32 = |i: usize| b.get(i..i + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]));
    let (version, count) = (be32(4)?, be32(8)? as usize);
    let (mut files, mut dirs) = (HashSet::new(), HashSet::new());
    let mut prev: Vec<u8> = Vec::new();
    let mut i = 12;
    for _ in 0..count {
        let start = i;
        let flags = u16::from_be_bytes([*b.get(i + 60)?, *b.get(i + 61)?]);
        i += 62;
        if version >= 3 && flags & 0x4000 != 0 {
            i += 2;
        }
        let path = if version >= 4 {
            // Prefix compression: drop N bytes of the previous path, then a suffix.
            let mut c = *b.get(i)?;
            i += 1;
            let mut strip = (c & 127) as usize;
            while c & 128 != 0 {
                c = *b.get(i)?;
                i += 1;
                strip = ((strip + 1) << 7) | (c & 127) as usize;
            }
            let end = i + b.get(i..)?.iter().position(|&x| x == 0)?;
            let mut p = prev[..prev.len().saturating_sub(strip)].to_vec();
            p.extend_from_slice(&b[i..end]);
            i = end + 1;
            p
        } else {
            let end = i + b.get(i..)?.iter().position(|&x| x == 0)?;
            let p = b[i..end].to_vec();
            // Entries are NUL-padded to a multiple of 8 bytes.
            i = start + (end - start + 8) / 8 * 8;
            p
        };
        let s = String::from_utf8_lossy(&path).into_owned();
        let mut d = s.as_str();
        while let Some(k) = d.rfind('/') {
            d = &d[..k];
            if !dirs.insert(d.to_string()) {
                break;
            }
        }
        files.insert(s);
        prev = path;
    }
    Some((files, dirs))
}

/// Source files under `root`, relative, bounded, skipping VCS and build
/// output and whatever `.gitignore` files (root and nested) say to skip,
/// unless git tracks the file.
pub fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    // Read only when an ignore rule first matches.
    let mut index: Option<Option<(HashSet<String>, HashSet<String>)>> = None;
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
                let t = index.get_or_insert_with(|| tracked(root));
                let keep = t.as_ref().is_some_and(|(files, dirs)| if ft.is_dir() { dirs.contains(&rel) } else { files.contains(&rel) });
                if !keep {
                    continue;
                }
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
    ("const", "const"), ("static", "const"), ("val", "val"), ("macro_rules!", "fn"), ("module", "type"),
];
const MODIFIERS: &[&str] = &["pub", "pub(crate)", "pub(super)", "async", "unsafe", "extern", "external", "tailrec", "operator", "infix", "actual", "expect", "inner", "annotation", "value", "mutating", "fileprivate", "convenience", "required", "export", "default", "private", "public", "protected", "internal", "override", "open", "abstract", "final", "suspend", "inline", "static", "data", "sealed", "const", "lateinit", "companion", "declare", "readonly"];

const JS_MODS: &[&str] = &["public", "private", "protected", "static", "async", "readonly", "override", "abstract", "get", "set", "declare", "*"];
const JS_NOT_METHODS: &[&str] = &["if", "for", "while", "switch", "catch", "function", "return", "with", "constructor", "super", "await", "typeof", "new", "else"];

/// `async load(p: string): Promise<void> {`: modifiers, a name, a parameter
/// list closed on the same line, an optional return type, then `{` ending
/// the line. A call with a callback (`describe("x", () => {`) never closes
/// its parenthesis before the brace, so it is not one.
fn js_method(l: &str) -> Option<&str> {
    let code = l.split(" //").next().unwrap_or(l).trim_end();
    // `private async load(` with its parameters on the next lines: a call
    // never starts with a modifier.
    let open = code.ends_with('(');
    if !code.contains('{') && !open {
        return None;
    }
    let mut rest = code;
    let mut modified = false;
    loop {
        let w = rest.split_whitespace().next()?;
        if JS_MODS.contains(&w) && rest.len() > w.len() {
            rest = rest[w.len()..].trim_start();
            modified = true;
        } else {
            break;
        }
    }
    let rest = rest.trim_start_matches('*').trim_start_matches('#');
    let name_len = rest.bytes().take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$').count();
    let (name, after) = rest.split_at(name_len);
    if name.is_empty() || name.as_bytes()[0].is_ascii_digit() || JS_NOT_METHODS.contains(&name) {
        return None;
    }
    // Optional generics, then the parameter list, closed on this line.
    let mut after = after.trim_start();
    if after.starts_with('<') {
        let close = after.find(">(")?;
        after = &after[close + 1..];
    }
    if !after.starts_with('(') {
        return None;
    }
    let mut depth = 0i32;
    let mut end = None;
    for (i, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return (open && modified && after.trim_end() == "(").then_some(name);
    };
    let tail = after[end + 1..].trim();
    // The body right after `)`, or after a return type `: T {`.
    // A return type ends at the body's `{` closing the line; an interface's
    // `name(): Promise<{ a: T }>;` has none.
    (tail.starts_with('{') || (tail.starts_with(':') && tail.ends_with('{') && !tail.contains("=>"))).then_some(name)
}

/// Python declares with `def` and `class` only, and `module` is Ruby's, so
/// either word elsewhere is prose (a docstring's "function from this module").
fn keyword_applies(ext: &str, kw: &str) -> bool {
    match ext {
        "py" => matches!(kw, "def" | "class"),
        "rb" => matches!(kw, "def" | "class" | "module"),
        // A script is indexed for its header and its `function`s; `fn` or
        // `class` in a command line is not a definition.
        "sh" | "bash" => kw == "function",
        _ => kw != "module",
    }
}

/// The name after a Go method's receiver: `func (s *Store) Size()` is `Size`.
fn go_method(l: &str) -> Option<String> {
    let after = &l[l.find("func")? + 4..];
    let after = after.trim_start().strip_prefix('(')?;
    let close = after.find(')')?;
    let name: String = after[close + 1..].trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// Definitions in one file: keyword-before-name on each line. Top-level or
/// not makes no difference; a method is a definition too.
pub fn definitions(path: &str, text: &str) -> Vec<Def> {
    if crate::cfamily::handles(ext_of(path)) {
        return crate::cfamily::definitions(path, text);
    }
    let mut out = Vec::new();
    let mut in_block_comment = false;
    let js = matches!(ext_of(path), "ts" | "tsx" | "js" | "jsx" | "mjs");
    let ext = ext_of(path);
    let go = ext == "go";
    // Inside Go's `type (` group every line opens with a type's name.
    let mut go_types = false;
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
        if go {
            if l.starts_with("type (") {
                go_types = true;
                continue;
            }
            if go_types {
                if raw.starts_with(')') {
                    go_types = false;
                } else if raw.starts_with('\t') && !raw.starts_with("\t\t") && !raw.starts_with("\t}") {
                    let name = def_name(l.split_whitespace().next().unwrap_or(""));
                    if name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') {
                        out.push(Def { path: path.to_string(), name, kind: "type", line: n as u32 + 1, end: 0, anchor: l.chars().take(160).collect() });
                    }
                }
                continue;
            }
        }
        // A class or object method in JavaScript and TypeScript has no
        // keyword: `async load(p: string) {`.
        if js {
            if let Some(name) = js_method(l) {
                out.push(Def { path: path.to_string(), name: name.to_string(), kind: "fn", line: n as u32 + 1, end: 0, anchor: l.chars().take(160).collect() });
                continue;
            }
        }
        // Keyword after modifiers only, or right after an opening brace on
        // the same line (`impl Foo { fn method`). Elsewhere a keyword is prose.
        // `pub(in crate::engine) fn` is `pub fn` for finding the keyword.
        let l = match l.strip_prefix("pub(in ").and_then(|r| r.split_once(')')) {
            Some((_, rest)) => rest.trim_start(),
            None => l,
        };
        let words: Vec<&str> = l.split_whitespace().collect();
        let mut kw = None;
        let mut name = "";
        let mut ok = true;
        for (i, w) in words.iter().enumerate().take(8) {
            let bare = w.trim_end_matches([':', '(', '{']);
            // `const` and `static` are keywords when a name follows, modifiers when another keyword does.
            let next_bare = words.get(i + 1).map(|n| n.trim_end_matches([':', '(', '{'])).unwrap_or("");
            let next_is_kw = DEF_KEYWORDS.iter().any(|(k, _)| *k == next_bare) || MODIFIERS.contains(&next_bare);
            // `extern "system" fn`: the ABI string is part of the modifier.
            if w.starts_with('"') && i > 0 && words[i - 1] == "extern" {
                continue;
            }
            if w.starts_with('@') || w.starts_with("#[") || (MODIFIERS.contains(&bare) && (next_is_kw || !DEF_KEYWORDS.iter().any(|(k, _)| *k == bare))) {
                continue;
            }
            // `type: 'message',` is an object key, not the keyword.
            if w.ends_with(':') && !w.ends_with("::") && DEF_KEYWORDS.iter().any(|(k, _)| *k == bare) {
                break;
            }
            if let Some((_, kind)) = DEF_KEYWORDS.iter().find(|(k, _)| *k == bare) {
                // `function` in a Python docstring, `module` outside Ruby: prose.
                if ok && !keyword_applies(ext, bare) {
                    break;
                }
                if ok {
                    kw = Some(*kind);
                    name = words.get(i + 1).copied().unwrap_or("");
                    // `enum class Quality`, `fun interface Listener`: the
                    // second keyword is part of the first.
                    if let Some((_, second)) = DEF_KEYWORDS.iter().find(|(k, _)| *k == name) {
                        kw = Some(*second);
                        name = words.get(i + 2).copied().unwrap_or("");
                    }
                    // Kotlin and Swift type parameters before the name:
                    // `fun <T> exclusive(`, `fun <K, V> Map<K, V>.merge(`.
                    if name.starts_with('<') {
                        let mut depth = 0i32;
                        let mut k = i + 1;
                        while let Some(w) = words.get(k) {
                            depth += w.matches('<').count() as i32 - w.matches('>').count() as i32;
                            k += 1;
                            if depth <= 0 {
                                break;
                            }
                        }
                        name = words.get(k).copied().unwrap_or("");
                    }
                }
                break;
            }
            // Something else before the keyword: allowed only if it opened a brace.
            ok = w.ends_with('{') || w.ends_with(';');
        }
        let Some(kind) = kw else { continue };
        // `import { type Foo, bar }`: a type named in a list, not defined.
        if js && kind == "type" && (name.ends_with(',') || name.ends_with('}')) {
            continue;
        }
        // Go's method receiver sits between `func` and the name:
        // `func (s *Store) Size() int {`.
        let receiver;
        let name = if kind == "fn" && name.starts_with('(') {
            receiver = go_method(l);
            receiver.as_deref().unwrap_or("")
        } else {
            name
        };
        let name = def_name(name);
        let name = name.as_str();
        if name.is_empty() || !name.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false) || NAME_STOPLIST.contains(&name) {
            continue;
        }
        // `val x =` in Kotlin and `const X =` are definitions; `let` is not (locals).
        out.push(Def { path: path.to_string(), name: name.to_string(), kind, line: n as u32 + 1, end: 0, anchor: l.chars().take(160).collect() });
    }
    let lines: Vec<&str> = text.lines().collect();
    // Python and Ruby close a definition by indentation (Ruby's `end` sits
    // at the definition's own indent), not by a brace.
    let py = matches!(ext_of(path), "py" | "rb");
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
    // Parentheses and brackets before the body opens: a parameter list
    // over several lines (`fun f(\n  a: Int,\n): Boolean`).
    let mut par: i32 = 0;
    // After `=` outside the parameters: an expression body or initializer.
    let mut expr = false;
    // Inside a string that runs over lines (`"\` in Rust, `"""` in Kotlin).
    let mut in_str = false;
    // A Rust `where` clause runs until the body opens.
    let mut in_where = false;
    // The next line of code, past blank lines and comments (a `/**` doc
    // block's `*` lines are not a multiplication).
    let next_code = |i: usize| {
        lines.iter().skip(i + 1).take(limit.saturating_sub(i)).map(|l| strip_strings_and_comments(l)).find(|c| {
            let c = c.trim_start();
            !c.is_empty() && !c.starts_with("/*") && !c.starts_with('*')
        })
    };
    for (i, l) in lines.iter().enumerate().take(limit + 1).skip(start) {
        // Whether this line leaves a string open: quotes that are not
        // escaped, not the character `'"'`, and not after a `//` comment.
        let toggles = {
            let cs: Vec<char> = l.chars().collect();
            let mut inside = in_str;
            let mut flips = false;
            let mut k = 0;
            while k < cs.len() {
                let c = cs[k];
                if !inside && c == '/' && cs.get(k + 1) == Some(&'/') {
                    break;
                }
                let escaped = cs[..k].iter().rev().take_while(|&&b| b == '\\').count() % 2 == 1;
                let char_lit = k > 0 && cs[k - 1] == '\'' && cs.get(k + 1) == Some(&'\'');
                if c == '"' && !escaped && !char_lit {
                    inside = !inside;
                    flips = !flips;
                }
                k += 1;
            }
            flips
        };
        // The line a string closes on is read from its closing quote on.
        let mut l = *l;
        if toggles && !opened {
            in_str = !in_str;
            if in_str {
                // The line that opens the string still counts up to it.
                expr |= l.split('"').next().is_some_and(|h| h.contains('='));
                continue;
            }
            l = l.split_once('"').map_or("", |(_, rest)| rest);
        } else if in_str && !opened {
            continue;
        }
        let code = strip_strings_and_comments(l);
        let mut prev = ' ';
        let cs: Vec<char> = code.chars().collect();
        for (k, &c) in cs.iter().enumerate() {
            match c {
                // Braces inside brackets before the body are a value
                // (`[Glyph { w: 1 }, ..]`), not the body.
                '{' if !opened && par > 0 => par += 1,
                '}' if !opened && par > 0 => par -= 1,
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                '(' | '[' if !opened => par += 1,
                ')' | ']' if !opened => par -= 1,
                '=' if !opened && par <= 0 && !matches!(prev, '=' | '!' | '<' | '>') && !matches!(cs.get(k + 1), Some('=' | '>')) => expr = true,
                _ => {}
            }
            prev = c;
        }
        if opened && depth <= 0 {
            return i;
        }
        let t = code.trim_end();
        if opened || par > 0 {
            continue;
        }
        if t.ends_with(';') {
            return i;
        }
        if t.trim_start().starts_with("where") {
            in_where = true;
        }
        if in_where {
            continue;
        }
        // `val revision: Long,` in a constructor's list ends at its comma.
        if t.ends_with(',') {
            return i;
        }
        // What the next line opens with decides whether this one goes on:
        // a body (`{`), a return type, a `where`, a chained call, an operator.
        let next = next_code(i).map(|c| c.trim_start().to_string()).unwrap_or_default();
        let carries_on = |n: &str| ["{", "=", ":", "->", "where", "throws", ".", "?.", "?:", "|", "&&", "||", "+", "-", "*", "/"].iter().any(|p| n.starts_with(p));
        // Strings are blanked in `code`, so `val K = "a"` would look open.
        let ends_in_literal = matches!(l.trim_end().chars().last(), Some('"' | '\'' | '`'));
        let open_end = !ends_in_literal && ["=", ",", ".", "(", "[", "+", "-", "*", "/", "&&", "||", "?:", "->", "=>", ":"].iter().any(|p| t.ends_with(p));
        if expr {
            // `fun f() =\n    other(x)`: the expression ends on the first
            // line that neither ends open nor is continued by the next.
            if !open_end && !carries_on(&next) {
                return i;
            }
            continue;
        }
        // The signature closed with no body and no `=`: a declaration
        // (`external fun f(x: Int): Long`, `abstract fun g()`,
        // `data class P(val a: Int)`), unless the next line opens one.
        if !open_end && !carries_on(&next) && !t.is_empty() {
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

/// The definitions on either side of a line no definition holds (imports,
/// a file header, the gap between two functions): the last one ending
/// before it and the first one starting after it.
pub fn defs_beside(conn: &Connection, project_id: i64, path: &str, line: u32) -> Result<(Option<Def>, Option<Def>)> {
    let row = |r: &rusqlite::Row| {
        let kind: String = r.get(2)?;
        Ok(Def { path: r.get(0)?, name: r.get(1)?, kind: kind_str(&kind), line: r.get::<_, i64>(3)? as u32, end: r.get::<_, i64>(4)? as u32, anchor: r.get(5)? })
    };
    let before = conn
        .prepare_cached("SELECT path, name, kind, line, end_line, anchor FROM code_defs WHERE project_id = ?1 AND path = ?2 AND end_line < ?3 ORDER BY end_line DESC, line ASC LIMIT 1")?
        .query_row(params![project_id, path, line as i64], row)
        .optional()?;
    let after = conn
        .prepare_cached("SELECT path, name, kind, line, end_line, anchor FROM code_defs WHERE project_id = ?1 AND path = ?2 AND line > ?3 ORDER BY line ASC LIMIT 1")?
        .query_row(params![project_id, path, line as i64], row)
        .optional()?;
    Ok((before, after))
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
    let py = matches!(ext_of(&d.path), "py" | "rb");
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
            e if crate::cfamily::is_c_family(e) => {
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
    // `[` opens Go's type parameters: `func New[T any](`.
    let head = no_generics.split(['(', ':', '{', '=', ';', ',', '[']).next().unwrap_or("");
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
    let defs: Vec<Def> = rows.collect::<rusqlite::Result<_>>()?;
    // `crates/server/src/mobile_auth.rs::refresh`, `mobile_auth.rs::refresh`,
    // `crates/server::refresh`: a path qualifier keeps the definitions in
    // that file or under that directory (four crates define `refresh`).
    if let Some((q, _)) = name.rsplit_once("::") {
        let q = q.trim_start_matches("./").trim_end_matches('/');
        let in_path = |d: &Def| d.path == q || d.path.ends_with(&format!("/{q}")) || d.path.starts_with(&format!("{q}/"));
        if (q.contains('/') || q.contains('.')) && defs.iter().any(in_path) {
            return Ok(defs.into_iter().filter(|d| in_path(d)).collect());
        }
    }
    // `Context::search`, `spans::context`, `self.nodes.store`: the qualifier
    // names a file stem or a type defined in the file. Keep those when any
    // match; else every definition, as before.
    let qual = name[..name.len() - short.len()].trim_end_matches("()").trim_end_matches(['.', ':']);
    let qual = qual.rsplit("::").next().unwrap_or(qual).rsplit('.').next().unwrap_or(qual);
    if defs.len() < 2 || qual.is_empty() || qual == "self" || qual == "Self" || qual == "crate" {
        return Ok(defs);
    }
    let mut owns = conn.prepare_cached("SELECT 1 FROM code_defs WHERE project_id = ?1 AND path = ?2 AND name = ?3 AND kind IN ('struct', 'enum', 'trait', 'type', 'class', 'impl') LIMIT 1")?;
    // The method's own type first: `BapCodec::encode` is the one in
    // `impl BapCodec`, not every `encode` in a file defining `BapCodec`.
    let mut keep = Vec::new();
    for d in &defs {
        if kind_label(conn, project_id, d)?.ends_with(&format!(" in {qual}")) {
            keep.push(d.clone());
        }
    }
    if !keep.is_empty() {
        return Ok(keep);
    }
    for d in &defs {
        let stem = std::path::Path::new(&d.path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.eq_ignore_ascii_case(qual) || owns.exists(params![project_id, d.path, qual])? {
            keep.push(d.clone());
        }
    }
    Ok(if keep.is_empty() { defs } else { keep })
}

/// `fn in BapCodec` for a method, `fn` for a free definition: what `links`
/// prints after a definition's span, so the agent can name it `Owner::name`.
pub fn kind_label(conn: &Connection, project_id: i64, d: &Def) -> Result<String> {
    let owner: Option<String> = conn
        .prepare_cached("SELECT owner FROM code_defs WHERE project_id = ?1 AND path = ?2 AND name = ?3 AND line = ?4")?
        .query_row(params![project_id, d.path, d.name, d.line], |r| r.get(0))
        .optional()?;
    Ok(match owner.filter(|o| !o.is_empty()) {
        Some(o) => format!("{} in {o}", d.kind),
        None => d.kind.to_string(),
    })
}

/// True when the project has a code index.
pub fn has_index(conn: &Connection, project_id: i64) -> Result<bool> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM code_files WHERE project_id = ?1)", [project_id], |r| r.get(0))?)
}

#[cfg(test)]
mod span_tests {
    use super::*;
    #[test]
    fn kotlin_two_word_type_heads() {
        let kt = "enum class Quality { Sd, Hd }\nfun interface Listener { fun on() }\nannotation class Marker\nvalue class Id(val v: Long)\n";
        let names: Vec<String> = definitions("a.kt", kt).into_iter().filter(|d| d.kind == "type").map(|d| d.name).collect();
        assert_eq!(names, ["Quality", "Listener", "Marker", "Id"]);
        let g = "private fun <T> exclusive(block: () -> T): T = synchronized(L) {\n    block()\n}\nfun <K, V> merge(a: K, b: V) {}\n";
        let d: Vec<(String, u32, u32)> = definitions("a.kt", g).into_iter().map(|d| (d.name, d.line, d.end)).collect();
        assert_eq!(d, [("exclusive".to_string(), 1, 3), ("merge".to_string(), 4, 4)]);
    }

    #[test]
    fn jni_names_resolve_to_the_class_and_method() {
        assert_eq!(jni_target("Java_com_app_bridge_RustBridge_nativeEncryptMediaFd"), Some(("com/app/bridge/RustBridge".into(), "nativeEncryptMediaFd".into())));
        assert_eq!(jni_target("Java_a_B_native_1x__I"), Some(("a/B".into(), "native_x".into())));
        assert_eq!(jni_target("Java_B"), None);
        assert_eq!(jni_target("helper"), None);
        let rs = "#[no_mangle]\npub extern \"system\" fn Java_a_B_nativeX(\n    env: JNIEnv,\n) -> jint {\n    0\n}\n";
        let d = definitions("ffi.rs", rs);
        assert_eq!(d.iter().map(|d| (d.name.as_str(), d.line, d.end)).collect::<Vec<_>>(), vec![("Java_a_B_nativeX", 2, 6)]);
    }

    #[test]
    fn spans_end_without_braces() {
        let end = |src: &str, start: usize| span_end(&src.lines().collect::<Vec<_>>(), start, false) + 1;
        let kt = "object B {\n    external fun a(\n        x: Int,\n    ): ByteArray?\n    external fun b(): Long\n    fun c(x: Int): Boolean =\n        a(x) != null\n    private const val K = \"a_b\"\n\n    private const val M = 16\n}\n";
        assert_eq!(end(kt, 1), 4, "external fun with parameters over lines");
        assert_eq!(end(kt, 4), 5, "one-line external fun");
        assert_eq!(end(kt, 5), 7, "expression body on the next line");
        assert_eq!(end(kt, 7), 8, "one-line string constant");
        let data = "data class P(\n    val a: Int,\n    val b: String = \"\",\n)\n";
        assert_eq!(end(data, 0), 4);
        assert_eq!(end(data, 1), 2, "a constructor property ends at its comma");
        let rs = "const H: &str = \"\\\nline \\\"q\nend\";\nfn f<T>(x: T) -> u8\nwhere\n    T: Copy,\n{\n    1\n}\nstatic T: [G; 2] = [\n    G { w: 1 }, // \"\n    G { w: 2 },\n];\nconst D: usize = d!(\"\\\\\");\nconst E: u8 = 1;\n";
        assert_eq!(end(rs, 0), 3, "a string over lines");
        assert_eq!(end(rs, 3), 9, "a where clause");
        assert_eq!(end(rs, 9), 13, "braces and a quote in a comment inside a table");
        assert_eq!(end(rs, 13), 14, "an escaped backslash before a quote");
    }

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
        let names = |p: &str, t: &str| definitions(p, t).into_iter().map(|d| d.name).collect::<Vec<_>>();
        // A docstring's "function from this module" is prose; a class with bases is one.
        let py = "\"\"\"\n    function from this module.\n\"\"\"\nclass IPv4Address(_BaseV4, _BaseAddress):\n    pass\n";
        assert_eq!(names("a.py", py), ["IPv4Address"]);
        let go = "type (\n\tA struct{}\n\tB = int\n)\nfunc New[T any]() *Store { return nil }\nfunc (s *Store) Size() int { return 0 }\n";
        assert_eq!(names("a.go", go), ["A", "B", "New", "Size"]);
        let ts = "import { type Opts, x } from './o';\nexport class Store {\n  async load(p: string): Promise<void> {\n  private async lock(\n    id: string,\n  ) {\n  list(): Promise<{ a: T }>;\n  send({ type: 'message' });\n  describe('x', () => {\n}\n";
        assert_eq!(names("a.ts", ts), ["Store", "load", "lock"]);
        let rb = "module Util\n  class Store\n    def size\n      1\n    end\n  end\nend\n";
        let d = definitions("a.rb", rb);
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Util", "Store", "size"]);
        assert_eq!(d[2].end, 4, "the body, by indentation");
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
pub fn file_facts(conn: &Connection, project_id: i64, path: &str, all: bool) -> Result<Vec<String>> {
    let paths = paths_ending_with(conn, project_id, path)?;
    let Some(p) = paths.first() else { return Ok(Vec::new()) };
    let cap = if all { usize::MAX } else { crate::limits::CONTEXT_ITEMS };
    let mut out = Vec::new();
    let mut stmt = conn.prepare_cached("SELECT name, kind, line, end_line FROM code_defs WHERE project_id = ?1 AND path = ?2 ORDER BY line")?;
    let defs: Vec<(String, String, i64, i64)> = stmt
        .query_map(params![project_id, p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if !defs.is_empty() {
        // Types and functions first: a file's constants open it and filled
        // the line (`val DRAFT_DEBOUNCE_MS` before any composable).
        let rank = |k: &str| match k {
            "fn" => 1,
            "variant" => 3,
            "type" | "struct" | "enum" | "trait" | "class" | "interface" | "impl" => 0,
            _ => 2,
        };
        let mut defs = defs;
        defs.sort_by_key(|d| rank(&d.1));
        let shown: Vec<String> = defs.iter().take(cap).map(|(n, k, a, b)| if b > a { format!("{k} {n}:{a}-{b}") } else { format!("{k} {n}:{a}") }).collect();
        let more = if defs.len() > cap { format!(" +{} more (--all lists them)", defs.len() - cap) } else { String::new() };
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
    let using: Vec<String> = users.query_map(params![project_id, p], |r| r.get(0))?.filter_map(|r| r.ok()).filter(|src: &String| same_family(src, p)).collect();
    if !using.is_empty() {
        let more = if using.len() > cap { format!(" +{} more", using.len() - cap) } else { String::new() };
        out.push(format!("used from: {} files  {}{more}", using.len(), using.iter().take(cap).cloned().collect::<Vec<_>>().join(", ")));
    }
    Ok(out)
}

/// The functions that use a non-function definition (a table, a constant)
/// by name, its own file first and then the files that mention it, as one
/// `links` line: `vspace` is called by the `DOC` table, and this says which
/// function reads `DOC`. `None` when nothing does.
pub fn used_by_line(conn: &Connection, project_id: i64, root: &Path, d: &Def) -> Result<Option<String>> {
    let cap = crate::limits::CONTEXT_ITEMS;
    let mut files = vec![d.path.clone()];
    let mut stmt = conn.prepare_cached("SELECT src FROM code_mentions WHERE project_id = ?1 AND name = ?2 AND src != ?3 ORDER BY n DESC LIMIT 20")?;
    files.extend(stmt.query_map(params![project_id, d.name, d.path], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?);
    let mut users: Vec<String> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for path in &files {
        let Ok(text) = std::fs::read_to_string(root.join(path)) else { continue };
        for (i, line) in text.lines().enumerate() {
            let ln = i as u32 + 1;
            if *path == d.path && d.line <= ln && ln <= d.end || !crate::spans::is_identifier_use(line, &d.name) {
                continue;
            }
            let Some(f) = def_enclosing(conn, project_id, path, ln)? else { continue };
            if f.kind != "fn" || f.name == d.name || !seen.insert((f.name.clone(), path.clone())) {
                continue;
            }
            users.push(format!("{} {path}:{ln}", f.name));
        }
    }
    if users.is_empty() {
        return Ok(None);
    }
    let more = if users.len() > cap { format!(" +{} more", users.len() - cap) } else { String::new() };
    Ok(Some(format!("used by: {}  {}{more}", users.len(), users.iter().take(cap).cloned().collect::<Vec<_>>().join(", "))))
}

/// The files that use a name, most mentions first, as one `links` line.
/// Languages that can name each other's definitions without a bridge:
/// Kotlin and Java, TypeScript and JavaScript, C and C++. A mention from
/// another family is a common word (`refresh`, `clear`), not a use: a
/// Kotlin class was "used from 291 files" of Rust server and Tor code.
/// Calls across a bridge are explicit edges (`jni`).
fn lang_family(path: &str) -> &'static str {
    match ext_of(path) {
        "kt" | "kts" | "java" => "jvm",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => "js",
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" | "inl" | "ipp" | "tpp" | "cu" | "cuh" => "c",
        "rs" => "rs",
        "py" => "py",
        "go" => "go",
        "rb" => "rb",
        "swift" => "swift",
        "cs" => "cs",
        "php" => "php",
        "sh" | "bash" => "sh",
        _ => "",
    }
}

fn same_family(a: &str, b: &str) -> bool {
    let (fa, fb) = (lang_family(a), lang_family(b));
    fa.is_empty() || fb.is_empty() || fa == fb
}

pub fn users_line(conn: &Connection, project_id: i64, name: &str, defining: &str) -> Result<Option<String>> {
    let short = name.rsplit("::").next().unwrap_or(name).rsplit('.').next().unwrap_or(name).trim_end_matches("()");
    let cap = crate::limits::CONTEXT_ITEMS;
    let mut stmt = conn.prepare_cached("SELECT src FROM code_mentions WHERE project_id = ?1 AND name = ?2 AND src != ?3 ORDER BY n DESC")?;
    let using: Vec<String> = stmt.query_map(params![project_id, short, defining], |r| r.get(0))?.filter_map(|r| r.ok()).filter(|src: &String| same_family(src, defining)).collect();
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
    let py = matches!(ext_of(&d.path), "py" | "rb");
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

/// For a definition wider than `limits::SPAN_WIDE`: its inner blocks (match
/// arms, `if`s, loops; indented suites in Python) as `a-b header`, the
/// widest `limits::WIDE_BLOCKS_SHOWN` in line order. A block covering most
/// of the span (the `match` of a dispatcher) is opened to its own children.
/// None when the definition is not wide or has no inner block.
pub fn wide_blocks(root: &Path, path: &str, start: u32, end: u32) -> Option<String> {
    if end.saturating_sub(start) < crate::limits::SPAN_WIDE {
        return None;
    }
    let text = std::fs::read_to_string(root.join(path)).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let py = matches!(ext_of(path), "py" | "rb");
    let (mut a, mut b) = ((start as usize).saturating_sub(1), (end as usize).saturating_sub(1).min(lines.len().saturating_sub(1)));
    let mut kids: Vec<(usize, usize)> = Vec::new();
    for _ in 0..4 {
        kids.clear();
        if py {
            let indent = |l: &str| l.len() - l.trim_start().len();
            let base = indent(lines[a]);
            let inner = lines[a + 1..=b].iter().filter(|l| !l.trim().is_empty()).map(|l| indent(l)).filter(|i| *i > base).min()?;
            for i in a + 1..=b {
                let l = lines[i];
                if !l.trim().is_empty() && indent(l) == inner && l.trim_end().ends_with(':') {
                    kids.push((i, span_end(&lines, i, true)));
                }
            }
        } else {
            let mut depth: i32 = 0;
            for i in a..=b {
                let code = strip_strings_and_comments(lines[i]);
                let (opens, closes) = (code.matches('{').count() as i32, code.matches('}').count() as i32);
                if i > a && depth == 1 && opens > closes {
                    kids.push((i, span_end(&lines, i, false).min(b)));
                }
                depth += opens - closes;
            }
        }
        kids.retain(|(i, e)| e > i);
        match kids.iter().max_by_key(|(i, e)| e - i) {
            Some(&(i, e)) if (e - i) * 5 >= (b - a) * 4 => (a, b) = (i, e),
            _ => break,
        }
    }
    if kids.is_empty() {
        return None;
    }
    let n = kids.len();
    kids.sort_by(|x, y| (y.1 - y.0).cmp(&(x.1 - x.0)).then(x.0.cmp(&y.0)));
    kids.truncate(crate::limits::WIDE_BLOCKS_SHOWN);
    kids.sort();
    let shown: Vec<String> = kids
        .iter()
        .map(|(i, e)| {
            let mut h: String = lines[*i].trim().trim_end_matches('{').trim_end().chars().take(40).collect();
            if lines[*i].trim().chars().count() > 40 {
                h.push_str("...");
            }
            format!("{}-{} {h}", i + 1, e + 1)
        })
        .collect();
    let more = if n > shown.len() { format!(" +{} more", n - shown.len()) } else { String::new() };
    Some(format!("   blocks: {} lines, too wide to read whole; widest: {}{more}; `links {path} --at <line>` narrows", end - start + 1, shown.join(", ")))
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
        // links <symbol>: the dispatcher's match is opened, its arms listed.
        let blocks = wide_blocks(&dir, "w.rs", 1, lines.len() as u32).expect("wide function lists blocks");
        assert!(blocks.contains("3-64 Cmd::A0 { x } =>") && blocks.contains("189-250 Cmd::A3"), "{blocks}");
        assert!(wide_blocks(&dir, "w.rs", 1, 100).is_none(), "a narrow definition is left alone");
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Names declared with a type in one file, as `name: Type` at the start of
/// a line (a struct field, a parameter on its own line, a `let`, `val` or
/// `var` binding, a TypeScript property): (name, Type) for a capitalised
/// type, references and `mut` dropped. What a receiver `toks` in
/// `self.toks.store(` is: `Tokens`.
pub fn fields(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for raw in text.lines() {
        let mut s = raw.trim_start();
        if s.starts_with("pub(") {
            s = s.split_once(')').map(|(_, r)| r.trim_start()).unwrap_or(s);
        }
        for kw in ["pub ", "private ", "protected ", "public ", "internal ", "readonly ", "override ", "let ", "const ", "val ", "var ", "mut "] {
            s = s.strip_prefix(kw).map(str::trim_start).unwrap_or(s);
        }
        let name: String = s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_lowercase() || c == '_') {
            continue;
        }
        let rest = s[name.len()..].trim_start().trim_start_matches('?');
        let Some(rest) = rest.strip_prefix(':') else { continue };
        if rest.starts_with(':') {
            continue;
        }
        let mut ty = rest.trim_start();
        for p in ["&'static ", "&mut ", "&", "mut ", "dyn ", "impl "] {
            ty = ty.strip_prefix(p).map(str::trim_start).unwrap_or(ty);
        }
        let ty: String = ty.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        if ty.chars().next().is_some_and(char::is_uppercase) && !out.iter().any(|(n, t)| *n == name && *t == ty) {
            out.push((name, ty));
        }
    }
    out
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
    let raw_lines: Vec<&str> = text.lines().collect();
    for (i, raw) in raw_lines.iter().enumerate() {
        let ln = i as u32 + 1;
        if !raw.contains('(') && !raw.contains("::") {
            continue;
        }
        // The innermost function holding the line; outside every function,
        // the definition that holds it: a dispatch table (`static DOC = [
        // ("vspace", |m| m.vspace()), ..]`), a field initializer. 363 of
        // rushtex's 541 functions with no caller were called only from such
        // places.
        // Only a top-level `const`, `static` or `val` (not indented): a local
        // `const bobKey = ..` in a test callback the index holds no function
        // for is not a caller anyone looks up.
        let table = |d: &&Def| matches!(d.kind, "const" | "val") && raw_lines.get(d.line as usize - 1).is_some_and(|l| !l.starts_with([' ', '\t']));
        // Last, the innermost type: a call in a constructor parameter's
        // default lambda or a property initializer (`class C(val load: .. =
        // { NativeGateway.f(..) })`) is the class's call; dropped, `links f`
        // called a live function dead.
        let holds = |d: &&Def| d.line <= ln && ln <= d.end;
        let Some(caller) = defs
            .iter()
            .filter(|d| holds(d) && d.kind == "fn")
            .max_by_key(|d| d.line)
            .or_else(|| defs.iter().filter(|d| holds(d) && table(d)).max_by_key(|d| d.line))
            .or_else(|| defs.iter().filter(|d| holds(d) && d.kind == "type" && d.line != ln).max_by_key(|d| d.line))
        else {
            continue;
        };
        if ln == caller.line && caller.kind == "fn" {
            continue;
        }
        // A line that declares a function (`external fun f(`) calls nothing.
        if caller.kind != "fn" && defs.iter().any(|d| d.line == ln && d.kind == "fn") {
            continue;
        }
        let code = strip_strings_and_comments(raw);
        for tok in identifiers(&code) {
            if tok.len() < MIN_CALL_LEN || !names.contains_key(tok) {
                continue;
            }
            // `.map(str::trim)` is the standard library's, as `x.trim()` is.
            let Some(q) = qualifier(&code, tok).or_else(|| reference(&code, tok).filter(|_| !METHOD_STOPLIST.contains(&tok))) else { continue };
            // A call to its own name is recursion unless it names another
            // module or type (`eval::run` inside `main.rs`'s `run`).
            if tok == caller.name && (q.is_empty() || q == "self" || q.starts_with('.')) {
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

/// A function named as a value, not called: `.map(Self::render)`,
/// `handlers.push(engine::vspace)`, Java and Kotlin `Foo::bar`, `::bar`.
/// The path before `::` makes it a function, where a bare name could be a
/// variable; what follows must end the expression. Returns the qualifier as
/// `qualifier` would, prefixed with `&`.
pub fn reference(code: &str, name: &str) -> Option<String> {
    let t = code.trim_start();
    if t.starts_with("use ") || t.starts_with("pub use ") || t.starts_with("import ") || t.starts_with("pub(crate) use ") {
        return None;
    }
    let bytes = code.as_bytes();
    let mut from = 0;
    while let Some(i) = code[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        from = end;
        if end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            continue;
        }
        let Some(head) = code[..start].strip_suffix("::") else { continue };
        let next = code[end..].trim_start().chars().next();
        if !matches!(next, None | Some(')' | ',' | ';' | '}' | ']')) {
            continue;
        }
        let word: String = head.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<Vec<_>>().into_iter().rev().collect();
        let q = match word.as_str() {
            "self" | "this" | "Self" | "super" | "crate" => "self".to_string(),
            "" => String::new(),
            w => format!("{w}::"),
        };
        return Some(format!("&{q}"));
    }
    None
}

/// How the first call of `name` in `code` is written: `self` (`self.f(`,
/// `this.f(`, `Self::f(`, `super::`, `crate::`), `X::` or `X.` (a module or
/// type, or a capitalised receiver), `.x` (a method on the value named `x`,
/// whose type `code_fields` may know), `.` (a method on an expression), or
/// empty (a bare call).
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
        // `self.e.toks.store(`: the receiver's last name, which the field
        // table (`code_fields`) can turn into a type.
        if !word.is_empty() && !word.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Some(format!(".{word}"));
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
/// `local`: the call's file defines the name too, and `d` is elsewhere, so a
/// `self.` or bare call there means the local one. `recv`: the types the
/// receiver name of a `.x` call is declared with (`code_fields`).
pub fn call_confidence(qual: &str, unique: bool, same_file: bool, own: &HashSet<String>, local: bool, recv: &HashSet<String>) -> f32 {
    use crate::limits::*;
    // A function passed by name (`&Self::render`) is judged by its path.
    if let Some(q) = qual.strip_prefix('&') {
        return call_confidence(q, unique, same_file, own, local, recv);
    }
    match qual {
        // A JNI pair is named exactly: package, class and method.
        "jni" => 1.0,
        "self" if same_file || unique => CALL_CONF_SELF,
        "self" if local => CALL_CONF_OTHER,
        // Another file's method on the same type (Rust splits an `impl`
        // across files) or an inherited one: possible, not likely.
        "self" => CALL_CONF_TYPE,
        "" if local => CALL_CONF_OTHER,
        "" if unique => CALL_CONF_BARE_UNIQUE,
        "" if same_file => CALL_CONF_SAME_FILE,
        "" => CALL_CONF_OTHER,
        "." if unique => CALL_CONF_METHOD_UNIQUE,
        "." => CALL_CONF_OTHER,
        q if q.starts_with('.') => {
            let word = &q[1..];
            let lower = word.to_lowercase();
            let by_name = own.iter().any(|o| o.to_lowercase() == lower || o.rsplit('_').next().is_some_and(|tail| tail == lower));
            // A field name has several declared types across the tree (`toks:
            // Table` in one file, `toks: Tokens` in another); when the name
            // spells one the definition does not own, that one wins.
            let spelled = |t: &String| abbreviates(&lower, &t.to_lowercase());
            let by_type = recv.iter().any(|t| own.contains(t)) && !recv.iter().any(|t| !own.contains(t) && spelled(t) && !recv.iter().any(|o| own.contains(o) && spelled(o)));
            if by_name || by_type {
                CALL_CONF_RECEIVER
            } else if unique {
                CALL_CONF_METHOD_UNIQUE
            } else {
                CALL_CONF_OTHER
            }
        }
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

/// True when `word` is `name` with letters left out (`toks` for `tokens`),
/// first letter kept.
fn abbreviates(word: &str, name: &str) -> bool {
    let mut it = name.chars();
    word.chars().next() == name.chars().next() && word.chars().all(|c| it.any(|n| n == c))
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

/// The types a `.x` call's receiver `x` is declared with anywhere in the
/// project (`code_fields`); empty for any other qualifier.
fn receiver_types(conn: &Connection, project_id: i64, qual: &str, cache: &mut HashMap<String, HashSet<String>>) -> Result<HashSet<String>> {
    let Some(word) = qual.strip_prefix('.').filter(|w| !w.is_empty()) else { return Ok(HashSet::new()) };
    if let Some(t) = cache.get(word) {
        return Ok(t.clone());
    }
    let mut stmt = conn.prepare_cached("SELECT DISTINCT type FROM code_fields WHERE project_id = ?1 AND name = ?2")?;
    let t: HashSet<String> = stmt.query_map(params![project_id, word], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    cache.insert(word.to_string(), t.clone());
    Ok(t)
}

fn defined_once(conn: &Connection, project_id: i64, name: &str) -> Result<bool> {
    let n: i64 = conn.query_row("SELECT count(DISTINCT path) FROM code_defs WHERE project_id = ?1 AND name = ?2 AND kind = 'fn'", params![project_id, name], |r| r.get(0))?;
    Ok(n <= 1)
}

/// Who calls the definition `d`: (caller, file, line, confidence), most
/// confident first, then by file and line.
/// The type owning `d` (`code_defs.owner`), empty for a free definition.
fn owner_of(conn: &Connection, project_id: i64, d: &Def) -> Result<String> {
    Ok(conn
        .prepare_cached("SELECT owner FROM code_defs WHERE project_id = ?1 AND path = ?2 AND name = ?3 AND line = ?4")?
        .query_row(params![project_id, d.path, d.name, d.line], |r| r.get::<_, Option<String>>(0))
        .optional()?
        .flatten()
        .unwrap_or_default())
}

/// A call written through the definition's own type (`RustBridge.f(` on a
/// Kotlin `object RustBridge`, `Foo::f(`) names it as exactly as a path does.
fn through_owner(qual: &str, owner: &str, conf: f32) -> f32 {
    let q = qual.trim_start_matches('&');
    if !owner.is_empty() && (q.strip_suffix('.') == Some(owner) || q.strip_suffix("::") == Some(owner)) {
        conf.max(crate::limits::CALL_CONF_QUALIFIED)
    } else {
        conf
    }
}

pub fn callers_for(conn: &Connection, project_id: i64, d: &Def) -> Result<Vec<(String, String, u32, f32)>> {
    let owner = owner_of(conn, project_id, d)?;
    let unique = defined_once(conn, project_id, &d.name)?;
    let own = own_names(conn, project_id, &d.path)?;
    let homes: HashSet<String> = defs_named(conn, project_id, &d.name)?.into_iter().filter(|x| x.kind == "fn").map(|x| x.path).collect();
    let mut recv_cache: HashMap<String, HashSet<String>> = HashMap::new();
    let mut stmt = conn.prepare_cached("SELECT caller, src, line, qual FROM code_calls WHERE project_id = ?1 AND callee = ?2 ORDER BY src, line")?;
    let rows: Vec<(String, String, u32, String)> = stmt
        .query_map(params![project_id, d.name], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u32, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // One line per calling function: its most confident way of calling.
    let mut out: Vec<(String, String, u32, f32)> = Vec::with_capacity(rows.len());
    for (c, f, l, q) in rows {
        let recv = receiver_types(conn, project_id, &q, &mut recv_cache)?;
        let conf = through_owner(&q, &owner, call_confidence(&q, unique, f == d.path, &own, f != d.path && homes.contains(&f), &recv));
        match out.iter_mut().find(|o| o.0 == c && o.1 == f) {
            Some(o) if o.3 < conf => *o = (c, f, l, conf),
            Some(_) => {}
            None => out.push((c, f, l, conf)),
        }
    }
    out.sort_by(|a, b| b.3.total_cmp(&a.3));
    Ok(out)
}

/// The confidence of one call to `d` written with qualifier `qual` in file
/// `f`, judged as `callers_for` judges an edge.
pub fn site_confidence(conn: &Connection, project_id: i64, d: &Def, f: &str, qual: &str) -> Result<f32> {
    let unique = defined_once(conn, project_id, &d.name)?;
    let own = own_names(conn, project_id, &d.path)?;
    let homes: HashSet<String> = defs_named(conn, project_id, &d.name)?.into_iter().filter(|x| x.kind == "fn").map(|x| x.path).collect();
    let recv = receiver_types(conn, project_id, qual, &mut HashMap::new())?;
    Ok(through_owner(qual, &owner_of(conn, project_id, d)?, call_confidence(qual, unique, f == d.path, &own, f != d.path && homes.contains(f), &recv)))
}

/// What `d` calls: (callee, confidence) in order of first call, the
/// confidence being the best over the callee's definitions.
pub fn callees_for(conn: &Connection, project_id: i64, d: &Def) -> Result<Vec<(String, f32)>> {
    let mut stmt = conn.prepare_cached("SELECT callee, qual FROM code_calls WHERE project_id = ?1 AND caller = ?2 AND src = ?3 ORDER BY line")?;
    let rows: Vec<(String, String)> = stmt.query_map(params![project_id, d.name, d.path], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<(String, f32)> = Vec::with_capacity(rows.len());
    for (callee, qual) in rows {
        let unique = defined_once(conn, project_id, &callee)?;
        let targets: Vec<Def> = defs_named(conn, project_id, &callee)?.into_iter().filter(|t| t.kind == "fn").collect();
        let local = targets.iter().any(|t| t.path == d.path);
        let recv = receiver_types(conn, project_id, &qual, &mut HashMap::new())?;
        let mut best = 0f32;
        for t in &targets {
            best = best.max(through_owner(&qual, &owner_of(conn, project_id, t)?, call_confidence(&qual, unique, t.path == d.path, &own_names(conn, project_id, &t.path)?, local && t.path != d.path, &recv)));
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

fn all_callers_under(callers: &[(String, String, u32, f32)], min: f32) -> Vec<String> {
    callers.iter().filter(|c| c.3 < min).map(|(c, f, l, p)| format!("{c} {f}:{l} {p:.1}")).collect()
}

const COMPLETE_NOTE: &str = "; complete for direct calls, not through traits, macros or pointers";

/// With several definitions of one name, `call_lines`' explanations are said
/// once after them, not under each (`links verify`: ten definitions, 3.6 KB,
/// most of it the same tail ten times). The line keeps its count as
/// `(+N unlikely)`; the flags say which explanation `calls_footer` owes.
pub fn compact_call_line(l: &str) -> (String, bool, bool) {
    let complete = l.contains(COMPLETE_NOTE);
    let mut s = l.replace(COMPLETE_NOTE, "");
    let mut unlikely = false;
    if let Some(i) = s.find(" (+") {
        if let Some(n) = s[i + 3..].split(' ').next().filter(|n| n.chars().all(|c| c.is_ascii_digit())) {
            if let Some(j) = s[i..].find("--all lists them)") {
                let n = n.to_string();
                s.replace_range(i..i + j + "--all lists them)".len(), &format!(" (+{n} unlikely)"));
                unlikely = true;
            }
        }
    }
    (s, unlikely, complete)
}

/// The explanations `compact_call_line` took out, said once.
pub fn calls_footer(name: &str, unlikely: bool, complete: bool) -> Vec<String> {
    let mut out = Vec::new();
    if unlikely {
        out.push(format!("+N unlikely: calls by name only, under {:.1}: another `{name}` or a call on a value of unknown type; --all lists them", crate::limits::CALL_MIN_CONF));
    }
    if complete {
        out.push("callers listed are complete for direct calls, not through traits, macros or pointers".to_string());
    }
    out
}

/// The two call lines `links <symbol>` prints under `defined:`, bounded by
/// `limits::CONTEXT_ITEMS` names each: who calls it, with file and line,
/// and what it calls. Empty for a name with no call edges (a constant, a
/// type), where `users_line` applies instead.
/// `every`: every caller and callee, and the unlikely callers listed too.
/// Callers are listed up to `limits::CONTEXT_SITES` (a "who calls this"
/// answer cut at four sent the agent to grep for the rest), and with `root`
/// each caller carries every line it calls from (`run src/main.rs:1185,1257`).
pub fn call_lines(conn: &Connection, project_id: i64, d: &Def, with_callers: bool, every: bool, root: Option<&Path>) -> Result<Vec<String>> {
    let cap = if every { usize::MAX } else { crate::limits::CONTEXT_ITEMS };
    let caller_cap = if every { usize::MAX } else { crate::limits::CONTEXT_SITES };
    let mut out = Vec::new();
    let min = crate::limits::CALL_MIN_CONF;
    let all = if with_callers { callers_for(conn, project_id, d)? } else { Vec::new() };
    let unlikely = all.iter().filter(|c| c.3 < min).count();
    let callers: Vec<_> = all.into_iter().filter(|c| c.3 >= min).collect();
    let unlikely_list: Vec<String> = if every && with_callers { all_callers_under(&callers_for(conn, project_id, d)?, min) } else { Vec::new() };
    let tail = if unlikely > 0 && !every { format!(" (+{unlikely} by name only, under {min:.1}: another `{}` or a call on a value of unknown type; --all lists them)", d.name) } else { String::new() };
    if !callers.is_empty() {
        let sites = match root {
            Some(r) => crate::spans::call_site_lines(conn, project_id, r, d, &callers)?,
            None => Vec::new(),
        };
        let shown: Vec<String> = callers
            .iter()
            .take(caller_cap)
            .map(|(c, f, l, p)| {
                let mut ls: Vec<u32> = sites.iter().filter(|s| &s.0 == f && &s.2.name == c).map(|s| s.1).collect();
                if ls.is_empty() {
                    ls.push(*l);
                }
                ls.dedup();
                let ls: Vec<String> = ls.iter().map(u32::to_string).collect();
                format!("{c} {f}:{} {p:.1}", ls.join(","))
            })
            .collect();
        let more = if callers.len() > caller_cap { format!(" +{} more", callers.len() - caller_cap) } else { String::new() };
        // Said once, so the agent does not grep to confirm a complete list.
        // Not with callers held back as unlikely: one of them may be real.
        let complete = if more.is_empty() && root.is_some() && unlikely == 0 { COMPLETE_NOTE } else { "" };
        out.push(format!("called by: {}  {}{more}{tail}{complete}", callers.len(), shown.join(", ")));
    } else if unlikely > 0 {
        out.push(format!("called by: 0{tail}"));
    }
    if !unlikely_list.is_empty() {
        out.push(format!("by name only: {}  {}", unlikely_list.len(), unlikely_list.join(", ")));
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
    #[test]
    fn header_is_the_first_comment_block() {
        let sh = "#!/bin/sh\n# Every blocker per document,\n# ranked.\nexport LC_ALL=C\n";
        assert_eq!(super::header(sh), ("Every blocker per document, ranked.".to_string(), 3));
        let rs = "//! The edit gate.\n//! Once per file.\n\nuse x;\n";
        assert_eq!(super::header(rs), ("The edit gate. Once per file.".to_string(), 2));
        let py = "\"\"\"Fetch the corpus\nonce.\"\"\"\nimport os\n";
        assert_eq!(super::header(py), ("Fetch the corpus once.".to_string(), 2));
        assert_eq!(super::header("fn main() {}\n").0, "");
    }

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
        assert_eq!(c, vec![("outer".to_string(), "helper".to_string(), 2), ("outer".to_string(), "thing".to_string(), 5), ("K".to_string(), "helper".to_string(), 10)], "a top-level constant's initializer calls too: {c:?}");
    }

    #[test]
    fn tables_and_function_references_are_callers() {
        let names: HashMap<String, usize> = ["vspace", "render", "trim", "helper"].iter().map(|s| (s.to_string(), 1)).collect();
        let src = "static DOC: [(&str, Native); 2] = [\n    (\"vspace\", |m| m.vspace()),\n    (\"x\", |m| m.render()),\n];\nfn run(v: Vec<X>) {\n    v.iter().map(Self::render);\n    v.split(',').map(str::trim);\n    use crate::a::helper;\n}\nfn vspace() {}\nfn render() {}\nfn trim() {}\nfn helper() {}\n";
        let c: Vec<(String, String, String)> = calls_in("a.rs", src, &names).into_iter().map(|(a, b, _, q)| (a, b, q)).collect();
        assert!(c.contains(&("DOC".to_string(), "vspace".to_string(), ".m".to_string())), "{c:?}");
        assert!(c.contains(&("DOC".to_string(), "render".to_string(), ".m".to_string())), "{c:?}");
        assert!(c.contains(&("run".to_string(), "render".to_string(), "&self".to_string())), "passed by name: {c:?}");
        assert!(!c.iter().any(|x| x.1 == "trim" || x.1 == "helper"), "a standard name or an import is not a reference: {c:?}");
        // A local constant in a callback is not a caller; a declaration calls nothing.
        let ts = "test('x', () => {\n  const k = render();\n});\n";
        assert!(calls_in("a.ts", ts, &names).is_empty());
        let kt = "object Bridge {\n    external fun render(k: String): Boolean\n}\n";
        assert!(calls_in("a.kt", kt, &names).is_empty());
    }

    #[test]
    fn qualifier_says_how_a_call_is_written() {
        assert_eq!(qualifier("let x = helper(1);", "helper").as_deref(), Some(""));
        assert_eq!(qualifier("self.helper(1)", "helper").as_deref(), Some("self"));
        assert_eq!(qualifier("Self::helper(1)", "helper").as_deref(), Some("self"));
        assert_eq!(qualifier("store::helper(1)", "helper").as_deref(), Some("store::"));
        assert_eq!(qualifier("Store.helper(1)", "helper").as_deref(), Some("Store."));
        assert_eq!(qualifier("x.y.helper(1)", "helper").as_deref(), Some(".y"));
        assert_eq!(qualifier("foo().helper(1)", "helper").as_deref(), Some("."));
        assert_eq!(qualifier("my_helper(1); helper(2)", "helper").as_deref(), Some(""));
        assert_eq!(qualifier("search::stem(x)", "search"), None, "a module path");
        assert_eq!(qualifier("parse::<u32>(x)", "parse").as_deref(), Some(""));
        let own: HashSet<String> = ["store".to_string(), "Store".to_string()].into_iter().collect();
        let none = HashSet::new();
        assert!(call_confidence("store::", false, false, &own, false, &none) > call_confidence("other::", false, false, &own, false, &none));
        assert!(call_confidence("", false, true, &own, false, &none) > call_confidence("", false, false, &own, false, &none));
        assert!(call_confidence(".", false, false, &own, false, &none) < crate::limits::CALL_MIN_CONF);
        // `self.f(` in a file with its own `f` is not a call to this one.
        assert!(call_confidence("self", false, false, &own, true, &none) < crate::limits::CALL_MIN_CONF);
        assert_eq!(call_confidence("self", false, true, &own, false, &none), crate::limits::CALL_CONF_SELF);
        // A receiver declared with a type defined here points here.
        let toks: HashSet<String> = ["Store".to_string()].into_iter().collect();
        assert_eq!(call_confidence(".toks", false, false, &own, false, &toks), crate::limits::CALL_CONF_RECEIVER);
        let other: HashSet<String> = ["Nodes".to_string()].into_iter().collect();
        assert!(call_confidence(".nodes", false, false, &own, false, &other) < crate::limits::CALL_MIN_CONF);
        // `toks` declared `Table` here and `Tokens` elsewhere: the name spells `Tokens`.
        let node_own: HashSet<String> = ["Nodes".to_string(), "Table".to_string()].into_iter().collect();
        let mixed: HashSet<String> = ["Table".to_string(), "Tokens".to_string(), "Vec".to_string()].into_iter().collect();
        assert!(call_confidence(".toks", false, false, &node_own, false, &mixed) < crate::limits::CALL_MIN_CONF);
        let tok_own: HashSet<String> = ["Tokens".to_string()].into_iter().collect();
        assert_eq!(call_confidence(".toks", false, false, &tok_own, false, &mixed), crate::limits::CALL_CONF_RECEIVER);
        assert_eq!(fields("    pub toks: Tokens,\n    nodes: &mut Nodes,\n    let x: u32 = 1;\n    path::Foo"), vec![("toks".into(), "Tokens".into()), ("nodes".into(), "Nodes".into())]);
    }
}

/// An identifier's words, lowercase: `encaps_internal` gives "encaps
/// internal", `BapSessionManager` "bap session manager", `HTTPServer` "http
/// server".
pub fn ident_words(id: &str) -> Vec<String> {
    let mut out = Vec::new();
    for part in id.split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()) {
        let cs: Vec<char> = part.chars().collect();
        let mut cur = String::new();
        for (i, &c) in cs.iter().enumerate() {
            let boundary = i > 0
                && c.is_ascii_uppercase()
                && (cs[i - 1].is_ascii_lowercase() || cs[i - 1].is_ascii_digit() || (cs[i - 1].is_ascii_uppercase() && cs.get(i + 1).is_some_and(|n| n.is_ascii_lowercase())));
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur.push(c.to_ascii_lowercase());
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// What `find` matches a definition by: its name's words, then its
/// signature and the comment block above it (attributes skipped, at most
/// `CODE_FIND_DOC_BYTES`), identifiers split into words.
fn find_words(lines: &[&str], d: &Def) -> (String, String) {
    let name = ident_words(&d.name).join(" ");
    let mut doc: Vec<&str> = Vec::new();
    let mut test = d.name.starts_with("test_") || d.name.starts_with("test") && d.name[4..].starts_with(char::is_uppercase);
    let mut i = (d.line as usize).saturating_sub(1);
    while i > 0 {
        i -= 1;
        let t = lines.get(i).map(|l| l.trim()).unwrap_or("");
        if t.starts_with("#[") || (t.starts_with('@') && !t.starts_with("@param")) || (t.starts_with('[') && t.ends_with(']')) {
            // `#[test]`, `#[tokio::test]`, `@Test`, `[Fact]`: a test, ranked last.
            test |= t.contains("test") || t.contains("Test") || t.contains("Fact");
            continue;
        }
        let preprocessor = ["#!", "#include", "#define", "#if", "#else", "#end", "#pragma", "#import"].iter().any(|p| t.starts_with(p));
        let body = ["///", "//!", "//", "/**", "/*", "*/", "*", "#", "--"].iter().find_map(|p| t.strip_prefix(p));
        match body {
            Some(b) if !preprocessor => doc.push(b.trim()),
            _ => break,
        }
        if doc.iter().map(|l| l.len()).sum::<usize>() > crate::limits::CODE_FIND_DOC_BYTES {
            break;
        }
    }
    doc.reverse();
    // A Python docstring sits under the definition line.
    if let Some(next) = lines.get(d.line as usize).map(|l| l.trim()) {
        if next.starts_with("\"\"\"") || next.starts_with("'''") {
            doc.push(next.trim_matches(|c| c == '"' || c == '\''));
        }
    }
    let mut text = String::new();
    // Where it lives says what it belongs to: `ams_lite_backend.rs`,
    // `dcp-pq-fips204/src/mldsa.rs`.
    for p in d.path.rsplit_once('.').map_or(d.path.as_str(), |(a, _)| a).split('/').filter(|p| !matches!(*p, "src" | "lib" | "main" | "mod" | "crates" | "app" | "java" | "com" | "kotlin")) {
        for w in ident_words(p) {
            text.push_str(&w);
            text.push(' ');
        }
    }
    'outer: for w in d.anchor.split_whitespace().chain(doc.iter().flat_map(|l| l.split_whitespace())) {
        for p in ident_words(w) {
            if text.len() + p.len() > crate::limits::CODE_FIND_DOC_BYTES * 2 {
                break 'outer;
            }
            text.push_str(&p);
            text.push(' ');
        }
    }
    if test {
        text.push_str(TEST_MARK);
    }
    (name, text.trim_end().to_string())
}

/// Appended to a test's find words, so `find` ranks it after the code.
const TEST_MARK: &str = " zztestzz";

/// Words of a question that say nothing about which code it means.
const FIND_STOP: &[&str] = &[
    "function", "functions", "method", "methods", "call", "calls", "called", "caller", "callers", "calling", "defined", "define", "definition", "where", "which", "what",
    "change", "changes", "changing", "signature", "site", "sites", "must", "want", "need", "including", "included", "tests", "test", "code", "file", "files", "line",
    "lines", "does", "the", "and", "for", "that", "this", "with", "from", "into", "its", "their", "there", "here", "when", "then", "than", "also", "other", "one",
    "all", "any", "each", "every", "both", "only", "just", "now", "new", "our", "your", "use", "used", "using", "uses", "handle", "handles", "done", "check",
];

/// Words that ask for a function, not a type or a constant.
const FN_CUES: &[&str] = &["function", "functions", "fn", "method", "methods", "routine", "call", "calls", "called", "caller", "callers", "signature", "parameter", "argument", "invoked"];

/// The words `find` searches for: the question's terms split like
/// identifiers, three letters or more, minus question words.
pub fn find_terms(question: &str) -> Vec<String> {
    let mut out: Vec<String> = crate::fts::terms(question)
        .into_iter()
        .flat_map(ident_words)
        .filter(|w| w.len() >= 3 && !crate::search::STOPWORDS.contains(&w.as_str()) && !FIND_STOP.contains(&w.as_str()))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Definitions a question describes without naming them: BM25 over the
/// words of each definition's name (weighted 4) and its signature and the
/// comment above it, kept when at least `CODE_FIND_MIN_WORDS` of the
/// question's words are present; tests rank after the rest.
pub fn find(conn: &Connection, project_id: i64, question: &str, k: usize) -> Result<Vec<(Def, usize)>> {
    let terms = find_terms(question);
    if terms.len() < crate::limits::CODE_FIND_MIN_WORDS {
        return Ok(Vec::new());
    }
    // "Which function", "the method that", "call sites": a type named like
    // the question's area is not the answer.
    let lower = question.to_ascii_lowercase();
    let wants_fn = crate::fts::terms(&lower).iter().any(|t| FN_CUES.contains(t));
    let prefix = |t: &str| -> String {
        let s = crate::search::stem(t);
        if s.len() >= 3 { s.to_string() } else { t.to_string() }
    };
    let q = terms.iter().map(|t| format!("\"{t}\" OR \"{}\"*", prefix(t))).collect::<Vec<_>>().join(" OR ");
    let mut stmt = conn.prepare_cached(
        "SELECT d.path, d.name, d.kind, d.line, d.end_line, d.anchor, d.name_words, d.words
         FROM code_defs_fts f JOIN code_defs d ON d.id = f.rowid
         WHERE code_defs_fts MATCH ?1 AND d.project_id = ?2
         ORDER BY bm25(code_defs_fts, 4.0, 1.0) LIMIT 2000",
    )?;
    let rows = stmt.query_map(params![q, project_id], |r| {
        let kind: String = r.get(2)?;
        Ok((
            Def { path: r.get(0)?, name: r.get(1)?, kind: kind_str(&kind), line: r.get::<_, i64>(3)? as u32, end: r.get::<_, i64>(4)? as u32, anchor: r.get(5)? },
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    let prefixes: Vec<String> = terms.iter().map(|t| prefix(t)).collect();
    let holds = |words: &str, p: &str| words.split(' ').any(|w| w.starts_with(p));
    let mut hits: Vec<(Def, usize, usize, usize, bool)> = Vec::new();
    for (rank, row) in rows.enumerate() {
        let (d, name, words) = row?;
        let in_name = prefixes.iter().filter(|p| holds(&name, p)).count();
        let n = prefixes.iter().filter(|p| holds(&name, p) || holds(&words, p)).count();
        if n >= crate::limits::CODE_FIND_MIN_WORDS {
            let test = words.ends_with(TEST_MARK.trim_start()) || d.path.contains("test") || d.path.contains("examples/") || d.path.contains("bench");
            hits.push((d, n, in_name, rank, test));
        }
    }
    // A word in the name says what the definition is; one in its comment or
    // signature only what it touches: name words count twice. Then BM25's
    // order; tests last.
    hits.sort_by_key(|(d, n, in_name, rank, test)| (*test, wants_fn && d.kind != "fn", std::cmp::Reverse(n + in_name), *rank));
    Ok(hits.into_iter().take(k).map(|(d, n, ..)| (d, n)).collect())
}

/// For `links <name>` when nothing is defined by that name: the definitions
/// its words describe, so a guessed name (`sign_internal`) leads to the real
/// one. Empty for a file path.
/// Whether a links target is a file: a path, or a dotted name whose tail
/// is not a definition (`store.rs`, not `MediaBridge.encrypt`).
pub fn is_file_target(conn: &Connection, project_id: i64, target: &str) -> Result<bool> {
    // `path/file.rs::name` is a definition in that file.
    if let Some((_, name)) = target.rsplit_once("::") {
        if target.contains('/') && !name.contains('/') && !name.contains('.') {
            return Ok(false);
        }
    }
    if target.contains('/') {
        return Ok(true);
    }
    if !target.contains('.') || target.contains("::") {
        return Ok(false);
    }
    let file: bool = conn
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM code_files WHERE project_id = ?1 AND (path = ?2 OR path LIKE '%/' || ?2))")?
        .query_row(params![project_id, target], |r| r.get(0))?;
    Ok(file || defs_named(conn, project_id, target)?.is_empty())
}

pub fn nearest_line(conn: &Connection, project_id: i64, target: &str) -> Result<Option<String>> {
    if target.contains('/') || target.rsplit_once('.').is_some_and(|(_, e)| e.len() <= 4 && e.chars().all(|c| c.is_ascii_lowercase())) {
        return Ok(None);
    }
    let want = ident_words(target);
    // A guessed name's words must stand whole in the definition's name or
    // signature: as prefixes, `sign` found `isInternalSignal`.
    let hits: Vec<_> = find(conn, project_id, &want.join(" "), crate::limits::CODE_FIND_K * 3)?
        .into_iter()
        .filter(|(d, _)| {
            let have: HashSet<String> = ident_words(&d.name).into_iter().chain(d.anchor.split_whitespace().flat_map(ident_words)).collect();
            want.iter().filter(|w| have.contains(*w)).count() >= crate::limits::CODE_FIND_MIN_WORDS.min(want.len())
        })
        .take(crate::limits::CODE_FIND_K)
        .collect();
    // One word (`encaps`) is under `find`'s two-word minimum: the
    // definitions whose name holds it as a whole word (`encaps_internal`),
    // functions first, shortest names first. Codex guessed `links encaps`
    // and was told nothing was near, then listed files.
    let hits = if hits.is_empty() && want.len() == 1 && want[0].len() >= 4 {
        let w = &want[0];
        let mut stmt = conn.prepare_cached("SELECT path, name, kind, line, end_line, anchor FROM code_defs WHERE project_id = ?1 AND name LIKE ?2 AND kind != 'variant' LIMIT 400")?;
        let mut v: Vec<(Def, usize)> = stmt
            .query_map(params![project_id, format!("%{w}%")], |r| {
                let kind: String = r.get(2)?;
                Ok(Def { path: r.get(0)?, name: r.get(1)?, kind: kind_str(&kind), line: r.get::<_, i64>(3)? as u32, end: r.get::<_, i64>(4)? as u32, anchor: r.get(5)? })
            })?
            .filter_map(|d| d.ok())
            .filter(|d| ident_words(&d.name).iter().any(|x| x == w))
            .map(|d| (d, 1))
            .collect();
        v.sort_by_key(|(d, _)| (d.kind != "fn", d.name.len(), d.path.clone()));
        v.dedup_by(|a, b| a.0.name == b.0.name && a.0.path == b.0.path);
        v.truncate(crate::limits::CODE_FIND_K);
        v
    } else {
        hits
    };
    // Said even with nothing near: "no notes" alone reads as "found, unnoted".
    if hits.is_empty() {
        return Ok(Some(format!("no definition named {target} in the code index")));
    }
    let list: Vec<String> = hits.iter().map(|(d, _)| format!("{} {}:{}-{} {}", d.name, d.path, d.line, d.end, d.kind)).collect();
    Ok(Some(format!("no definition named {target}; nearest by its words: {}", list.join(", "))))
}

/// The type each definition belongs to, in `defs`' order: the innermost
/// `impl X` / `impl Trait for X` block (Rust), class, struct, trait or
/// interface body holding it, a Go method's receiver type, or a C++ `X::`
/// before the name. Empty for a free definition. Line scanning, like the
/// rest: an `impl` is a line whose first word is `impl` (after `unsafe`).
pub fn owners(path: &str, lines: &[&str], defs: &[Def]) -> Vec<String> {
    let py = matches!(ext_of(path), "py" | "rb");
    // (start, end, owner) of every block that owns what it holds.
    let mut blocks: Vec<(u32, u32, String)> = Vec::new();
    if ext_of(path) == "rs" {
        for (i, raw) in lines.iter().enumerate() {
            let l = raw.trim_start();
            let l = l.strip_prefix("unsafe ").unwrap_or(l);
            let Some(rest) = l.strip_prefix("impl") else { continue };
            if !(rest.starts_with(' ') || rest.starts_with('<')) {
                continue;
            }
            let head: String = lines[i..lines.len().min(i + 6)].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
            let head = head.split('{').next().unwrap_or("").split(" where ").next().unwrap_or("");
            let head = skip_generics(head.trim_start_matches("unsafe ").trim_start_matches("impl"));
            let ty = head.rsplit_once(" for ").map_or(head, |(_, t)| t);
            let name = rust_type_name(ty);
            if !name.is_empty() {
                blocks.push((i as u32 + 1, span_end(lines, i, false) as u32 + 1, name));
            }
        }
    }
    for d in defs.iter().filter(|d| d.kind == "type" && d.end > d.line) {
        blocks.push((d.line, d.end, d.name.clone()));
    }
    defs.iter()
        .map(|d| {
            if d.kind != "fn" && d.kind != "const" && d.kind != "val" {
                return String::new();
            }
            if ext_of(path) == "go" {
                if let Some(r) = go_receiver(&d.anchor) {
                    return r;
                }
            }
            // `void Foo::bar(int)`: the qualifier is the owner.
            if let Some(i) = d.anchor.find(&format!("::{}(", d.name)) {
                let q: String = d.anchor[..i].chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !q.is_empty() {
                    return q.chars().rev().collect();
                }
            }
            blocks
                .iter()
                .filter(|(a, b, n)| *a < d.line && d.line <= *b && *n != d.name)
                .min_by_key(|(a, b, _)| b - a)
                .map(|(.., n)| n.clone())
                .filter(|_| !py || d.kind == "fn")
                .unwrap_or_default()
        })
        .collect()
}

/// `<T: Clone> Foo<T>` to `Foo<T>`: drops a leading generic list.
fn skip_generics(s: &str) -> &str {
    let s = s.trim_start();
    if !s.starts_with('<') {
        return s;
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return s[i + 1..].trim_start();
                }
            }
            _ => {}
        }
    }
    s
}

/// The type's own name in `&'a mut crate::x::Foo<T>`: `Foo`.
fn rust_type_name(ty: &str) -> String {
    let ty = ty.trim().trim_start_matches('&').trim_start();
    let ty = ty.strip_prefix('\'').map_or(ty, |r| r.split_once(' ').map_or("", |(_, t)| t)).trim_start();
    let ty = ty.trim_start_matches("mut ").trim_start_matches("dyn ");
    let base = ty.split('<').next().unwrap_or("");
    base.rsplit("::").next().unwrap_or("").trim().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect()
}

/// `func (s *Store) Size() int`: `Store`.
fn go_receiver(anchor: &str) -> Option<String> {
    let after = anchor.strip_prefix("func")?.trim_start().strip_prefix('(')?;
    let inner = &after[..after.find(')')?];
    let ty = inner.split_whitespace().last()?.trim_start_matches('*');
    let ty: String = ty.split('[').next()?.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!ty.is_empty()).then_some(ty)
}
