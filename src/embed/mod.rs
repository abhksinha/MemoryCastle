//! Embeddings: the `Embedder` trait, embedding spaces, vector encoding and the
//! bounded query cache. The runtime behind the trait is a dependency chosen by
//! Cargo feature; nothing on the keyword path touches this module's runtime.

#[cfg(feature = "ort")]
pub mod download;
#[cfg(feature = "ort")]
pub mod ort_rt;
#[cfg(feature = "ort")]
pub mod tokenizer;

use crate::{sys, Result};
use half::f16;
use rusqlite::{params, Connection, OptionalExtension};

/// Produces vectors of `dims()` floats, L2-normalised, appended to `out`.
/// `texts` already carry their task prefix.
pub trait Embedder {
    fn dims(&self) -> usize;
    fn embed(&mut self, texts: &[&str], out: &mut Vec<f32>) -> Result<()>;
}

/// What defines an embedding space. Any change is a new space, never a migration.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceDef {
    pub model: String,
    pub dims: usize,
    pub precision: String,
    pub runtime: String,
    pub query_prefix: String,
    pub doc_prefix: String,
}

impl SpaceDef {
    /// EmbeddingGemma 2, text only, 512 dims, search prefixes. The weight
    /// precision defaults to int8 and can be set with `MEMORYCASTLE_MODEL`
    /// (int8, q4, q4f16) so the harness can compare them.
    pub fn gemma2_default() -> SpaceDef {
        let precision = std::env::var("MEMORYCASTLE_MODEL").unwrap_or_else(|_| "int8".into());
        // Matryoshka cut: 512 by default; `MEMORYCASTLE_DIMS=768` keeps the
        // model's full width for comparison (a new space either way).
        let dims = std::env::var("MEMORYCASTLE_DIMS").ok().and_then(|d| d.parse().ok()).filter(|d| [128usize, 256, 512, 768].contains(d)).unwrap_or(crate::limits::EMBED_DIMS);
        SpaceDef {
            model: "onnx-community/embeddinggemma-2-ONNX".into(),
            dims,
            precision,
            runtime: "ort".into(),
            query_prefix: "task: search result | query: ".into(),
            doc_prefix: "title: {title} | text: ".into(),
        }
    }

    pub fn query_text(&self, query: &str) -> String {
        let mut s = String::with_capacity(self.query_prefix.len() + query.len());
        s.push_str(&self.query_prefix);
        s.push_str(query);
        s
    }

    /// `title: {heading path} | text: {chunk}`; `none` when there is no heading.
    pub fn doc_text(&self, heading_path: &str, text: &str) -> String {
        let title = if heading_path.is_empty() { "none" } else { heading_path };
        let mut s = String::with_capacity(self.doc_prefix.len() + title.len() + text.len());
        s.push_str(&self.doc_prefix.replace("{title}", title));
        s.push_str(text);
        s
    }
}

#[derive(Debug, Clone)]
pub struct Space {
    pub id: i64,
    pub def: SpaceDef,
    pub status: String,
}

fn row_to_space(r: &rusqlite::Row) -> rusqlite::Result<Space> {
    Ok(Space {
        id: r.get(0)?,
        def: SpaceDef {
            model: r.get(1)?,
            dims: r.get::<_, i64>(2)? as usize,
            precision: r.get(3)?,
            runtime: r.get(4)?,
            query_prefix: r.get(5)?,
            doc_prefix: r.get(6)?,
        },
        status: r.get(7)?,
    })
}

const SPACE_COLS: &str = "id, model, dims, precision, runtime, query_prefix, doc_prefix, status";

pub fn active_space(conn: &Connection) -> Result<Option<Space>> {
    Ok(conn
        .query_row(
            &format!("SELECT {SPACE_COLS} FROM spaces WHERE status = 'active' ORDER BY id DESC LIMIT 1"),
            [],
            row_to_space,
        )
        .optional()?)
}

pub fn space_by_status(conn: &Connection, status: &str) -> Result<Option<Space>> {
    Ok(conn
        .query_row(
            &format!("SELECT {SPACE_COLS} FROM spaces WHERE status = ?1 ORDER BY id DESC LIMIT 1"),
            [status],
            row_to_space,
        )
        .optional()?)
}

pub fn insert_space(conn: &Connection, def: &SpaceDef, status: &str) -> Result<Space> {
    conn.execute(
        "INSERT INTO spaces(model, dims, precision, runtime, query_prefix, doc_prefix, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![def.model, def.dims as i64, def.precision, def.runtime, def.query_prefix, def.doc_prefix, status, sys::now()],
    )?;
    Ok(Space { id: conn.last_insert_rowid(), def: def.clone(), status: status.into() })
}

/// Makes `space_id` the active space; the previous active one is retired and
/// its vectors dropped (they are derived and rebuildable).
pub fn activate_space(conn: &mut Connection, space_id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("UPDATE spaces SET status = 'retired' WHERE status = 'active' AND id != ?1", [space_id])?;
    tx.execute("UPDATE spaces SET status = 'active' WHERE id = ?1", [space_id])?;
    tx.execute("DELETE FROM vectors WHERE space_id IN (SELECT id FROM spaces WHERE status = 'retired')", [])?;
    tx.execute("DELETE FROM query_cache WHERE space_id != ?1", [space_id])?;
    tx.execute("DELETE FROM vector_bits WHERE space_id != ?1", [space_id])?;
    tx.execute("DELETE FROM spaces WHERE status = 'retired'", [])?;
    tx.commit()?;
    Ok(())
}

/// Returns the active space, creating one from `def` if none exists. If the
/// active space differs from `def`, errors: that is a `reindex --space new`.
pub fn ensure_space(conn: &Connection, def: &SpaceDef) -> Result<Space> {
    if let Some(s) = active_space(conn)? {
        if s.def != *def {
            return Err(crate::err!(
                "active space is {} ({} dims, {}); the configured model differs. Run `memorycastle reindex --space new`",
                s.def.model, s.def.dims, s.def.precision
            ));
        }
        return Ok(s);
    }
    insert_space(conn, def, "active")
}

/// f32 slice -> little-endian f16 bytes.
pub fn encode_f16(v: &[f32], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(v.len() * 2);
    for &x in v {
        out.extend_from_slice(&f16::from_f32(x).to_le_bytes());
    }
}

/// f16 bytes -> f32, written into `out` (cleared first).
pub fn decode_f16(bytes: &[u8], out: &mut Vec<f32>) {
    out.clear();
    out.reserve(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push(f16::from_le_bytes([pair[0], pair[1]]).to_f32());
    }
}

/// Truncates to `dims` (Matryoshka) and L2-normalises in place.
pub fn truncate_normalise(v: &mut Vec<f32>, dims: usize) {
    v.truncate(dims);
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

pub use crate::limits::QUERY_CACHE_MAX;

/// Looks up a cached query vector (the key is the prefixed text).
pub fn cached_query(conn: &Connection, space_id: i64, text: &str, out: &mut Vec<f32>) -> Result<bool> {
    let mut stmt = conn.prepare_cached("SELECT vec FROM query_cache WHERE space_id = ?1 AND text = ?2")?;
    let mut rows = stmt.query(params![space_id, text])?;
    if let Some(r) = rows.next()? {
        let blob: Vec<u8> = r.get(0)?;
        decode_f16(&blob, out);
        drop(rows);
        conn.execute(
            "UPDATE query_cache SET last_used = ?3 WHERE space_id = ?1 AND text = ?2",
            params![space_id, text, sys::now()],
        )?;
        return Ok(true);
    }
    Ok(false)
}

/// Inserts a query vector and keeps the cache under `QUERY_CACHE_MAX` rows.
pub fn cache_query(conn: &Connection, space_id: i64, text: &str, vec: &[f32]) -> Result<()> {
    if crate::db::is_readonly(conn) {
        return Ok(());
    }
    let mut blob = Vec::new();
    encode_f16(vec, &mut blob);
    conn.execute(
        "INSERT OR REPLACE INTO query_cache(space_id, text, vec, last_used) VALUES (?1, ?2, ?3, ?4)",
        params![space_id, text, blob, sys::now()],
    )?;
    let n: i64 = conn.query_row("SELECT count(*) FROM query_cache", [], |r| r.get(0))?;
    if n > QUERY_CACHE_MAX {
        conn.execute(
            "DELETE FROM query_cache WHERE (space_id, text) IN
               (SELECT space_id, text FROM query_cache ORDER BY last_used ASC LIMIT ?1)",
            [n - QUERY_CACHE_MAX],
        )?;
    }
    Ok(())
}

/// A deterministic bag-of-terms embedder for tests and the stage-1 harness.
/// Each term hashes to a dimension; the vector is L2-normalised. It has no
/// semantics, so it only proves plumbing, fusion and isolation.
pub struct HashEmbedder {
    pub dims: usize,
}

impl Embedder for HashEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }
    fn embed(&mut self, texts: &[&str], out: &mut Vec<f32>) -> Result<()> {
        for t in texts {
            let start = out.len();
            out.resize(start + self.dims, 0.0);
            let body = t.rsplit(" | ").next().unwrap_or(t);
            for term in crate::fts::terms(body) {
                let mut h: u64 = 0xcbf29ce484222325;
                for b in term.to_lowercase().bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                out[start + (h % self.dims as u64) as usize] += 1.0;
            }
            let slice = &mut out[start..];
            let norm = slice.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                slice.iter_mut().for_each(|x| *x /= norm);
            }
        }
        Ok(())
    }
}

impl HashEmbedder {
    pub fn space_def(dims: usize) -> SpaceDef {
        SpaceDef {
            model: "hash".into(),
            dims,
            precision: "f32".into(),
            runtime: "stub".into(),
            query_prefix: String::new(),
            doc_prefix: String::new(),
        }
    }
}

/// Opens the configured embedder. With the `ort` feature this downloads the
/// model on first use; without it, meaning search is unavailable.
pub fn open_default(quiet: bool) -> Result<Box<dyn Embedder>> {
    #[cfg(feature = "ort")]
    {
        let e = ort_rt::OrtEmbedder::open(&SpaceDef::gemma2_default(), quiet)?;
        Ok(Box::new(e))
    }
    #[cfg(not(feature = "ort"))]
    {
        let _ = quiet;
        Err("this build has no embedding runtime (built without the `ort` feature); keyword search only".into())
    }
}

pub fn default_space_def() -> SpaceDef {
    #[cfg(feature = "ort")]
    {
        SpaceDef::gemma2_default()
    }
    #[cfg(not(feature = "ort"))]
    {
        HashEmbedder::space_def(64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_roundtrip() {
        let v = vec![0.5f32, -0.25, 0.0, 1.0];
        let mut b = Vec::new();
        encode_f16(&v, &mut b);
        assert_eq!(b.len(), 8);
        let mut back = Vec::new();
        decode_f16(&b, &mut back);
        assert_eq!(back, v);
    }

    #[test]
    fn truncate_and_normalise() {
        let mut v = vec![3.0, 4.0, 100.0];
        truncate_normalise(&mut v, 2);
        assert_eq!(v, vec![0.6, 0.8]);
    }

    #[test]
    fn doc_text_uses_none_without_heading() {
        let d = SpaceDef::gemma2_default();
        assert_eq!(d.doc_text("", "x"), "title: none | text: x");
        assert_eq!(d.doc_text("A > B", "x"), "title: A > B | text: x");
        assert_eq!(d.query_text("q"), "task: search result | query: q");
    }

    #[test]
    fn query_cache_is_bounded() {
        let conn = crate::db::open_memory().unwrap();
        for i in 0..(QUERY_CACHE_MAX + 20) {
            cache_query(&conn, 1, &format!("q{i}"), &[1.0, 0.0]).unwrap();
        }
        let n: i64 = conn.query_row("SELECT count(*) FROM query_cache", [], |r| r.get(0)).unwrap();
        assert_eq!(n, QUERY_CACHE_MAX);
        let mut out = Vec::new();
        assert!(cached_query(&conn, 1, "q275", &mut out).unwrap());
        assert_eq!(out, vec![1.0, 0.0]);
        assert!(!cached_query(&conn, 1, "q0", &mut out).unwrap());
    }
}
