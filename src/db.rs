//! Opening the one store and keeping its schema current.
//!
//! Every command opens the file, works, and exits. No connection pool, no
//! background thread. `PRAGMA foreign_keys` makes `chunks -> vectors` cascade so
//! forgetting a note is one `DELETE`; the FTS triggers keep `chunks_fts` in step.

use crate::{sys, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 32;

pub fn db_path() -> PathBuf {
    sys::data_dir().join("memory.db")
}

/// Open (creating if needed) the store at the default location.
pub fn open() -> Result<Connection> {
    open_at(&db_path())
}

pub fn open_at(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    use rusqlite::OpenFlags;
    let rw = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let writable: Result<Connection> = (|| {
        let conn = Connection::open_with_flags(path, rw)?;
        configure(&conn)?;
        migrate(&conn)?;
        Ok(conn)
    })();
    match writable {
        Ok(conn) => Ok(conn),
        Err(e) if path.exists() => {
            // Sandboxed agents (Codex read-only, for one) may read the store
            // but not write beside it. `immutable` skips the WAL/shm files and
            // locking; every write on the read path checks `is_readonly`.
            let uri = format!("file:{}?immutable=1", path.display());
            let ro = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            let conn = Connection::open_with_flags(uri, ro).map_err(|ro_err| crate::err!("{e} (read-only fallback: {ro_err})"))?;
            let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if version != SCHEMA_VERSION {
                return Err(crate::err!("store is read-only here and needs migration (v{version} to v{SCHEMA_VERSION}); run `memorycastle status` outside the sandbox once"));
            }
            Ok(conn)
        }
        Err(e) => Err(e),
    }
}

/// True when the store was opened without write access.
pub fn is_readonly(conn: &Connection) -> bool {
    conn.is_readonly(rusqlite::MAIN_DB).unwrap_or(false)
}

/// An in-memory store, for tests and the stub harness.
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    // The page cache is bounded: 2 MB is plenty for a store under 30 MB.
    conn.pragma_update(None, "cache_size", -2048)?;
    Ok(())
}

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    // Several processes open the store at once at session start (sync,
    // serve, status, hooks). Unlocked, one re-ran v28's table rebuild after
    // another had finished v29, leaving v29 without its column. The write
    // lock is taken first and the version read again under it.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).map_err(Into::into).and_then(|v| if v >= SCHEMA_VERSION { Ok(()) } else { migrate_from(conn, v) });
    match result {
        Ok(()) => conn.execute_batch("COMMIT").map_err(Into::into),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn migrate_from(conn: &Connection, version: i64) -> Result<()> {
    if version == 0 {
        conn.execute_batch(SCHEMA_V1)?;
    }
    if version < 2 {
        // v2: the path of the newer note that replaced this one (derived).
        conn.execute_batch("ALTER TABLE notes ADD COLUMN superseded_by TEXT")?;
    }
    if version < 3 {
        // v3: what this note declares it replaces (`Supersedes: <path>` in its header).
        conn.execute_batch("ALTER TABLE notes ADD COLUMN supersedes TEXT")?;
    }
    if version < 4 {
        // v4: vectors as a rowid table. As WITHOUT ROWID, every 1 KB blob
        // spilled to its own overflow page: 16 MB on disk for 3.5 MB of
        // vectors, and a scan that walked a page per row.
        conn.execute_batch(
            "CREATE TABLE vectors_v4 (
                 chunk_id    INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
                 space_id    INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
                 project_id  INTEGER NOT NULL,
                 vec         BLOB NOT NULL,
                 PRIMARY KEY (space_id, chunk_id)
             );
             INSERT INTO vectors_v4(chunk_id, space_id, project_id, vec)
                 SELECT chunk_id, space_id, project_id, vec FROM vectors;
             DROP TABLE vectors;
             ALTER TABLE vectors_v4 RENAME TO vectors;
             CREATE INDEX vectors_scope ON vectors(space_id, project_id);",
        )?;
    }
    if version < 5 {
        // v5: interlinkages, one row per (note, kind, target); filled lazily
        // for notes saved before this version (graph::ensure_links).
        conn.execute_batch(
            "CREATE TABLE links (
                 note_id    INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                 project_id INTEGER NOT NULL,
                 kind       INTEGER NOT NULL,
                 target     TEXT NOT NULL,
                 PRIMARY KEY (note_id, kind, target)
             ) WITHOUT ROWID;
             CREATE INDEX links_target ON links(project_id, kind, target);",
        )?;
    }
    if version < 6 {
        // v6: bit-packed sign vectors, one contiguous blob per (space, project),
        // the prefilter for the vector scan. Derived; dropped on any vector change.
        conn.execute_batch(
            "CREATE TABLE vector_bits (
                 space_id   INTEGER NOT NULL,
                 project_id INTEGER NOT NULL,
                 n          INTEGER NOT NULL,
                 ids        BLOB NOT NULL,
                 bits       BLOB NOT NULL,
                 PRIMARY KEY (space_id, project_id)
             ) WITHOUT ROWID;",
        )?;
    }
    if version < 7 {
        // v7: the checkout's own graph (files, definitions, imports, mentions),
        // keyed by content hash so re-indexing touches only changed files.
        conn.execute_batch(
            "CREATE TABLE code_files (
                 project_id INTEGER NOT NULL,
                 path       TEXT NOT NULL,
                 hash       BLOB NOT NULL,
                 lang       TEXT NOT NULL,
                 PRIMARY KEY (project_id, path)
             ) WITHOUT ROWID;
             CREATE TABLE code_defs (
                 project_id INTEGER NOT NULL,
                 path       TEXT NOT NULL,
                 name       TEXT NOT NULL,
                 kind       TEXT NOT NULL,
                 line       INTEGER NOT NULL,
                 FOREIGN KEY (project_id, path) REFERENCES code_files(project_id, path) ON DELETE CASCADE
             );
             CREATE INDEX code_defs_name ON code_defs(project_id, name);
             CREATE INDEX code_defs_path ON code_defs(project_id, path);
             CREATE TABLE code_imports (
                 project_id INTEGER NOT NULL,
                 src        TEXT NOT NULL,
                 dst        TEXT NOT NULL,
                 PRIMARY KEY (project_id, src, dst),
                 FOREIGN KEY (project_id, src) REFERENCES code_files(project_id, path) ON DELETE CASCADE
             ) WITHOUT ROWID;
             CREATE TABLE code_mentions (
                 project_id INTEGER NOT NULL,
                 src        TEXT NOT NULL,
                 name       TEXT NOT NULL,
                 n          INTEGER NOT NULL,
                 PRIMARY KEY (project_id, src, name),
                 FOREIGN KEY (project_id, src) REFERENCES code_files(project_id, path) ON DELETE CASCADE
             ) WITHOUT ROWID;
             CREATE INDEX code_mentions_name ON code_mentions(project_id, name);",
        )?;
    }
    if version < 8 {
        // v8: chunks know their line in the note, for `note.md:line` citations.
        conn.execute_batch("ALTER TABLE chunks ADD COLUMN line INTEGER NOT NULL DEFAULT 0;")?;
    }
    if version < 9 {
        // v9: a definition's end line and its anchor (the definition line's
        // text), so a span can be re-found after the file changes.
        conn.execute_batch(
            "ALTER TABLE code_defs ADD COLUMN end_line INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE code_defs ADD COLUMN anchor TEXT NOT NULL DEFAULT '';",
        )?;
    }
    if version < 10 {
        // v10: what each search or links call printed, and the size of what it
        // stood in for (the whole notes and files it drew from). Savings are
        // reported as a percentage of bytes, so no tokenizer is involved.
        conn.execute_batch(
            "CREATE TABLE usage (
                 at           INTEGER NOT NULL,
                 project_id   INTEGER NOT NULL,
                 cmd          TEXT NOT NULL,
                 served       INTEGER NOT NULL,
                 stood_for    INTEGER NOT NULL
             );
             CREATE INDEX usage_project ON usage(project_id, at);",
        )?;
    }
    if version < 11 {
        // v11: observed reads. `served_spans` is every path a search or links
        // call printed a span for; `reads` is every file read the agent's
        // hooks reported, with the bytes it returned and the file's size.
        conn.execute_batch(
            "CREATE TABLE served_spans (
                 at         INTEGER NOT NULL,
                 project_id INTEGER NOT NULL,
                 path       TEXT NOT NULL
             );
             CREATE INDEX served_spans_path ON served_spans(project_id, path, at);
             CREATE TABLE reads (
                 at         INTEGER NOT NULL,
                 project_id INTEGER NOT NULL,
                 path       TEXT NOT NULL,
                 bytes      INTEGER NOT NULL,
                 whole      INTEGER NOT NULL,
                 after_span INTEGER NOT NULL
             );
             CREATE INDEX reads_project ON reads(project_id, at);",
        )?;
    }
    if version < 12 {
        // v12: floor baseline per read (a range read instead of the whole file).
        conn.execute_batch("ALTER TABLE reads ADD COLUMN range_bytes INTEGER NOT NULL DEFAULT 0; UPDATE reads SET range_bytes = whole;")?;
    }
    if version < 13 {
        // v13: the agent session each read and call happened in, so savings
        // can be shown for this session beside all time.
        conn.execute_batch("ALTER TABLE reads ADD COLUMN session TEXT; ALTER TABLE usage ADD COLUMN session TEXT;")?;
    }
    if version < 14 {
        // v14: the note's tree, typed: one row per `## ` section. Markdown
        // stays the file format; this is what `--sections`, the lint and
        // the JSON output read. Rebuilt on every save of the note.
        conn.execute_batch(
            "CREATE TABLE sections (
                 note_id    INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                 ordinal    INTEGER NOT NULL,
                 name       TEXT NOT NULL,
                 line       INTEGER NOT NULL,
                 body       TEXT NOT NULL,
                 PRIMARY KEY (note_id, ordinal)
             ) WITHOUT ROWID;
             CREATE INDEX sections_name ON sections(name);",
        )?;
        crate::db::meta_set(conn, "sections_rebuild", "1")?;
    }
    if version < 15 {
        // v15: served spans carry their range and rank; reads record which
        // rank they followed. Old rows: range 0 (any), rank 0.
        conn.execute_batch(
            "ALTER TABLE served_spans ADD COLUMN start INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE served_spans ADD COLUMN end INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE served_spans ADD COLUMN rank INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE reads ADD COLUMN rank INTEGER;
             UPDATE reads SET rank = 0 WHERE after_span = 1;",
        )?;
    }
    if version < 16 {
        // v16: size and mtime per indexed file, so a refresh before every
        // `links` and `search` stats files instead of hashing them.
        conn.execute_batch("ALTER TABLE code_files ADD COLUMN size INTEGER NOT NULL DEFAULT -1; ALTER TABLE code_files ADD COLUMN mtime INTEGER NOT NULL DEFAULT -1;")?;
    }
    if version < 17 {
        // v17: the note's own `Date:` header, so migrated notes show when
        // they were written, not when they were saved.
        conn.execute_batch("ALTER TABLE notes ADD COLUMN dated INTEGER;")?;
        crate::db::meta_set(conn, "sections_rebuild", "1")?;
    }
    if version < 18 {
        // v18: `guarded` marks a served span the read guard has already
        // pointed at once; `edits` records files the agent edited, so an
        // edit with no read after a lookup can count as a read avoided.
        conn.execute_batch(
            "ALTER TABLE served_spans ADD COLUMN guarded INTEGER NOT NULL DEFAULT 0;
             CREATE TABLE IF NOT EXISTS edits (
                 at         INTEGER NOT NULL,
                 project_id INTEGER NOT NULL,
                 path       TEXT NOT NULL,
                 whole      INTEGER NOT NULL,
                 range_bytes INTEGER NOT NULL,
                 session    TEXT
             );
             CREATE INDEX IF NOT EXISTS edits_project ON edits(project_id, at);",
        )?;
    }
    if version < 19 {
        // v19: the label log. Every note a search printed, with how much of
        // it (brief line, snippet, sections), so `quality` can say which
        // notes are served and never opened.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS served_notes (
                 at         INTEGER NOT NULL,
                 project_id INTEGER NOT NULL,
                 note_id    INTEGER NOT NULL,
                 mode       TEXT NOT NULL,
                 session    TEXT
             );
             CREATE INDEX IF NOT EXISTS served_notes_note ON served_notes(project_id, note_id, at);",
        )?;
    }
    if version < 20 {
        // What the code a note names looked like when the note was saved:
        // one row per (note, symbol, file) with the definition body's hash,
        // so a search can say "code changed since" without guessing.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS note_code (
                 note_id    INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                 project_id INTEGER NOT NULL,
                 name       TEXT NOT NULL,
                 path       TEXT NOT NULL,
                 body_hash  BLOB NOT NULL,
                 PRIMARY KEY (note_id, name, path)
             ) WITHOUT ROWID;",
        )?;
    }
    if version < 21 {
        // v21: function-to-function edges from the code index. One row per
        // (file, caller, callee) with the first call line, found by the call
        // shape `name(` inside a definition's span. Name-level, no parser.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS code_calls (
                 project_id INTEGER NOT NULL,
                 src        TEXT NOT NULL,
                 caller     TEXT NOT NULL,
                 callee     TEXT NOT NULL,
                 line       INTEGER NOT NULL,
                 PRIMARY KEY (project_id, src, caller, callee),
                 FOREIGN KEY (project_id, src) REFERENCES code_files(project_id, path) ON DELETE CASCADE
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS code_calls_callee ON code_calls(project_id, callee);",
        )?;
    }
    if version < 22 {
        // v22: how each call is written (`code::qualifier`), so an edge gets
        // a confidence for the definition it is printed under. Filled by the
        // reindex `CODE_VERSION` 13 triggers.
        // One row per (file, caller, callee, qualifier): `gate::check` and
        // `tripwire::check` from one function are two edges.
        conn.execute_batch(
            "DROP TABLE IF EXISTS code_calls;
             CREATE TABLE code_calls (
                 project_id INTEGER NOT NULL,
                 src        TEXT NOT NULL,
                 caller     TEXT NOT NULL,
                 callee     TEXT NOT NULL,
                 line       INTEGER NOT NULL,
                 qual       TEXT NOT NULL DEFAULT '',
                 PRIMARY KEY (project_id, src, caller, callee, qual),
                 FOREIGN KEY (project_id, src) REFERENCES code_files(project_id, path) ON DELETE CASCADE
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS code_calls_callee ON code_calls(project_id, callee);",
        )?;
    }
    if version < 23 {
        // v23: names declared with a type (`code::fields`), so a method
        // call's receiver can say which definition it reaches. Filled by
        // the reindex `CODE_VERSION` 14 triggers.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS code_fields (
                 project_id INTEGER NOT NULL,
                 src        TEXT NOT NULL,
                 name       TEXT NOT NULL,
                 type       TEXT NOT NULL,
                 PRIMARY KEY (project_id, src, name, type),
                 FOREIGN KEY (project_id, src) REFERENCES code_files(project_id, path) ON DELETE CASCADE
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS code_fields_name ON code_fields(project_id, name);",
        )?;
    }
    if version < 24 {
        // v24: agent searches and the questions learned from them
        // (`learned.rs`): a search followed by an open of a note it did not
        // rank first teaches that note the question.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS searches (
                 id         INTEGER PRIMARY KEY,
                 at         INTEGER NOT NULL,
                 project_id INTEGER NOT NULL,
                 session    TEXT,
                 query      TEXT NOT NULL,
                 also       TEXT NOT NULL DEFAULT '',
                 printed    TEXT NOT NULL DEFAULT '',
                 space_id   INTEGER,
                 vec        BLOB,
                 done       INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS searches_pending ON searches(project_id, done, at);
             CREATE TABLE IF NOT EXISTS learned (
                 id         INTEGER PRIMARY KEY,
                 project_id INTEGER NOT NULL,
                 note_id    INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
                 query      TEXT NOT NULL,
                 vec        BLOB NOT NULL,
                 space_id   INTEGER NOT NULL,
                 at         INTEGER NOT NULL,
                 kind       TEXT NOT NULL,
                 UNIQUE (note_id, query)
             );
             CREATE INDEX IF NOT EXISTS learned_project ON learned(project_id, space_id);",
        )?;
    }
    if version < 25 {
        // v25: a learned question keeps its content words (indexed, so a
        // search reads only rows sharing a word) and the sign bits of its
        // vector (64 bytes, a prefilter before the exact cosine): at 8,000
        // rows the scan of every vector and question cost 57 ms a search.
        // Backfill before the index and its triggers exist, then rebuild it.
        conn.execute_batch("ALTER TABLE learned ADD COLUMN words TEXT NOT NULL DEFAULT ''; ALTER TABLE learned ADD COLUMN bits BLOB;")?;
        let rows: Vec<(i64, String, Vec<u8>)> =
            conn.prepare("SELECT id, query, vec FROM learned")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (id, query, vec) in rows {
            conn.execute(
                "UPDATE learned SET words = ?2, bits = ?3 WHERE id = ?1",
                rusqlite::params![id, crate::learned::words_of(&query), crate::learned::bits_of_blob(&vec)],
            )?;
        }
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS learned_fts USING fts5(words, content='learned', content_rowid='id', tokenize=\"unicode61 tokenchars '_-'\");
             CREATE TRIGGER IF NOT EXISTS learned_ai AFTER INSERT ON learned BEGIN
                 INSERT INTO learned_fts(rowid, words) VALUES (new.id, new.words);
             END;
             CREATE TRIGGER IF NOT EXISTS learned_ad AFTER DELETE ON learned BEGIN
                 INSERT INTO learned_fts(learned_fts, rowid, words) VALUES ('delete', old.id, old.words);
             END;
             CREATE TRIGGER IF NOT EXISTS learned_au AFTER UPDATE OF words ON learned BEGIN
                 INSERT INTO learned_fts(learned_fts, rowid, words) VALUES ('delete', old.id, old.words);
                 INSERT INTO learned_fts(rowid, words) VALUES (new.id, new.words);
             END;
             INSERT INTO learned_fts(learned_fts) VALUES ('rebuild');",
        )?;
    }
    if version < 26 {
        // v26: a read keeps its line range, and a learned question the code
        // range its asker read next, so the next similar question is shown
        // where to read first (`learned.rs`, `spans::ranked`).
        conn.execute_batch(
            "ALTER TABLE reads ADD COLUMN start INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE reads ADD COLUMN end INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE learned ADD COLUMN path TEXT;
             ALTER TABLE learned ADD COLUMN start INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE learned ADD COLUMN end INTEGER NOT NULL DEFAULT 0;
             CREATE INDEX IF NOT EXISTS reads_session ON reads(project_id, session, at);",
        )?;
    }
    if version < 27 {
        // v27: the line a chunk ends on (`chunk::Chunk::end`), so a hit's
        // range covers every bullet it holds; filled by the rechunk that
        // `CHUNK_VERSION` 5 triggers, 0 until then.
        conn.execute_batch("ALTER TABLE chunks ADD COLUMN end_line INTEGER NOT NULL DEFAULT 0;")?;
    }
    if version < 28 {
        // v28: definitions found by description (`code::find`): the words of
        // a definition's name, signature and the comment above it in an FTS
        // index. `code_defs` gets an explicit id so the index's rowids survive
        // VACUUM; it is rebuilt from the checkout, so every code index starts
        // over (`code_files` cascades to the rest).
        conn.execute_batch(
            "DELETE FROM code_files;
             DELETE FROM meta WHERE key LIKE 'code_version:%';
             DROP TABLE code_defs;
             CREATE TABLE code_defs (
                 id         INTEGER PRIMARY KEY,
                 project_id INTEGER NOT NULL,
                 path       TEXT NOT NULL,
                 name       TEXT NOT NULL,
                 kind       TEXT NOT NULL,
                 line       INTEGER NOT NULL,
                 end_line   INTEGER NOT NULL DEFAULT 0,
                 anchor     TEXT NOT NULL DEFAULT '',
                 name_words TEXT NOT NULL DEFAULT '',
                 words      TEXT NOT NULL DEFAULT '',
                 FOREIGN KEY (project_id, path) REFERENCES code_files(project_id, path) ON DELETE CASCADE
             );
             CREATE INDEX code_defs_name ON code_defs(project_id, name);
             CREATE INDEX code_defs_path ON code_defs(project_id, path);
             CREATE VIRTUAL TABLE code_defs_fts USING fts5(name_words, words, content='code_defs', content_rowid='id');
             CREATE TRIGGER code_defs_ai AFTER INSERT ON code_defs BEGIN
                 INSERT INTO code_defs_fts(rowid, name_words, words) VALUES (new.id, new.name_words, new.words);
             END;
             CREATE TRIGGER code_defs_ad AFTER DELETE ON code_defs BEGIN
                 INSERT INTO code_defs_fts(code_defs_fts, rowid, name_words, words) VALUES ('delete', old.id, old.name_words, old.words);
             END;",
        )?;
    }
    if version < 29 {
        // v29: the type a method belongs to (`code::owners`: its `impl`,
        // class, trait or Go receiver), so `X::name` resolves to X's method
        // and `links` can print it. The code index is rebuilt to fill it.
        conn.execute_batch(
            "ALTER TABLE code_defs ADD COLUMN owner TEXT NOT NULL DEFAULT '';
             DELETE FROM code_files;
             DELETE FROM meta WHERE key LIKE 'code_version:%';",
        )?;
    }
    if version < 30 {
        // v30: repairs a store the unlocked migration left at v29 without
        // `code_defs.owner` (0.11.51); the code index is rebuilt to fill it.
        let has_owner: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('code_defs') WHERE name = 'owner')", [], |r| r.get(0))?;
        // The same race dropped `code_defs`' triggers with the table, so the
        // word index stopped following it: the index and its triggers are
        // made again and refilled from the table.
        if !has_owner {
            conn.execute_batch("ALTER TABLE code_defs ADD COLUMN owner TEXT NOT NULL DEFAULT '';")?;
        }
        conn.execute_batch(
            "DROP TRIGGER IF EXISTS code_defs_ai;
             DROP TRIGGER IF EXISTS code_defs_ad;
             DROP TABLE IF EXISTS code_defs_fts;
             CREATE VIRTUAL TABLE code_defs_fts USING fts5(name_words, words, content='code_defs', content_rowid='id');
             CREATE TRIGGER code_defs_ai AFTER INSERT ON code_defs BEGIN
                 INSERT INTO code_defs_fts(rowid, name_words, words) VALUES (new.id, new.name_words, new.words);
             END;
             CREATE TRIGGER code_defs_ad AFTER DELETE ON code_defs BEGIN
                 INSERT INTO code_defs_fts(code_defs_fts, rowid, name_words, words) VALUES ('delete', old.id, old.name_words, old.words);
             END;",
        )?;
        if has_owner {
            conn.execute_batch("INSERT INTO code_defs_fts(code_defs_fts) VALUES ('rebuild');")?;
        } else {
            conn.execute_batch("DELETE FROM code_files; DELETE FROM meta WHERE key LIKE 'code_version:%';")?;
        }
    }
    if version < 31 {
        // v31: a file's header comment (`code::header`), so a script no note
        // names is found by what its first lines say it does. Filled as
        // `CODE_VERSION` 27 reindexes every file.
        let has_head: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('code_files') WHERE name = 'head')", [], |r| r.get(0))?;
        if !has_head {
            conn.execute_batch("ALTER TABLE code_files ADD COLUMN head TEXT NOT NULL DEFAULT ''; ALTER TABLE code_files ADD COLUMN head_end INTEGER NOT NULL DEFAULT 0;")?;
        }
    }
    if version < 32 {
        // v32: the policy's candidates per logged search (`policy.rs`), so a
        // settled search rewards what was shown.
        let has: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('searches') WHERE name = 'policy')", [], |r| r.get(0))?;
        if !has {
            conn.execute_batch("ALTER TABLE searches ADD COLUMN policy TEXT NOT NULL DEFAULT '';")?;
        }
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// Schema v1. `text_indexed` is derived from `text` (see `chunk::index_text`)
/// and exists only so that FTS5 external content can rebuild from it.
const SCHEMA_V1: &str = r#"
CREATE TABLE projects (
    id        INTEGER PRIMARY KEY,
    name      TEXT NOT NULL UNIQUE,
    paths     TEXT NOT NULL,
    grp       TEXT,
    added_at  INTEGER NOT NULL
);

CREATE TABLE notes (
    id            INTEGER PRIMARY KEY,
    project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    path          TEXT NOT NULL,
    title         TEXT NOT NULL,
    content       TEXT NOT NULL,
    content_hash  BLOB NOT NULL,
    source        TEXT NOT NULL,
    saved_at      INTEGER NOT NULL,
    deleted_at    INTEGER,
    UNIQUE (project_id, path)
);

CREATE TABLE chunks (
    id            INTEGER PRIMARY KEY,
    note_id       INTEGER NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    project_id    INTEGER NOT NULL,
    ordinal       INTEGER NOT NULL,
    heading_path  TEXT NOT NULL,
    text          TEXT NOT NULL,
    text_indexed  TEXT NOT NULL
);
CREATE INDEX chunks_note ON chunks(note_id);

CREATE VIRTUAL TABLE chunks_fts USING fts5(
    heading_path,
    text_indexed,
    project_id UNINDEXED,
    content='chunks',
    content_rowid='id',
    tokenize="unicode61 tokenchars '_-'"
);

CREATE TRIGGER chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, heading_path, text_indexed, project_id)
    VALUES (new.id, new.heading_path, new.text_indexed, new.project_id);
END;
CREATE TRIGGER chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, heading_path, text_indexed, project_id)
    VALUES ('delete', old.id, old.heading_path, old.text_indexed, old.project_id);
END;
CREATE TRIGGER chunks_au AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, heading_path, text_indexed, project_id)
    VALUES ('delete', old.id, old.heading_path, old.text_indexed, old.project_id);
    INSERT INTO chunks_fts(rowid, heading_path, text_indexed, project_id)
    VALUES (new.id, new.heading_path, new.text_indexed, new.project_id);
END;

CREATE TABLE spaces (
    id            INTEGER PRIMARY KEY,
    model         TEXT NOT NULL,
    dims          INTEGER NOT NULL,
    precision     TEXT NOT NULL,
    runtime       TEXT NOT NULL,
    query_prefix  TEXT NOT NULL,
    doc_prefix    TEXT NOT NULL,
    status        TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);

-- project_id is denormalised here so the scan filters without a join.
-- A rowid table on purpose: 1 KB blobs pack three to a page; as WITHOUT
-- ROWID each one spilled to an overflow page (4x the disk, 8x the scan time).
CREATE TABLE vectors (
    chunk_id    INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    space_id    INTEGER NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    project_id  INTEGER NOT NULL,
    vec         BLOB NOT NULL,
    PRIMARY KEY (space_id, chunk_id)
);
CREATE INDEX vectors_scope ON vectors(space_id, project_id);

CREATE TABLE query_cache (
    space_id   INTEGER NOT NULL,
    text       TEXT NOT NULL,
    vec        BLOB NOT NULL,
    last_used  INTEGER NOT NULL,
    PRIMARY KEY (space_id, text)
) WITHOUT ROWID;

CREATE TABLE meta (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
) WITHOUT ROWID;
"#;

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare_cached("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    Ok(match rows.next()? {
        Some(r) => Some(r.get(0)?),
        None => None,
    })
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

/// Builds `(?N, ?N+1, ...)` for an `IN` list. Scopes are small, so a
/// stack-friendly string is cheaper than a carray extension.
pub fn placeholders(start: usize, count: usize) -> String {
    let mut s = String::with_capacity(count * 4 + 2);
    s.push('(');
    for i in 0..count {
        if i > 0 {
            s.push(',');
        }
        s.push('?');
        s.push_str(&(start + i).to_string());
    }
    s.push(')');
    s
}
