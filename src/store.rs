//! Save, forget, status and reindex. The note text is the only source of
//! truth; chunks and vectors are derived here and can always be rebuilt.

use crate::chunk;
use crate::embed::{self, Embedder, Space};
use crate::project::Project;
use crate::{err, secrets, sys, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Unchanged,
    Added,
    Updated,
}

pub struct SaveOptions<'a> {
    pub source: &'a str,
    pub force: bool,
}

/// Saves one note. Unchanged content is skipped by hash. When the content
/// changed, the old chunks are replaced; vectors of chunks whose text did not
/// change are carried over so the model only sees new text.
pub fn save_note(
    conn: &mut Connection,
    project: &Project,
    rel_path: &str,
    content: &str,
    opts: &SaveOptions,
) -> Result<SaveOutcome> {
    if !opts.force {
        if secrets::suspicious_name(rel_path) {
            return Err(err!("{rel_path}: looks like a secrets file; pass --force to save it"));
        }
        if let Some(what) = secrets::suspicious_content(content) {
            return Err(err!("{rel_path}: contains what looks like a {what}; pass --force to save it"));
        }
    }
    let hash = Sha256::digest(content.as_bytes()).to_vec();
    let existing: Option<(i64, Vec<u8>, Option<i64>)> = conn
        .query_row(
            "SELECT id, content_hash, deleted_at FROM notes WHERE project_id = ?1 AND path = ?2",
            params![project.id, rel_path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((_, old_hash, None)) = &existing {
        if *old_hash == hash {
            return Ok(SaveOutcome::Unchanged);
        }
    }
    let fallback = Path::new(rel_path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let title = chunk::title_of(content, &fallback);
    let supersedes = chunk::supersedes(content).map(|t| crate::graph::resolve_note_ref(rel_path, &t));
    let author = chunk::author(content);
    let dated = chunk::dated(content);
    let source = author.as_deref().unwrap_or(opts.source);
    let words = content.split_whitespace().count();
    if words > crate::limits::NOTE_MAX_WORDS {
        eprintln!("memorycastle: {rel_path}: {words} words; split it into notes of one decision each so search returns the answer, not a document");
    }
    for w in lint(content) {
        eprintln!("memorycastle: {rel_path}: {w}");
    }
    if chunk::dated(content).is_some() {
        let missing = files_missing_from_note(conn, project.id, rel_path, content)?;
        if !missing.is_empty() {
            eprintln!("memorycastle: {rel_path}: `Files:` does not name {} edited this session; add what this task touched", missing.join(", "));
        }
    }
    let chunks = chunk::split(content);
    let now = sys::now();
    let active = embed::active_space(conn)?;

    let tx = conn.transaction()?;
    let (note_id, outcome) = match existing {
        Some((id, _, _)) => {
            tx.execute(
                "UPDATE notes SET title = ?1, content = ?2, content_hash = ?3, source = ?4, saved_at = ?5, deleted_at = NULL, supersedes = ?7, dated = ?8 WHERE id = ?6",
                params![title, content, hash, source, now, id, supersedes, dated],
            )?;
            (id, SaveOutcome::Updated)
        }
        None => {
            tx.execute(
                "INSERT INTO notes(project_id, path, title, content, content_hash, source, saved_at, deleted_at, supersedes, dated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9)",
                params![project.id, rel_path, title, content, hash, source, now, supersedes, dated],
            )?;
            (tx.last_insert_rowid(), SaveOutcome::Added)
        }
    };

    crate::graph::store_links(&tx, note_id, project.id, rel_path, content)?;

    // Supersession works in either save order: mark the note this one replaces,
    // and take over any claim an already-saved note makes on this path.
    tx.execute(
        "UPDATE notes SET superseded_by = NULL WHERE project_id = ?1 AND superseded_by = ?2",
        params![project.id, rel_path],
    )?;
    if let Some(target) = &supersedes {
        tx.execute(
            "UPDATE notes SET superseded_by = ?3 WHERE project_id = ?1 AND path = ?2",
            params![project.id, target, rel_path],
        )?;
    }
    tx.execute(
        "UPDATE notes SET superseded_by = (SELECT path FROM notes n2 WHERE n2.project_id = ?1 AND n2.supersedes = ?2 AND n2.deleted_at IS NULL ORDER BY n2.saved_at DESC LIMIT 1)
         WHERE id = ?3",
        params![project.id, rel_path, note_id],
    )?;

    // Keep vectors of unchanged chunk text: text -> vec blob.
    let mut kept: HashMap<String, Vec<u8>> = HashMap::new();
    if let (Some(space), SaveOutcome::Updated) = (&active, outcome) {
        let mut stmt = tx.prepare_cached(
            "SELECT c.heading_path, c.text, v.vec FROM chunks c JOIN vectors v ON v.chunk_id = c.id
             WHERE c.note_id = ?1 AND v.space_id = ?2",
        )?;
        for row in stmt.query_map(params![note_id, space.id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?))
        })? {
            let (h, t, v) = row?;
            kept.insert(chunk_key(&h, &t), v);
        }
    }
    tx.execute("DELETE FROM chunks WHERE note_id = ?1", [note_id])?;
    write_sections(&tx, note_id, content)?;
    crate::search::invalidate_bits(&tx, project.id)?;
    {
        let mut ins = tx.prepare_cached(
            "INSERT INTO chunks(note_id, project_id, ordinal, heading_path, text, text_indexed, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        let mut ins_vec = tx.prepare_cached(
            "INSERT INTO vectors(chunk_id, space_id, project_id, vec) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (i, c) in chunks.iter().enumerate() {
            let indexed = chunk::index_text(&c.text);
            ins.execute(params![note_id, project.id, i as i64, c.heading_path, c.text, indexed, c.line])?;
            if let Some(space) = &active {
                if let Some(v) = kept.get(&chunk_key(&c.heading_path, &c.text)) {
                    let chunk_id = tx.last_insert_rowid();
                    ins_vec.execute(params![chunk_id, space.id, project.id, v])?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(outcome)
}

fn chunk_key(heading: &str, text: &str) -> String {
    let mut k = String::with_capacity(heading.len() + text.len() + 1);
    k.push_str(heading);
    k.push('\u{1f}');
    k.push_str(text);
    k
}

/// Embeds every chunk in `space` that has no vector yet, in batches sorted by
/// length so padding is small. Returns the number embedded.
pub fn embed_missing(
    conn: &mut Connection,
    space: &Space,
    embedder: &mut dyn Embedder,
    project_id: Option<i64>,
) -> Result<usize> {
    if embedder.dims() != space.def.dims {
        return Err(err!("embedder produces {} dims but the space has {}", embedder.dims(), space.def.dims));
    }
    let sql = match project_id {
        Some(_) => "SELECT c.id, c.project_id, c.heading_path, c.text FROM chunks c
                    WHERE c.project_id = ?2 AND NOT EXISTS (SELECT 1 FROM vectors v WHERE v.chunk_id = c.id AND v.space_id = ?1)",
        None => "SELECT c.id, c.project_id, c.heading_path, c.text FROM chunks c
                 WHERE NOT EXISTS (SELECT 1 FROM vectors v WHERE v.chunk_id = c.id AND v.space_id = ?1)",
    };
    let mut pending: Vec<(i64, i64, String)> = {
        let mut stmt = conn.prepare(sql)?;
        let map = |r: &rusqlite::Row| -> rusqlite::Result<(i64, i64, String)> {
            let heading: String = r.get(2)?;
            let text: String = r.get(3)?;
            Ok((r.get(0)?, r.get(1)?, space.def.doc_text(&heading, &text)))
        };
        let rows = match project_id {
            Some(pid) => stmt.query_map(params![space.id, pid], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt.query_map(params![space.id], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
        };
        rows
    };
    if pending.is_empty() {
        return Ok(0);
    }
    // Longest first: the runtime's arena grows to fit the first large batch
    // and every later batch fits inside it, instead of growing step by step.
    pending.sort_by_key(|p| std::cmp::Reverse(p.2.len()));

    // Activations, not weights, set the peak, and batching buys no throughput
    // on the CPU (measured: 200 ms per chunk alone, 190 ms per chunk in a
    // batch of 8). One chunk per call keeps peak RSS close to the query path.
    let dims = space.def.dims;
    let mut floats: Vec<f32> = Vec::with_capacity(dims);
    let mut blob: Vec<u8> = Vec::with_capacity(dims * 2);
    let mut done = 0usize;
    for batch in pending.chunks(crate::limits::EMBED_BATCH) {
        floats.clear();
        let texts: Vec<&str> = batch.iter().map(|p| p.2.as_str()).collect();
        embedder.embed(&texts, &mut floats)?;
        if floats.len() != texts.len() * dims {
            return Err(err!("embedder returned {} floats for {} texts", floats.len(), texts.len()));
        }
        let tx = conn.transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR REPLACE INTO vectors(chunk_id, space_id, project_id, vec) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (i, (chunk_id, pid, _)) in batch.iter().enumerate() {
                embed::encode_f16(&floats[i * dims..(i + 1) * dims], &mut blob);
                ins.execute(params![chunk_id, space.id, pid, blob])?;
            }
        }
        if let Some((_, pid, _)) = batch.first() {
            crate::search::invalidate_bits(&tx, *pid)?;
        }
        tx.commit()?;
        done += batch.len();
    }
    Ok(done)
}

/// Soft-deletes a note (or hard-deletes with `purge`). Chunks and vectors go
/// either way: they are derived, and dropping them keeps the search path free
/// of a `deleted_at` join. Returns false when no such note exists.
pub fn forget(conn: &mut Connection, project_id: i64, path_or_id: &str, purge: bool) -> Result<bool> {
    let id: Option<i64> = match path_or_id.parse::<i64>() {
        Ok(n) => conn
            .query_row("SELECT id FROM notes WHERE id = ?1 AND project_id = ?2", params![n, project_id], |r| r.get(0))
            .optional()?,
        Err(_) => conn
            .query_row(
                "SELECT id FROM notes WHERE path = ?1 AND project_id = ?2",
                params![path_or_id, project_id],
                |r| r.get(0),
            )
            .optional()?,
    };
    let Some(id) = id else { return Ok(false) };
    let tx = conn.transaction()?;
    if purge {
        tx.execute("DELETE FROM notes WHERE id = ?1", [id])?;
    } else {
        tx.execute("DELETE FROM chunks WHERE note_id = ?1", [id])?;
        crate::search::invalidate_bits(&tx, project_id)?;
        tx.execute("UPDATE notes SET deleted_at = ?1 WHERE id = ?2", params![sys::now(), id])?;
    }
    tx.commit()?;
    Ok(true)
}

/// Removes a whole project's memory in one transaction.
pub fn forget_project(conn: &mut Connection, project_id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM projects WHERE id = ?1", [project_id])?;
    tx.execute("DELETE FROM vector_bits WHERE project_id = ?1", [project_id])?;
    tx.commit()?;
    Ok(())
}

/// Counts derived rows that still point at `project_id` notes that are
/// deleted, or at nothing. Zero means forgetting left nothing behind.
pub fn verify_clean(conn: &Connection) -> Result<i64> {
    let orphans: i64 = conn.query_row(
        "SELECT (SELECT count(*) FROM chunks c JOIN notes n ON n.id = c.note_id WHERE n.deleted_at IS NOT NULL)
              + (SELECT count(*) FROM chunks c WHERE NOT EXISTS (SELECT 1 FROM notes n WHERE n.id = c.note_id))
              + (SELECT count(*) FROM vectors v WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.id = v.chunk_id))
              + (SELECT count(*) FROM chunks_fts f WHERE NOT EXISTS (SELECT 1 FROM chunks c WHERE c.id = f.rowid))",
        [],
        |r| r.get(0),
    )?;
    Ok(orphans)
}

/// Rebuilds chunks from the notes of every live project (or one project), then
/// embeds whatever lacks a vector in `space`. Vectors for unchanged chunk text
/// are kept, as in `save_note`.
/// Rechunks every note once after `chunk::CHUNK_VERSION` changed. Vectors of
/// unchanged chunk text are kept; new chunks get theirs at the next embed.
pub fn ensure_chunk_version(conn: &mut Connection) -> Result<usize> {
    if crate::db::is_readonly(conn) || crate::db::meta_get(conn, "chunk_version")?.as_deref() == Some(chunk::CHUNK_VERSION) {
        return Ok(0);
    }
    let n = rechunk(conn, None)?;
    crate::db::meta_set(conn, "chunk_version", chunk::CHUNK_VERSION)?;
    Ok(n)
}

pub fn rechunk(conn: &mut Connection, project_id: Option<i64>) -> Result<usize> {
    let notes: Vec<(i64, i64, String)> = {
        let sql = match project_id {
            Some(_) => "SELECT id, project_id, content FROM notes WHERE deleted_at IS NULL AND project_id = ?1",
            None => "SELECT id, project_id, content FROM notes WHERE deleted_at IS NULL",
        };
        let mut stmt = conn.prepare(sql)?;
        let map = |r: &rusqlite::Row| Ok((r.get(0)?, r.get(1)?, r.get(2)?));
        match project_id {
            Some(pid) => stmt.query_map([pid], map)?.collect::<rusqlite::Result<_>>()?,
            None => stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?,
        }
    };
    let active = embed::active_space(conn)?;
    let mut total = 0usize;
    for (note_id, pid, content) in notes {
        let chunks = chunk::split(&content);
        let tx = conn.transaction()?;
        let mut kept: HashMap<String, Vec<u8>> = HashMap::new();
        if let Some(space) = &active {
            let mut stmt = tx.prepare_cached(
                "SELECT c.heading_path, c.text, v.vec FROM chunks c JOIN vectors v ON v.chunk_id = c.id
                 WHERE c.note_id = ?1 AND v.space_id = ?2",
            )?;
            for row in stmt.query_map(params![note_id, space.id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?))
            })? {
                let (h, t, v) = row?;
                kept.insert(chunk_key(&h, &t), v);
            }
        }
        tx.execute("DELETE FROM chunks WHERE note_id = ?1", [note_id])?;
        crate::search::invalidate_bits(&tx, pid)?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO chunks(note_id, project_id, ordinal, heading_path, text, text_indexed, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut ins_vec = tx.prepare_cached(
                "INSERT INTO vectors(chunk_id, space_id, project_id, vec) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (i, c) in chunks.iter().enumerate() {
                ins.execute(params![note_id, pid, i as i64, c.heading_path, c.text, chunk::index_text(&c.text), c.line])?;
                if let Some(space) = &active {
                    if let Some(v) = kept.get(&chunk_key(&c.heading_path, &c.text)) {
                        ins_vec.execute(params![tx.last_insert_rowid(), space.id, pid, v])?;
                    }
                }
            }
        }
        tx.commit()?;
        total += chunks.len();
    }
    Ok(total)
}

/// What the agent's hooks saw it read: bytes returned by each file read and
/// the file's whole size. A read is `guided` when a span for that file was
/// printed shortly before. Saving = whole − bytes over guided reads, with
/// the one stated assumption that the whole file would have been read.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Observed {
    pub reads: u64,
    pub read_bytes: u64,
    pub guided_reads: u64,
    pub guided_bytes: u64,
    pub guided_whole: u64,
    /// Bytes a grep-then-range read of each guided file would have cost.
    pub guided_range: u64,
    /// Saving against the whole file (ceiling).
    pub saved_tokens: u64,
    pub saved_pct: f64,
    /// Saving against a range read (floor).
    pub saved_tokens_min: u64,
    pub saved_pct_min: f64,
    /// Tokens of search/links output in the same scope, already subtracted
    /// from both savings figures.
    pub lookup_tokens: u64,
    /// Guided reads followed within the window by another read of the same file.
    pub rereads: u64,
    /// Files edited after a lookup printed a span for them, with no read of
    /// the file in the session: the lookup answered, the file stayed closed.
    pub avoided_reads: u64,
    pub avoided_whole: u64,
    pub avoided_range: u64,
}



/// Cumulative savings for a project: bytes printed by search and links
/// against the bytes of the notes and files they stood in for.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Usage {
    /// Guided reads by the rank of the span they followed: index 0 is a
    /// `links`/`code:` span, 1.. the `read first:` position. The label log
    /// that says whether the ranking is right.
    pub reads_by_rank: Vec<u64>,
    /// The label log: notes a search printed, and how many were opened after.
    pub served_notes: i64,
    pub opened_notes: i64,
    /// Observed by the agent's hooks, when installed: all time.
    pub observed: Observed,
    /// Observed in the current agent session (set by the session-start hook).
    pub observed_session: Observed,
    /// Observed over the `RECENT_SESSIONS` most recent sessions, and how many that was.
    pub observed_recent: Observed,
    pub recent_sessions: usize,
    pub session: Option<String>,
    pub calls_session: u64,
    /// Distinct files the `edits` ledger holds for the current session.
    pub edits_session: u64,
    pub saved_tokens_session: u64,
    pub saved_pct_session: f64,
    pub calls: u64,
    pub served: u64,
    pub stood_for: u64,
    /// Tokens not read, from bytes at `limits::BYTES_PER_TOKEN`.
    pub saved_tokens: u64,
    /// 0 to 100: share not read because the answer was printed instead.
    pub saved_pct: f64,
    pub calls_today: u64,
    pub saved_pct_today: f64,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Status {
    pub usage: Usage,
    pub project: Option<String>,
    pub project_id: Option<i64>,
    pub group: Option<String>,
    pub notes: i64,
    pub chunks: i64,
    pub vectors: i64,
    pub projects: i64,
    pub space: Option<String>,
    pub db_bytes: u64,
    pub notes_newer_on_disk: Vec<String>,
    /// `Files:` targets of live notes that no longer exist in the checkout, with note counts.
    pub dangling_files: Vec<(String, usize)>,
    /// Agent instruction files that still point at an old notes location.
    pub stale_instructions: Vec<String>,
    pub links: i64,
    pub last_eval: Option<serde_json::Value>,
}

/// Counts for the current project (or the whole store) and the notes whose
/// file on disk is newer than the saved copy (one `stat` per note, no git).
pub fn status(conn: &mut Connection, project: Option<&Project>, repo_root: Option<&Path>, db_path: &Path) -> Result<Status> {
    let mut st = Status { projects: conn.query_row("SELECT count(*) FROM projects", [], |r| r.get(0))?, ..Status::default() };
    if let Some(p) = project {
        crate::graph::ensure_links(conn, p.id)?;
        st.project = Some(p.name.clone());
        st.project_id = Some(p.id);
        st.group = p.group.clone();
        st.notes = conn.query_row(
            "SELECT count(*) FROM notes WHERE project_id = ?1 AND deleted_at IS NULL",
            [p.id],
            |r| r.get(0),
        )?;
        st.chunks = conn.query_row("SELECT count(*) FROM chunks WHERE project_id = ?1", [p.id], |r| r.get(0))?;
        st.vectors = conn.query_row(
            "SELECT count(*) FROM vectors v WHERE v.project_id = ?1 AND v.space_id = (SELECT id FROM spaces WHERE status = 'active')",
            [p.id],
            |r| r.get(0),
        )?;
        st.links = conn.query_row("SELECT count(*) FROM links WHERE project_id = ?1", [p.id], |r| r.get(0))?;
        if let Some(root) = repo_root {
            st.dangling_files = crate::graph::dangling_files(conn, p.id, root)?;
            st.stale_instructions = stale_instructions(root);
            // A note counts as changed on disk when it is newer than its save
            // and its bytes differ: a checkout or a touch moves the mtime of an
            // identical file, and reading the few that moved is cheap.
            let mut stmt = conn.prepare(
                "SELECT path, saved_at, content_hash FROM notes WHERE project_id = ?1 AND deleted_at IS NULL",
            )?;
            for row in stmt.query_map([p.id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?)))? {
                let (path, saved_at, hash) = row?;
                if let Ok(md) = std::fs::metadata(root.join(&path)) {
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    if mtime > saved_at {
                        let same = std::fs::read(root.join(&path)).map(|b| Sha256::digest(&b).to_vec() == hash).unwrap_or(false);
                        if !same {
                            st.notes_newer_on_disk.push(path);
                        }
                    }
                }
            }
        }
    } else {
        st.notes = conn.query_row("SELECT count(*) FROM notes WHERE deleted_at IS NULL", [], |r| r.get(0))?;
        st.chunks = conn.query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))?;
        st.vectors = conn.query_row(
            "SELECT count(*) FROM vectors WHERE space_id = (SELECT id FROM spaces WHERE status = 'active')",
            [],
            |r| r.get(0),
        )?;
    }
    if let Some(s) = embed::active_space(conn)? {
        st.space = Some(format!("{} {}d {} via {}", s.def.model, s.def.dims, s.def.precision, s.def.runtime));
    }
    st.db_bytes = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
    if let Some(pid) = st.project_id {
        st.usage = usage_summary(conn, pid)?;
    }
    if let Some(v) = crate::db::meta_get(conn, "last_eval")? {
        st.last_eval = serde_json::from_str(&v).ok();
    }
    Ok(st)
}

/// Writes every live note of `project_id` back to `root/<path>`, creating
/// directories. Files whose content already matches are left alone (and not
/// counted). Returns the number written.
pub fn export(conn: &Connection, project_id: i64, root: &Path) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT path, content FROM notes WHERE project_id = ?1 AND deleted_at IS NULL ORDER BY path")?;
    let mut written = 0;
    for row in stmt.query_map([project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (path, content) = row?;
        if path.contains("..") || path.starts_with('/') {
            continue;
        }
        let full = root.join(&path);
        if std::fs::read_to_string(&full).map(|c| c == content).unwrap_or(false) {
            continue;
        }
        if let Some(dir) = full.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&full, content)?;
        written += 1;
    }
    Ok(written)
}

/// Relative path of `file` inside `root`, refusing symlinks that escape it.
pub fn relative_note_path(root: &Path, file: &Path) -> Result<String> {
    let canon = std::fs::canonicalize(file).map_err(|e| err!("{}: {e}", file.display()))?;
    let root_c = std::fs::canonicalize(root)?;
    let rel = canon
        .strip_prefix(&root_c)
        .map_err(|_| err!("{}: outside the repository {}", file.display(), root.display()))?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

/// CLAUDE.md, AGENTS.md or GEMINI.md lines that name `journal/` outside
/// `memorycastle/journal/`: a note location the layout moved away from.
pub fn stale_instructions(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for name in ["CLAUDE.md", "AGENTS.md", "GEMINI.md"] {
        let Ok(text) = std::fs::read_to_string(root.join(name)) else { continue };
        if crate::install::section_start(&text).is_some() && !text.contains(crate::app::SNIPPET_MARKER) && !text.contains(crate::app::REPO_SNIPPET_MARKER) {
            out.push(format!("{name}: memorycastle section is older than this binary's; replace the section with the output of `memcastle init`"));
        }
        for (i, line) in text.lines().enumerate() {
            let mut from = 0;
            while let Some(j) = line[from..].find("journal/") {
                let at = from + j;
                let before = &line[..at];
                if !before.ends_with("memorycastle/") && !before.ends_with("memorycastle/graph/") {
                    out.push(format!("{name}:{}: names journal/ but notes live in memorycastle/journal/", i + 1));
                    break;
                }
                from = at + 8;
            }
        }
    }
    out
}

/// Records one search or links call. `stood_for` is the byte size of every
/// distinct note and file the output drew from, read whole. Skipped on a
/// read-only store.
pub fn record_usage(conn: &Connection, project_id: i64, cmd: &str, served: usize, stood_for: usize) -> Result<()> {
    let session = current_session(conn, project_id)?;
    record_usage_at(conn, project_id, &via(cmd), served, stood_for, crate::sys::now(), session.as_deref())
}

/// As `record_usage`, at a given time and session (imported transcripts).
pub fn record_usage_at(conn: &Connection, project_id: i64, cmd: &str, served: usize, stood_for: usize, at: i64, session: Option<&str>) -> Result<()> {
    if crate::db::is_readonly(conn) {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO usage(at, project_id, cmd, served, stood_for, session) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![at, project_id, cmd, served as i64, stood_for as i64, session],
    )?;
    Ok(())
}

pub fn usage_summary(conn: &Connection, project_id: i64) -> Result<Usage> {
    let mut u = Usage::default();
    let (calls, served, stood): (i64, i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(sum(served), 0), coalesce(sum(stood_for), 0) FROM usage WHERE project_id = ?1",
        [project_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let day = crate::sys::now() - 86_400;
    let (calls_d, served_d, stood_d): (i64, i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(sum(served), 0), coalesce(sum(stood_for), 0) FROM usage WHERE project_id = ?1 AND at >= ?2",
        params![project_id, day],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let pct = |served: i64, stood: i64| if stood > 0 { (100.0 * (stood - served) as f64 / stood as f64).max(0.0) } else { 0.0 };
    u.calls = calls as u64;
    u.served = served as u64;
    u.stood_for = stood as u64;
    u.saved_pct = pct(served, stood);
    u.saved_tokens = ((stood - served).max(0) as f64 / crate::limits::BYTES_PER_TOKEN) as u64;
    u.calls_today = calls_d as u64;
    u.saved_pct_today = pct(served_d, stood_d);
    u.observed = observed(conn, project_id, None)?;
    {
        let mut stmt = conn.prepare_cached("SELECT rank, count(*) FROM reads WHERE project_id = ?1 AND rank IS NOT NULL GROUP BY rank ORDER BY rank")?;
        for row in stmt.query_map([project_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))? {
            let (rank, n) = row?;
            let rank = rank.max(0) as usize;
            if u.reads_by_rank.len() <= rank {
                u.reads_by_rank.resize(rank + 1, 0);
            }
            u.reads_by_rank[rank] = n as u64;
        }
    }
    (u.served_notes, u.opened_notes) = crate::quality::label_totals(conn, project_id)?;
    {
        let mut stmt = conn.prepare_cached("SELECT session FROM reads WHERE project_id = ?1 AND session IS NOT NULL GROUP BY session ORDER BY max(at) DESC LIMIT ?2")?;
        let recent: Vec<String> = stmt.query_map(params![project_id, crate::limits::RECENT_SESSIONS as i64], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        u.recent_sessions = recent.len();
        if !recent.is_empty() {
            u.observed_recent = observed_sessions(conn, project_id, &recent)?;
        }
    }
    u.session = current_session(conn, project_id)?;
    if let Some(sid) = &u.session {
        u.observed_session = observed(conn, project_id, Some(sid))?;
        let (calls_s, served_s, stood_s): (i64, i64, i64) = conn.query_row(
            "SELECT count(*) FILTER (WHERE cmd NOT LIKE 'gate%' AND cmd NOT LIKE 'changed%'), coalesce(sum(served), 0), coalesce(sum(stood_for), 0) FROM usage WHERE project_id = ?1 AND session = ?2",
            params![project_id, sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        u.calls_session = calls_s as u64;
        u.edits_session = conn.query_row("SELECT count(DISTINCT path) FROM edits WHERE project_id = ?1 AND session = ?2", params![project_id, sid], |r| r.get::<_, i64>(0))? as u64;
        u.saved_tokens_session = ((stood_s - served_s).max(0) as f64 / crate::limits::BYTES_PER_TOKEN) as u64;
        u.saved_pct_session = pct(served_s, stood_s);
    }
    Ok(u)
}

/// `observed` summed over several sessions.
fn observed_sessions(conn: &Connection, project_id: i64, sessions: &[String]) -> Result<Observed> {
    let mut total = Observed::default();
    let (mut g_whole, mut g_range, mut g_bytes, mut lookup, mut a_whole, mut a_range) = (0i64, 0i64, 0i64, 0i64, 0i64, 0i64);
    for s in sessions {
        let o = observed(conn, project_id, Some(s))?;
        total.reads += o.reads;
        total.read_bytes += o.read_bytes;
        total.guided_reads += o.guided_reads;
        total.guided_bytes += o.guided_bytes;
        total.guided_whole += o.guided_whole;
        total.guided_range += o.guided_range;
        total.rereads += o.rereads;
        total.avoided_reads += o.avoided_reads;
        total.avoided_whole += o.avoided_whole;
        total.avoided_range += o.avoided_range;
        total.lookup_tokens += o.lookup_tokens;
        g_whole += o.guided_whole as i64;
        g_range += o.guided_range as i64;
        g_bytes += o.guided_bytes as i64;
        a_whole += o.avoided_whole as i64;
        a_range += o.avoided_range as i64;
        lookup += (o.lookup_tokens as f64 * crate::limits::BYTES_PER_TOKEN) as i64;
    }
    let pct = |served: i64, stood: i64| if stood > 0 { (100.0 * (stood - served) as f64 / stood as f64).max(0.0) } else { 0.0 };
    let tokens = |b: i64| (b.max(0) as f64 / crate::limits::BYTES_PER_TOKEN) as u64;
    total.saved_tokens = tokens(g_whole + a_whole - g_bytes - lookup);
    total.saved_pct = pct(g_bytes + lookup, g_whole + a_whole);
    total.saved_tokens_min = tokens(g_range + a_range - g_bytes - lookup);
    total.saved_pct_min = pct(g_bytes + lookup, g_range + a_range);
    Ok(total)
}

fn observed(conn: &Connection, project_id: i64, session: Option<&str>) -> Result<Observed> {
    let sql = format!(
        "SELECT count(*), coalesce(sum(bytes), 0),
                coalesce(sum(after_span), 0),
                coalesce(sum(CASE WHEN after_span THEN bytes END), 0),
                coalesce(sum(CASE WHEN after_span THEN whole END), 0),
                coalesce(sum(CASE WHEN after_span THEN range_bytes END), 0)
         FROM reads WHERE project_id = ?1{}",
        if session.is_some() { " AND session = ?2" } else { "" }
    );
    let row = |r: &rusqlite::Row| -> rusqlite::Result<(i64, i64, i64, i64, i64, i64)> { Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)) };
    let (reads, read_bytes, g_reads, g_bytes, g_whole, g_range) = match session {
        Some(sid) => conn.query_row(&sql, params![project_id, sid], row)?,
        None => conn.query_row(&sql, [project_id], row)?,
    };
    let pct = |served: i64, stood: i64| if stood > 0 { (100.0 * (stood - served) as f64 / stood as f64).max(0.0) } else { 0.0 };
    // Net: the lookup output that produced the spans is a cost too.
    let lookup_sql = format!("SELECT coalesce(sum(served), 0) FROM usage WHERE project_id = ?1{}", if session.is_some() { " AND session = ?2" } else { "" });
    let lookup: i64 = match session {
        Some(sid) => conn.query_row(&lookup_sql, params![project_id, sid], |r| r.get(0))?,
        None => conn.query_row(&lookup_sql, [project_id], |r| r.get(0))?,
    };
    let reread_sql = format!(
        "SELECT count(*) FROM reads a WHERE a.project_id = ?1 AND a.after_span = 1{} AND EXISTS (
             SELECT 1 FROM reads b WHERE b.project_id = a.project_id AND b.path = a.path AND b.at > a.at AND b.at <= a.at + {})",
        if session.is_some() { " AND a.session = ?2" } else { "" },
        crate::limits::SPAN_FOLLOW_SECS
    );
    let rereads: i64 = match session {
        Some(sid) => conn.query_row(&reread_sql, params![project_id, sid], |r| r.get(0))?,
        None => conn.query_row(&reread_sql, [project_id], |r| r.get(0))?,
    };
    // Reads avoided: an edit of a file that a lookup printed a span for
    // within the window, with no read of that file in the same session
    // before the edit. Counted once per file per session.
    let avoided_sql = format!(
        "SELECT count(*), coalesce(sum(whole), 0), coalesce(sum(range_bytes), 0) FROM (
             SELECT e.path, max(e.whole) AS whole, max(e.range_bytes) AS range_bytes FROM edits e
             WHERE e.project_id = ?1{}
               AND EXISTS (SELECT 1 FROM served_spans s WHERE s.project_id = e.project_id AND s.path = e.path AND s.at <= e.at AND s.at >= e.at - {})
               AND NOT EXISTS (SELECT 1 FROM reads r WHERE r.project_id = e.project_id AND r.path = e.path AND r.at <= e.at AND coalesce(r.session, '') = coalesce(e.session, ''))
             GROUP BY e.path, e.session)",
        if session.is_some() { " AND e.session = ?2" } else { "" },
        crate::limits::SPAN_FOLLOW_SECS
    );
    let arow = |r: &rusqlite::Row| -> rusqlite::Result<(i64, i64, i64)> { Ok((r.get(0)?, r.get(1)?, r.get(2)?)) };
    let (a_n, a_whole, a_range) = match session {
        Some(sid) => conn.query_row(&avoided_sql, params![project_id, sid], arow)?,
        None => conn.query_row(&avoided_sql, [project_id], arow)?,
    };
    let tokens = |b: i64| (b.max(0) as f64 / crate::limits::BYTES_PER_TOKEN) as u64;
    Ok(Observed {
        reads: reads as u64,
        read_bytes: read_bytes as u64,
        guided_reads: g_reads as u64,
        guided_bytes: g_bytes as u64,
        guided_whole: g_whole as u64,
        guided_range: g_range as u64,
        saved_tokens: tokens(g_whole + a_whole - g_bytes - lookup),
        saved_pct: pct(g_bytes + lookup, g_whole + a_whole),
        saved_tokens_min: tokens(g_range + a_range - g_bytes - lookup),
        saved_pct_min: pct(g_bytes + lookup, g_range + a_range),
        lookup_tokens: tokens(lookup),
        rereads: rereads as u64,
        avoided_reads: a_n as u64,
        avoided_whole: a_whole as u64,
        avoided_range: a_range as u64,
    })
}

/// The first sentence of a note's `Decision` section, at most
/// `limits::RECALL_DECISION_CHARS`, cut at a word and ended with `...` when
/// longer; `None` when the note has no such section.
pub fn decision_sentence(conn: &Connection, note_id: i64) -> Result<Option<String>> {
    let content: Option<String> = conn.query_row("SELECT content FROM notes WHERE id = ?1", params![note_id], |r| r.get(0)).optional()?;
    let Some(content) = content else { return Ok(None) };
    for (heading, _, text) in crate::chunk::sections(&content) {
        if !heading.to_ascii_lowercase().starts_with("decision") {
            continue;
        }
        let body: String = text.lines().map(|l| l.trim().trim_start_matches("- ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        let mut end = body.find(". ").map(|i| i + 1).unwrap_or(body.len());
        let mut cut = false;
        if end > crate::limits::RECALL_DECISION_CHARS {
            end = body.char_indices().map(|(i, _)| i).take_while(|&i| i < crate::limits::RECALL_DECISION_CHARS).last().unwrap_or(0);
            // At the last space, so a word is never split ("that answ").
            end = body[..end].rfind(' ').unwrap_or(end);
            cut = true;
        }
        let mut sentence = body[..end].trim().to_string();
        if cut {
            sentence.push_str(" ...");
        }
        return Ok(if sentence.is_empty() { None } else { Some(sentence) });
    }
    Ok(None)
}

/// The session a call belongs to: `MEMORYCASTLE_SESSION` when the caller
/// knows it (every hook sets it from its own event, and the children it
/// spawns inherit it), else the session last declared for the project. The
/// declared slot alone mixed concurrent sessions in one repo: a second
/// window, or a Codex session, took over the first one's status line, edit
/// gate, owed files and stop hold.
pub fn current_session(conn: &Connection, project_id: i64) -> Result<Option<String>> {
    if let Some(s) = std::env::var("MEMORYCASTLE_SESSION").ok().filter(|s| !s.is_empty()) {
        return Ok(Some(s));
    }
    crate::db::meta_get(conn, &format!("session:{project_id}"))
}

pub fn set_session(conn: &Connection, project_id: i64, id: &str, agent: Option<&str>) -> Result<()> {
    if crate::db::is_readonly(conn) {
        return Ok(());
    }
    let same = current_session(conn, project_id)?.as_deref() == Some(id);
    crate::db::meta_set(conn, &format!("session:{project_id}"), id)?;
    if !same {
        crate::db::meta_set(conn, &format!("session_at:{project_id}"), &crate::sys::now().to_string())?;
    }
    // Per session as well, so a session other than the last declared one
    // still knows when it began and which agent it is.
    let at_key = format!("session_at:{project_id}:{id}");
    if crate::db::meta_get(conn, &at_key)?.is_none() {
        crate::db::meta_set(conn, &at_key, &crate::sys::now().to_string())?;
    }
    if let Some(a) = agent {
        crate::db::meta_set(conn, &format!("agent:{project_id}"), a)?;
        crate::db::meta_set(conn, &format!("agent:{project_id}:{id}"), a)?;
    }
    Ok(())
}

/// The label log: which notes a search printed and how much of each.
/// A lookup the edit gate ran for the agent is logged as `gate-<cmd>` (or
/// `gateblock-<cmd>` when blocking was on), not as the agent's own: the gate
/// raising the lookup count says nothing about whether anything was read,
/// and the label log's opened rate per mode is what tells context from block.
fn via(cmd: &str) -> String {
    match std::env::var("MEMORYCASTLE_VIA") {
        Ok(v) if !v.is_empty() => format!("{v}-{cmd}"),
        _ => cmd.to_string(),
    }
}

pub fn record_served_notes(conn: &Connection, project_id: i64, notes: &[i64], mode: &str) -> Result<()> {
    let mode = &via(mode);
    if crate::db::is_readonly(conn) || notes.is_empty() || std::env::var_os("MEMORYCASTLE_NO_LEDGER").is_some() {
        return Ok(());
    }
    let session = current_session(conn, project_id)?;
    let now = crate::sys::now();
    let mut ins = conn.prepare_cached("INSERT INTO served_notes(at, project_id, note_id, mode, session) VALUES (?1, ?2, ?3, ?4, ?5)")?;
    for id in notes {
        ins.execute(params![now, project_id, id, mode, session])?;
    }
    Ok(())
}

/// The agent the session-start hook declared (`session <id> --agent claude`).
pub fn current_agent(conn: &Connection, project_id: i64) -> Result<Option<String>> {
    if let Some(sid) = current_session(conn, project_id)? {
        if let Some(a) = crate::db::meta_get(conn, &format!("agent:{project_id}:{sid}"))? {
            return Ok(Some(a));
        }
    }
    crate::db::meta_get(conn, &format!("agent:{project_id}"))
}

/// When the current session began, for "did this session save a note": the
/// first read or edit recorded under its id, or when the id was declared if
/// earlier. The declared time alone moved forward on a resume (a Claude
/// restart declared another id first, then the old one again), and a note
/// saved before the restart no longer counted.
pub fn session_started(conn: &Connection, project_id: i64) -> Result<Option<i64>> {
    let slot: Option<i64> = crate::db::meta_get(conn, &format!("session_at:{project_id}"))?.and_then(|v| v.parse().ok());
    let Some(session) = current_session(conn, project_id)? else { return Ok(slot) };
    // This session's own declared time; the project's slot only when this
    // is the session it belongs to (stores from before the per-session key).
    let declared: Option<i64> = match crate::db::meta_get(conn, &format!("session_at:{project_id}:{session}"))?.and_then(|v| v.parse().ok()) {
        Some(t) => Some(t),
        None if crate::db::meta_get(conn, &format!("session:{project_id}"))?.as_deref() == Some(session.as_str()) => slot,
        None => None,
    };
    let first: Option<i64> = conn.query_row(
        "SELECT MIN(at) FROM (SELECT MIN(at) AS at FROM edits WHERE project_id = ?1 AND session = ?2 UNION ALL SELECT MIN(at) FROM reads WHERE project_id = ?1 AND session = ?2)",
        params![project_id, session],
        |r| r.get(0),
    )?;
    Ok(match (declared, first) {
        (Some(d), Some(f)) => Some(d.min(f)),
        (d, f) => d.or(f),
    })
}

/// The git user name, from the checkout's config then the global one. No
/// `git` process: both files are INI with a `[user]` section.
pub fn git_user(root: &Path) -> Option<String> {
    let mut candidates = vec![root.join(".git/config")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(Path::new(&home).join(".gitconfig"));
        candidates.push(Path::new(&home).join(".config/git/config"));
    }
    for c in candidates {
        let Ok(text) = std::fs::read_to_string(&c) else { continue };
        let mut in_user = false;
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with('[') {
                in_user = l.eq_ignore_ascii_case("[user]");
                continue;
            }
            if in_user {
                if let Some(v) = l.strip_prefix("name") {
                    let v = v.trim_start().trim_start_matches('=').trim().trim_matches('"');
                    if !v.is_empty() {
                        return Some(v.split_whitespace().next().unwrap_or(v).to_ascii_lowercase());
                    }
                }
            }
        }
    }
    None
}

/// Fills a missing `Author:` (and `Date:`, from the file name) in a note's
/// header from the session's agent and the git user, so attribution never
/// depends on the writer. Returns the new content when something was added.
pub fn fill_header(content: &str, rel_path: &str, user: Option<&str>, agent: Option<&str>) -> Option<String> {
    let has_author = chunk::author(content).is_some();
    let has_date = chunk::dated(content).is_some();
    let name = Path::new(rel_path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let date_from_name = name.get(..10).filter(|d| crate::render::parse_date(d).is_some());
    if has_author && (has_date || date_from_name.is_none()) {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if !has_author {
        if let (Some(u), Some(a)) = (user, agent) {
            parts.push(format!("Author: {u}+{a}"));
        }
    }
    if !has_date {
        if let Some(d) = date_from_name {
            parts.push(format!("Date: {d}"));
        }
    }
    if parts.is_empty() {
        return None;
    }
    let added = parts.join(" | ");
    let mut out = String::with_capacity(content.len() + added.len() + 1);
    let mut lines = content.lines();
    let mut done = false;
    // After the title line; or, when only a date is missing and an Author
    // line exists, appended to that line.
    for line in lines.by_ref() {
        if !done && !has_author && line.starts_with("# ") {
            out.push_str(line);
            out.push('\n');
            out.push_str(&added);
            out.push('\n');
            done = true;
            continue;
        }
        if !done && has_author && line.contains("Author:") {
            out.push_str(line.trim_end());
            out.push_str(" | ");
            out.push_str(&added);
            out.push('\n');
            done = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !done {
        return None;
    }
    Some(out)
}

/// Files the agent edited in this session (outside `memorycastle/`) that
/// the note's `Files:` line does not name: the ledger knows what the task
/// touched better than the writer remembers.
/// The code files this session edited since the previous note (not
/// `rel_path`), and that note's save time: a long session writes several
/// notes, and each covers its own task, not the whole session.
pub fn edited_for_note(conn: &Connection, project_id: i64, rel_path: &str) -> Result<(i64, Vec<String>)> {
    let Some(session) = current_session(conn, project_id)? else { return Ok((0, Vec::new())) };
    let since: i64 = conn
        .query_row(
            "SELECT MAX(saved_at) FROM notes WHERE project_id = ?1 AND path != ?2 AND deleted_at IS NULL",
            params![project_id, rel_path],
            |r| r.get::<_, Option<i64>>(0),
        )?
        .unwrap_or(0);
    let mut stmt = conn.prepare_cached("SELECT DISTINCT path FROM edits WHERE project_id = ?1 AND session = ?2 AND at >= ?3 ORDER BY path")?;
    let edited: Vec<String> = stmt.query_map(params![project_id, session, since], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok((since, edited.into_iter().filter(|e| !e.starts_with("memorycastle/")).collect()))
}

pub fn files_missing_from_note(conn: &Connection, project_id: i64, rel_path: &str, content: &str) -> Result<Vec<String>> {
    let declared: Vec<String> = crate::graph::extract("", content).into_iter().filter(|(k, _)| *k == crate::graph::Kind::File).map(|(_, v)| v).collect();
    let (_, edited) = edited_for_note(conn, project_id, rel_path)?;
    Ok(edited
        .into_iter()
        .filter(|e| !declared.iter().any(|d| e == d || e.ends_with(&format!("/{d}")) || d.ends_with(&format!("/{e}")) || Path::new(e).file_name() == Path::new(d).file_name()))
        .collect())
}

/// Remembers that a span was printed for `path`, so a read of it soon after
/// counts as guided.
pub fn record_span(conn: &Connection, project_id: i64, path: &str, start: u32, end: u32, rank: u32) -> Result<()> {
    if crate::db::is_readonly(conn) {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO served_spans(at, project_id, path, start, end, rank) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![crate::sys::now(), project_id, path, start as i64, end as i64, rank as i64],
    )?;
    Ok(())
}

/// Which served span (if any) a read of `path` lines `a..=b` followed: the
/// most recent within the window whose range overlaps. `rank` 0 means a
/// `links`/`code:` span, 1.. the position in `read first:`.
fn guiding_rank(conn: &Connection, project_id: i64, path: &str, a: u32, b: u32) -> Result<Option<u32>> {
    let since = crate::sys::now() - crate::limits::SPAN_FOLLOW_SECS;
    Ok(conn
        .query_row(
            "SELECT rank FROM served_spans WHERE project_id = ?1 AND path = ?2 AND at >= ?3
               AND (start = 0 OR end = 0 OR (start <= ?5 AND end >= ?4))
             ORDER BY at DESC LIMIT 1",
            params![project_id, path, since, a as i64, b as i64],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|r| r as u32))
}

/// Records a file read reported by an agent hook. `path` is relative to the
/// repo root; `bytes` is what the tool returned. Returns whether it was guided.
pub fn record_read(conn: &Connection, project_id: i64, root: &Path, path: &str, bytes: usize) -> Result<bool> {
    record_read_lines(conn, project_id, root, path, bytes, None, None)
}

/// As `record_read`, with the line range when known (a `sed -n` or a Read
/// with offset) and, for imported logs, the guided decision already made.
pub fn record_read_lines(conn: &Connection, project_id: i64, root: &Path, path: &str, bytes: usize, lines_read: Option<(u32, u32)>, forced: Option<Option<u32>>) -> Result<bool> {
    let session = current_session(conn, project_id)?;
    record_read_at(conn, project_id, root, path, bytes, lines_read, forced, crate::sys::now(), session.as_deref())
}

/// As `record_read_lines`, at a given time and session: for transcripts
/// imported after the fact, so the ledger keeps their original timeline.
#[allow(clippy::too_many_arguments)]
pub fn record_read_at(conn: &Connection, project_id: i64, root: &Path, path: &str, bytes: usize, lines_read: Option<(u32, u32)>, forced: Option<Option<u32>>, at: i64, session: Option<&str>) -> Result<bool> {
    if crate::db::is_readonly(conn) {
        return Ok(false);
    }
    let size = std::fs::metadata(root.join(path)).map(|m| m.len() as usize).unwrap_or(bytes).max(bytes);
    let lines = std::fs::read(root.join(path)).map(|b| b.iter().filter(|&&c| c == b'\n').count().max(1)).unwrap_or(1);
    // Floor baseline: a grep-then-range read of RANGE_READ_LINES lines at the
    // file's own average line length, never less than what was read.
    let range = (size * crate::limits::RANGE_READ_LINES.min(lines) / lines).max(bytes);
    // Ceiling: what an agent reads of this file without a span, which is the
    // whole file only up to WHOLE_READ_LINES; past that, the head it gets
    // plus one range it greps to. Never less than what was read.
    let whole = if lines <= crate::limits::WHOLE_READ_LINES { size } else { (size * crate::limits::WHOLE_READ_LINES / lines + range).min(size) }.max(bytes);
    let (a, b) = lines_read.unwrap_or((1, u32::MAX));
    let rank = match forced {
        Some(r) => r,
        None => guiding_rank(conn, project_id, path, a, b)?,
    };
    let guided = rank.is_some();
    conn.execute(
        "INSERT INTO reads(at, project_id, path, bytes, whole, range_bytes, after_span, session, rank, start, end) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![at, project_id, path, bytes as i64, whole as i64, range as i64, guided as i64, session, rank.map(|r| r as i64), lines_read.map_or(0, |l| l.0), lines_read.map_or(0, |l| l.1)],
    )?;
    Ok(guided)
}

/// Records an edit reported by the agent hook. Sizes are taken now, before
/// the edit lands, so they describe what a read would have cost.
pub fn record_edit(conn: &Connection, project_id: i64, root: &Path, path: &str) -> Result<()> {
    record_edit_at(conn, project_id, root, path, crate::sys::now())
}

/// As `record_edit`, at a given time: the after-command hook passes the
/// file's mtime, since the hook runs after the whole command and a note
/// saved in that same command was otherwise followed by an "edit" and owed.
pub fn record_edit_at(conn: &Connection, project_id: i64, root: &Path, path: &str, at: i64) -> Result<()> {
    if crate::db::is_readonly(conn) {
        return Ok(());
    }
    let size = std::fs::metadata(root.join(path)).map(|m| m.len() as i64).unwrap_or(0);
    let lines = std::fs::read(root.join(path)).map(|b| b.iter().filter(|&&c| c == b'\n').count().max(1)).unwrap_or(1);
    let range = size * crate::limits::RANGE_READ_LINES.min(lines) as i64 / lines as i64;
    let whole = if lines <= crate::limits::WHOLE_READ_LINES { size } else { (size * crate::limits::WHOLE_READ_LINES as i64 / lines as i64 + range).min(size) };
    let session = current_session(conn, project_id)?;
    conn.execute(
        "INSERT INTO edits(at, project_id, path, whole, range_bytes, session) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![at, project_id, path, whole, range, session],
    )?;
    Ok(())
}

/// The read guard: spans served for `path` within the window that no read
/// has followed and that the guard has not pointed at before. Marks them
/// guarded, so the agent is redirected once and never blocked twice.
pub fn unread_spans(conn: &Connection, project_id: i64, path: &str) -> Result<Vec<(u32, u32)>> {
    if crate::db::is_readonly(conn) {
        return Ok(Vec::new());
    }
    let since = crate::sys::now() - crate::limits::SPAN_FOLLOW_SECS;
    let mut stmt = conn.prepare(
        "SELECT rowid, start, end FROM served_spans s WHERE project_id = ?1 AND path = ?2 AND at >= ?3 AND guarded = 0
           AND start > 0 AND end >= start
           AND NOT EXISTS (SELECT 1 FROM reads r WHERE r.project_id = s.project_id AND r.path = s.path AND r.at >= s.at)
         ORDER BY at DESC, start LIMIT ?4",
    )?;
    let rows: Vec<(i64, u32, u32)> = stmt
        .query_map(params![project_id, path, since, crate::limits::READ_SPANS as i64], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u32, r.get::<_, i64>(2)? as u32)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (id, a, b) in rows {
        conn.execute("UPDATE served_spans SET guarded = 1 WHERE rowid = ?1", [id])?;
        if !out.contains(&(a, b)) {
            out.push((a, b));
        }
    }
    out.sort_unstable();
    Ok(out)
}

/// The sections of a note that name any of `names` (a whole word, so `run`
/// is not `running`), as `(first line, last line, section)`: the lines to
/// read instead of the whole note. The header lines are left out: `Files:`
/// names every file the note is about.
pub fn note_lines_naming(conn: &Connection, note_id: i64, names: &[&str]) -> Result<Vec<(u32, u32, String)>> {
    let content: Option<String> = conn.query_row("SELECT content FROM notes WHERE id = ?1", [note_id], |r| r.get(0)).optional()?;
    let Some(content) = content else { return Ok(Vec::new()) };
    let lines: Vec<&str> = content.lines().collect();
    let names: Vec<&str> = names.iter().copied().filter(|n| !n.is_empty()).collect();
    let mut out = Vec::new();
    for (name, at, _) in crate::chunk::sections(&content) {
        // Body: from after the heading to the line before the next heading.
        let first = at as usize; // 0-based index of the line after the heading
        let mut last = first;
        while last < lines.len() && !(last > first && lines[last].starts_with("## ")) {
            last += 1;
        }
        let body = &lines[first.min(lines.len())..last];
        if !body.iter().any(|l| names.iter().any(|n| names_word(l, n))) {
            continue;
        }
        let mut end = last;
        while end > first && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let mut start = first;
        while start < end && lines[start].trim().is_empty() {
            start += 1;
        }
        if start < end {
            out.push((start as u32 + 1, end as u32, name));
        }
    }
    Ok(out)
}

/// The printed `lines` field and the ranges it names.
pub type LinesField = (String, Vec<(u32, u32)>);

/// The `links` line under a note: `   lines      12-14 Decision, 18 Rejected`,
/// the sections that name `target` (or the spelling `via` the note used),
/// matched by the last segment (`store::save` by `save`, `src/a.rs` by `a.rs`).
pub fn lines_field(conn: &Connection, note_id: i64, target: &str, via: &str) -> Result<Option<LinesField>> {
    fn bare(s: &str) -> &str {
        s.rsplit(['/', ':']).next().unwrap_or(s)
    }
    let names = [bare(target), bare(via)];
    let found = note_lines_naming(conn, note_id, &names)?;
    if found.is_empty() {
        return Ok(None);
    }
    let parts: Vec<String> = found.iter().map(|(a, b, s)| if b > a { format!("{a}-{b} {s}") } else { format!("{a} {s}") }).collect();
    Ok(Some((format!("   lines      {}\n", parts.join(", ")), found.iter().map(|(a, b, _)| (*a, *b)).collect())))
}

/// True when `line` holds `name` with no identifier character on either side.
fn names_word(line: &str, name: &str) -> bool {
    let is_id = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(name).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let after = line[i + name.len()..].chars().next();
        !before.is_some_and(is_id) && !after.is_some_and(is_id)
    })
}

/// Records the printed line ranges of a long note as served spans (rank 0),
/// so the read guard can point a whole read of it at them.
pub fn record_note_spans(conn: &Connection, project_id: i64, note_id: i64, path: &str, ranges: &[(u32, u32)]) -> Result<()> {
    if ranges.is_empty() || note_bytes(conn, note_id)? < crate::limits::NOTE_GUARD_MIN_BYTES {
        return Ok(());
    }
    for (a, b) in ranges {
        record_span(conn, project_id, path, *a, *b, 0)?;
    }
    Ok(())
}

/// Byte size of a note's stored text.
pub fn note_bytes(conn: &Connection, note_id: i64) -> Result<usize> {
    Ok(conn.query_row("SELECT length(content) FROM notes WHERE id = ?1", [note_id], |r| r.get::<_, i64>(0))? as usize)
}

/// Known section names, in the order the snippet lists them.
pub const SECTION_NAMES: &[&str] = &["Context", "Decision", "Rejected", "Measured", "Open"];

/// Replaces the note's typed section rows.
pub fn write_sections(conn: &Connection, note_id: i64, content: &str) -> Result<()> {
    conn.execute("DELETE FROM sections WHERE note_id = ?1", [note_id])?;
    let mut ins = conn.prepare_cached("INSERT INTO sections(note_id, ordinal, name, line, body) VALUES (?1, ?2, ?3, ?4, ?5)")?;
    for (i, (name, line, body)) in chunk::sections(content).into_iter().enumerate() {
        // canonical capitalisation for the known names
        let canon = canonical_section(&name).map(str::to_string).unwrap_or(name);
        ins.execute(params![note_id, i as i64, canon, line as i64, body])?;
    }
    Ok(())
}

/// `Measured (correction, same day)` is a `Measured` section: a known name,
/// then anything. Appended sections keep the old text untouched, as the
/// rules require, and still print.
pub fn canonical_section(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    SECTION_NAMES.iter().copied().find(|n| lower.starts_with(&n.to_ascii_lowercase()))
}

/// A note's section bodies by canonical name, in note order, joined by a
/// blank line; `None` when the note has no such section.
pub fn section_body(conn: &Connection, note_id: i64, name: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT body FROM sections WHERE note_id = ?1 AND name = ?2 ORDER BY ordinal")?;
    let bodies: Vec<String> = stmt.query_map(params![note_id, name], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(if bodies.is_empty() { None } else { Some(bodies.join("\n\n")) })
}

/// A note's section bodies by canonical name, each line with its line
/// number in the note file, in note order.
pub fn section_lines(conn: &Connection, note_id: i64, name: &str) -> Result<Vec<(u32, String)>> {
    let content: String = conn.query_row("SELECT content FROM notes WHERE id = ?1", [note_id], |r| r.get(0))?;
    let lines: Vec<&str> = content.lines().collect();
    let mut stmt = conn.prepare_cached("SELECT line, body FROM sections WHERE note_id = ?1 AND name = ?2 ORDER BY ordinal")?;
    let mut out = Vec::new();
    for row in stmt.query_map(params![note_id, name], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (heading, body) = row?;
        // The body is trimmed: skip the blank lines under the heading.
        let mut at = heading as usize;
        while at < lines.len() && lines[at].trim().is_empty() {
            at += 1;
        }
        for (i, l) in body.lines().enumerate() {
            out.push(((at + i + 1) as u32, l.to_string()));
        }
    }
    Ok(out)
}

/// Fills `sections` for every note once after the v14 migration.
pub fn ensure_sections(conn: &Connection) -> Result<()> {
    if crate::db::meta_get(conn, "sections_rebuild")?.is_none() || crate::db::is_readonly(conn) {
        return Ok(());
    }
    let notes: Vec<(i64, String)> = conn
        .prepare("SELECT id, content FROM notes WHERE deleted_at IS NULL")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, content) in notes {
        write_sections(conn, id, &content)?;
        conn.execute("UPDATE notes SET dated = ?2 WHERE id = ?1", params![id, chunk::dated(&content)])?;
    }
    conn.execute("DELETE FROM meta WHERE key = 'sections_rebuild'", [])?;
    Ok(())
}

/// What is wrong with a note's shape, as one line each. Warnings, never a
/// refusal: a thin note beats no note, and the author sees how to fix it.
pub fn lint(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let words = content.split_whitespace().count();
    if words > crate::limits::NOTE_MAX_WORDS {
        // A living tracker (no Date: header) grows by closed entries; those
        // are what to move out. A dated note is one task and should split.
        let closed = content.lines().filter(|l| l.trim_start().starts_with("- [x]")).count();
        if chunk::dated(content).is_none() && closed > 0 {
            out.push(format!("{words} words, {closed} closed `[x]` entries; move the closed ones to an archive note (`<name>-archive.md`) so open items rank first"));
        } else {
            out.push(format!("{words} words; split it into notes of one decision each so search returns the answer, not a document"));
        }
    }
    if !chunk::has_files_line(content) {
        out.push("header has no `Files:` line (the codebase files this note is about)".into());
    }
    if chunk::author(content).is_none() {
        out.push("header has no `Author:` line (user+agent, e.g. abhi+claude)".into());
    }
    let secs = chunk::sections(content);
    let get = |n: &str| secs.iter().find(|(name, _, _)| canonical_section(name) == Some(n)).map(|(_, _, b)| b.as_str());
    match get("Decision") {
        None => out.push("no `## Decision` section; search --sections has nothing to print for it".into()),
        Some(b) if b.split_whitespace().count() < crate::limits::DECISION_MIN_WORDS => {
            out.push(format!("`## Decision` is under {} words; say what was chosen and why", crate::limits::DECISION_MIN_WORDS))
        }
        Some(b) => {
            let words = b.split_whitespace().count();
            let bullets = b.lines().filter(|l| l.trim_start().starts_with("- ")).count();
            if words > crate::limits::DECISION_MAX_WORDS || bullets > crate::limits::DECISION_MAX_BULLETS {
                out.push(format!(
                    "`## Decision` is {words} words in {bullets} bullets; one decision per note (under {} words), the why goes in Rejected: split it",
                    crate::limits::DECISION_MAX_WORDS
                ));
            }
        }
    }
    if chunk::title_of(content, "") == crate::draft::PLACEHOLDER_TITLE {
        out.push("title is the draft placeholder; state the decision in one line".into());
    }
    {
        let title = chunk::title_of(content, "").to_ascii_lowercase();
        let task_words: Vec<&str> = title.split(|c: char| !c.is_alphanumeric()).filter(|w| crate::limits::TASK_TITLE_WORDS.contains(w)).collect();
        if !task_words.is_empty() {
            out.push(format!("title names a task ({}); the title is embedded with every chunk, so state the decision (\"Choose int8 over q4\")", task_words.join(", ")));
        }
    }
    if get("Context").is_none() {
        out.push("no `## Context` section; the next reader will not know why this task existed".into());
    }
    if !content.contains('`') {
        out.push("no backticked file or symbol; name them (`src/store.rs`, `embed_missing`) so `links` can find this note".into());
    }
    let has_number = content.contains(" ms") || content.contains(" MB") || content.contains('%');
    if get("Measured").is_none() && has_number {
        out.push("numbers in the text but no `## Measured` section; put them there so a numeric question finds them".into());
    }
    for (name, line, _) in &secs {
        if canonical_section(name).is_none() {
            out.push(format!("line {line}: section `## {name}` is not one of Context, Decision, Rejected, Measured, Open; it is indexed but not printed by --sections"));
        }
    }
    out
}

#[cfg(test)]
mod header_tests {
    #[test]
    fn fills_author_and_date_after_title() {
        let out = super::fill_header("# T\nFiles: `a.rs`\n\n## Decision\nx\n", "memorycastle/journal/2026-10-19-t.md", Some("abhi"), Some("claude")).unwrap();
        assert!(out.starts_with("# T\nAuthor: abhi+claude | Date: 2026-10-19\nFiles:"), "{out}");
        assert!(super::fill_header(&out, "memorycastle/journal/2026-10-19-t.md", Some("abhi"), Some("claude")).is_none());
    }
    #[test]
    fn appends_date_to_an_author_line() {
        let out = super::fill_header("# T\nAuthor: abhi+codex\n## Decision\nx\n", "j/2026-10-01-t.md", None, None).unwrap();
        assert!(out.contains("Author: abhi+codex | Date: 2026-10-01\n"), "{out}");
    }
    #[test]
    fn tracker_without_date_in_name_gets_author_only() {
        let out = super::fill_header("# Bugs\n## Decision\nx\n", "j/app-bugs.md", Some("abhi"), Some("claude")).unwrap();
        assert!(out.starts_with("# Bugs\nAuthor: abhi+claude\n"), "{out}");
        assert!(super::fill_header("# Bugs\n## Decision\nx\n", "j/app-bugs.md", None, Some("claude")).is_none());
    }
}

#[cfg(test)]
mod lint_tests {
    #[test]
    fn long_or_bulleted_decision_is_told_to_split() {
        let long = format!("# Choose x\nAuthor: a+b | Date: 2026-10-19\nFiles: `src/a.rs`\n## Context\nx\n## Decision\n{}", "word ".repeat(130));
        let w = super::lint(&long);
        assert!(w.iter().any(|m| m.contains("split it")), "{w:?}");
        let bullets = "# Choose x\nAuthor: a+b | Date: 2026-10-19\nFiles: `src/a.rs`\n## Context\nx\n## Decision\n- one two three\n- four five six\n- seven eight nine\n- ten eleven twelve\n";
        let w = super::lint(bullets);
        assert!(w.iter().any(|m| m.contains("4 bullets")), "{w:?}");
        let fine = "# Choose x\nAuthor: a+b | Date: 2026-10-19\nFiles: `src/a.rs`\n## Context\nx\n## Decision\nOne two three four five six seven eight nine ten.\n";
        assert!(!super::lint(fine).iter().any(|m| m.contains("split it") || m.contains("names a task")));
    }

    #[test]
    fn oversized_tracker_is_told_to_archive_closed_entries() {
        let filler = "word ".repeat(1600);
        let tracker = format!("# Bugs\nAuthor: abhi+claude\nFiles: `src/a.rs`\n## Context\nx\n## Decision\none two three four five six seven eight nine\n- [x] done\n- [ ] open\n{filler}");
        let w = super::lint(&tracker);
        assert!(w.iter().any(|m| m.contains("closed `[x]` entries")), "{w:?}");
        assert!(super::lint("# Expert review batch\nAuthor: a+b | Date: 2026-10-19\nFiles: `a.rs`\n## Context\nx\n## Decision\nOne two three four five six seven eight nine.\n").iter().any(|m| m.contains("title names a task (review, batch)")));
        let dated = tracker.replace("Author: abhi+claude", "Author: abhi+claude | Date: 2026-10-19");
        let w = super::lint(&dated);
        assert!(w.iter().any(|m| m.contains("split it into notes")), "{w:?}");
    }
}
