//! The search pipeline: BM25 always, the model only when it pays, reciprocal
//! rank fusion, and a trace on every result.

use crate::embed::{self, Embedder, Space};
use crate::fts;
use crate::project::Scope;
use crate::Result;
use rusqlite::{params, OptionalExtension};
use rusqlite::{params_from_iter, Connection};
use std::collections::HashMap;

pub use crate::limits::NO_COVERAGE_FACTOR;
use crate::limits::{BITS_CANDIDATES, BITS_MIN_ROWS, CHANNEL_TOP, RRF_K, SUPERSEDED_WEIGHT};


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Keyword first; the model only when cold and no exact identifier hit.
    Auto,
    /// Never load the model.
    Keyword,
    /// Always run the meaning channel.
    Meaning,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub k: usize,
    pub mode: Mode,
    /// Files this session is working on, repo-relative, with a weight 0 to
    /// 1 (`Context::working_set`); notes linked to them rank higher.
    pub working: Vec<(String, f32)>,
}

/// Where a result came from. Ranks are 1-based; `None` means that channel did
/// not return the chunk.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Trace {
    pub bm25_rank: Option<u16>,
    pub vec_rank: Option<u16>,
    pub score: f32,
    pub weight: f32,
    /// Cosine between the query and this chunk (meaning channel), if computed.
    pub cosine: Option<f32>,
    /// Share of the query's terms found in the chunk text, 0 to 1.
    pub coverage: f32,
    /// 0 to 1: the best of cosine and coverage, what "relevant" is judged on.
    pub confidence: f32,
    /// Graph weight applied: 1.5 when the note links a file or symbol the
    /// question names, 1.2 when a question symbol is defined in a file the
    /// note links, 1.0 otherwise.
    pub graph: f32,
    /// Working-set weight applied: up to `WORKING_WEIGHT` when the note links
    /// a file the session is working on, or a symbol defined in one.
    pub working: f32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Hit {
    pub chunk_id: i64,
    pub note_id: i64,
    pub project_id: i64,
    pub project: String,
    pub path: String,
    pub heading: String,
    pub saved_at: i64,
    /// The note's own `Date:` header, when it has one.
    pub dated: Option<i64>,
    pub source: String,
    /// Set when the note's header declares a newer note replaced it.
    pub superseded_by: Option<String>,
    /// Codebase files the note is about (from its `Files:` line and body).
    pub files: Vec<String>,
    pub text: String,
    /// 1-based line in the note where the chunk starts.
    pub line: u32,
    pub trace: Trace,
    /// Symbols the note names whose code changed since the note was saved
    /// (filled by `mark_code_changed` when a checkout is at hand).
    #[serde(default)]
    pub code_changed: Vec<String>,
    /// The note's scored chunks as line ranges, best first, at most
    /// `SEARCH_CHUNKS_PER_NOTE`: which lines of the note to read, with their
    /// own confidence. A note that qualified lists its other matching parts
    /// even under `MIN_CONFIDENCE` (a Rejected section missing one of the
    /// question's words still holds the answer).
    #[serde(default)]
    pub parts: Vec<Part>,
}

/// One chunk of a hit's note: lines `line..=end`, its section, its confidence.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Part {
    pub line: u32,
    pub end: u32,
    pub confidence: f32,
    pub section: String,
}

impl Hit {
    /// The last line of the chunk in its note.
    pub fn end_line(&self) -> u32 {
        self.line + (self.text.trim_end().lines().count() as u32).saturating_sub(1)
    }
}

/// What the pipeline decided, for `--explain`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Explain {
    pub fts_query: Option<String>,
    pub bm25_hits: usize,
    pub model_used: bool,
    pub model_reason: &'static str,
    pub query_cached: bool,
    pub vec_hits: usize,
    /// Milliseconds spent in each stage.
    pub bm25_ms: f32,
    pub embed_ms: f32,
    pub scan_ms: f32,
    /// Vectors examined by the bit prefilter (0 when the exact scan ran).
    pub prefiltered: usize,
    /// Best confidence seen, including hits dropped as irrelevant.
    pub best_confidence: f32,
    /// The question's rare words, which a hit must hold for its cosine to count in full.
    pub distinctive: Vec<String>,
    /// Hits dropped for confidence under `MIN_CONFIDENCE`.
    pub dropped: usize,
}

/// A searchable memory: the store, or a baseline. The harness measures any of these.
pub trait Memory {
    fn search(&mut self, scope: &Scope, query: &str, opts: &SearchOptions) -> Result<Vec<Hit>>;
}

/// The model source for a search: already warm, loadable on demand, or absent.
pub enum ModelSource<'a> {
    None,
    Warm(&'a mut dyn Embedder),
    Lazy(&'a mut dyn FnMut() -> Result<Box<dyn Embedder>>),
}

pub fn search(
    conn: &Connection,
    scope: &Scope,
    query: &str,
    opts: &SearchOptions,
    model: ModelSource<'_>,
) -> Result<(Vec<Hit>, Explain)> {
    let mut ex = Explain::default();
    if scope.is_empty() {
        return Ok((Vec::new(), ex));
    }
    // 1. BM25, always.
    let fts_query = fts::build(query);
    ex.fts_query = fts_query.clone();
    let t = std::time::Instant::now();
    let bm25: Vec<i64> = match &fts_query {
        Some(q) => bm25_channel(conn, scope, q)?,
        None => Vec::new(),
    };
    ex.bm25_ms = t.elapsed().as_secs_f32() * 1000.0;
    ex.bm25_hits = bm25.len();
    // Identifiers from the question that prove a hit when found verbatim.
    let idents = rare_identifiers(conn, scope, query)?;

    // 2. Decide whether the model runs.
    let space = embed::active_space(conn)?;
    let run_model = match (opts.mode, &space, &model) {
        (Mode::Keyword, _, _) => {
            ex.model_reason = "--keyword";
            false
        }
        (_, None, _) => {
            ex.model_reason = "no active embedding space";
            false
        }
        (_, _, ModelSource::None) => {
            ex.model_reason = "no embedding runtime";
            false
        }
        (Mode::Meaning, _, _) => {
            ex.model_reason = "--meaning";
            true
        }
        (Mode::Auto, _, ModelSource::Warm(_)) => {
            ex.model_reason = "model warm";
            true
        }
        (Mode::Auto, _, ModelSource::Lazy(_)) => {
            if bm25.first().is_some_and(|&id| exact_identifier_hit(conn, &idents, id).unwrap_or(false)) {
                ex.model_reason = "cold model, exact identifier hit";
                false
            } else {
                ex.model_reason = "cold model loaded";
                true
            }
        }
    };

    // 3. Meaning channel.
    let mut vec_hits: Vec<(f32, i64)> = Vec::new();
    if run_model {
        let space = space.as_ref().unwrap();
        let qtext = space.def.query_text(query);
        let mut qvec: Vec<f32> = Vec::with_capacity(space.def.dims);
        let t = std::time::Instant::now();
        if embed::cached_query(conn, space.id, &qtext, &mut qvec)? {
            ex.query_cached = true;
        } else {
            let mut owned;
            let embedder: &mut dyn Embedder = match model {
                ModelSource::Warm(e) => e,
                ModelSource::Lazy(open) => {
                    owned = open()?;
                    owned.as_mut()
                }
                ModelSource::None => unreachable!(),
            };
            embedder.embed(&[qtext.as_str()], &mut qvec)?;
            embed::truncate_normalise(&mut qvec, space.def.dims);
            embed::cache_query(conn, space.id, &qtext, &qvec)?;
        }
        ex.embed_ms = t.elapsed().as_secs_f32() * 1000.0;
        ex.model_used = true;
        let t = std::time::Instant::now();
        let (hits_v, prefiltered) = vector_channel(conn, scope, space, &qvec)?;
        vec_hits = hits_v;
        ex.prefiltered = prefiltered;
        ex.scan_ms = t.elapsed().as_secs_f32() * 1000.0;
        ex.vec_hits = vec_hits.len();
    }

    // 4. Fuse.
    let terms: Vec<String> = fts::terms(query).iter().filter(|t| !is_stopword(t)).map(|t| t.to_ascii_lowercase()).collect();
    // An acronym written in capitals (NDK, JWT) is rare and topical even at
    // three letters; everyday short words are not.
    let acronyms: Vec<String> = fts::terms(query).iter().filter(|t| t.len() >= 3 && t.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())).map(|t| t.to_ascii_lowercase()).collect();
    let distinctive = distinctive_terms(conn, scope, &terms, &acronyms)?;
    ex.distinctive = distinctive.clone();
    let mut fused: HashMap<i64, Trace> = HashMap::with_capacity(bm25.len() + vec_hits.len());
    for (i, id) in bm25.iter().enumerate() {
        let t = fused.entry(*id).or_default();
        t.bm25_rank = Some(i as u16 + 1);
        t.score += 1.0 / (RRF_K + i as f32 + 1.0);
    }
    for (i, (cos, id)) in vec_hits.iter().enumerate() {
        let t = fused.entry(*id).or_default();
        t.vec_rank = Some(i as u16 + 1);
        t.score += 1.0 / (RRF_K + i as f32 + 1.0);
        t.cosine = Some(*cos);
    }
    if fused.is_empty() {
        return Ok((Vec::new(), ex));
    }
    // 5. Metadata for the fused set, then project weights, section cues,
    //    confidence, then top k.
    let ids: Vec<i64> = fused.keys().copied().collect();
    let mut hits = fetch_hits(conn, &ids)?;
    let cue = section_cue(query);
    for h in hits.iter_mut() {
        let mut t = fused.remove(&h.chunk_id).unwrap_or_default();
        t.weight = scope.weight(h.project_id);
        if h.superseded_by.is_some() {
            t.weight *= SUPERSEDED_WEIGHT;
        }
        if let Some(c) = cue {
            if h.heading.to_ascii_lowercase().contains(c) {
                t.weight *= SECTION_CUE_WEIGHT;
            }
        }        t.score *= t.weight;
        // Heading and text together: a title holds the words a body implies.
        let with_heading = format!("{}\n{}", h.heading, h.text);
        t.coverage = coverage(&terms, &with_heading);
        // The model judges relevance when it ran. Coverage stands in when it
        // did not, and an identifier from the question found verbatim in the
        // chunk is relevant whatever the model thinks.
        let ident_hit = idents.iter().any(|t| h.text.contains(t.as_str()));
        let cos = t.cosine.map(|c| ((c - COSINE_FLOOR) / (COSINE_CEIL - COSINE_FLOOR)).clamp(0.0, 1.0)).unwrap_or(0.0);
        // Coverage of one content word proves little ("memorycastle" in
        // passing); of four, a lot. Scale by how many words the question had.
        // Without the model, the words are all there is, and a chunk that
        // has them is what a keyword search means.
        let cov = if run_model { t.coverage * (terms.len().min(4) as f32 / 4.0) } else { t.coverage };
        if run_model {
            t.score *= crate::limits::COSINE_SCORE_BASE + cos;
        }
        // A chunk with none of the question's distinctive words (the rare
        // ones, by document frequency) is a near miss whatever the model
        // says: same area, different feature. Its cosine counts for a
        // fraction, so only a very close match survives.
        // Graded: the cosine counts in full only when every rare word is
        // present; with none it counts for NO_COVERAGE_FACTOR of itself.
        // How much the gate bites grows with how many rare words there were:
        // one absent word is weak evidence of a near miss (it may be the
        // asker's synonym), three absent words are strong.
        let share = if distinctive.is_empty() { 1.0 } else { coverage(&distinctive, &with_heading) };
        let weight = (distinctive.len().min(3) as f32) / 3.0;
        let cos = if run_model { cos * (1.0 - (1.0 - NO_COVERAGE_FACTOR) * (1.0 - share) * weight) } else { cos };
        // When the question has rare words, coverage is coverage of those:
        // "status editor" in passing proves nothing about "blurred backdrop".
        // One rare word present is weak evidence (0.25, under the gate); the
        // model's cosine must carry such a hit on its own.
        let cov = if distinctive.is_empty() { cov } else { coverage(&distinctive, &with_heading) * if run_model { distinctive.len().min(4) as f32 / 4.0 } else { 1.0 } };
        // The model judges relevance when it ran: words can add to its
        // verdict but not overrule it, so coverage is capped at the raw
        // rescaled cosine (a chunk the model put under the floor stays there).
        let raw_cos = t.cosine.map(|c| ((c - COSINE_FLOOR) / (COSINE_CEIL - COSINE_FLOOR)).clamp(0.0, 1.0)).unwrap_or(0.0);
        t.confidence = if ident_hit { 1.0 } else if run_model { cos.max(cov.min(raw_cos)) } else { cos.max(cov) };
        h.trace = t;
    }
    // Graph rerank: what the note declares it is about, against what the
    // question names. Links for the whole fused set, one indexed query.
    let (q_files, q_symbols) = crate::graph::query_entities(query);
    let mut note_ids: Vec<i64> = hits.iter().map(|h| h.note_id).collect();
    note_ids.sort_unstable();
    note_ids.dedup();
    let links = crate::graph::links_of(conn, &note_ids)?;
    if !q_files.is_empty() || !q_symbols.is_empty() {
        // Bridge: files that define the question's symbols, from the code index.
        let mut def_files: Vec<String> = Vec::new();
        for pid in scope.ids() {
            for sym in &q_symbols {
                for d in crate::code::defs_named(conn, pid, sym).unwrap_or_default() {
                    def_files.push(d.path);
                }
            }
        }
        for h in hits.iter_mut() {
            let Some(l) = links.get(&h.note_id) else { continue };
            let direct = l.iter().any(|(k, t)| match k {
                crate::graph::Kind::File => q_files.iter().any(|f| crate::graph::same_file(t, f)),
                crate::graph::Kind::Symbol => q_symbols.iter().any(|s| crate::graph::same_symbol(s, t)),
                crate::graph::Kind::Note => false,
            });
            let bridge = !direct
                && l.iter().any(|(k, t)| *k == crate::graph::Kind::File && def_files.iter().any(|f| crate::graph::same_file(t, f)));
            h.trace.graph = if direct { GRAPH_DIRECT_WEIGHT } else if bridge { GRAPH_BRIDGE_WEIGHT } else { 1.0 };
            h.trace.score *= h.trace.graph;
        }
    }
    // Working set: notes about the files this session edited or read rank
    // higher. A smaller factor than the graph's, and only the score moves, so
    // it orders hits inside a confidence quarter and never lifts one over
    // the floor.
    if !opts.working.is_empty() {
        let mut defined: HashMap<String, f32> = HashMap::new();
        for pid in scope.ids() {
            for (path, w) in &opts.working {
                let mut stmt = conn.prepare_cached("SELECT name FROM code_defs WHERE project_id = ?1 AND path = ?2")?;
                for name in stmt.query_map(rusqlite::params![pid, path], |r| r.get::<_, String>(0))? {
                    let e = defined.entry(name?).or_insert(0.0);
                    *e = e.max(*w);
                }
            }
        }
        for h in hits.iter_mut() {
            let Some(l) = links.get(&h.note_id) else { continue };
            let mut best = 0.0f32;
            for (k, t) in l {
                let w = match k {
                    crate::graph::Kind::File => opts.working.iter().filter(|(f, _)| crate::graph::same_file(t, f)).map(|(_, w)| *w).fold(0.0, f32::max),
                    crate::graph::Kind::Symbol => defined.iter().filter(|(n, _)| crate::graph::same_symbol(t, n)).map(|(_, w)| *w).fold(0.0, f32::max),
                    crate::graph::Kind::Note => 0.0,
                };
                best = best.max(w);
            }
            if best > 0.0 {
                h.trace.working = 1.0 + (WORKING_WEIGHT - 1.0) * best;
                h.trace.score *= h.trace.working;
            }
        }
    }
    for h in hits.iter_mut() {
        if h.trace.graph == 0.0 {
            h.trace.graph = 1.0;
        }
        if h.trace.working == 0.0 {
            h.trace.working = 1.0;
        }
    }
    // Order by the number that is printed, in quarters: a 0.44 never sits
    // above a 0.80. Within a quarter the fused score decides, so section cues
    // and graph links still order near-equal hits; then newer first.
    // Without the model, confidence is word overlap, too coarse to order by;
    // the fused score (with section cue and graph) orders alone.
    let bucket = |c: f32| if run_model { (c * 4.0).floor() as i32 } else { 0 };
    hits.sort_by(|a, b| {
        bucket(b.trace.confidence)
            .cmp(&bucket(a.trace.confidence))
            .then(b.trace.score.total_cmp(&a.trace.score))
            .then(b.saved_at.cmp(&a.saved_at))
            .then(a.chunk_id.cmp(&b.chunk_id))
    });
    ex.best_confidence = hits.iter().map(|h| h.trace.confidence).fold(0.0, f32::max);
    // Every scored chunk per note, before the floor, for `parts`.
    let mut by_note: HashMap<i64, Vec<Part>> = HashMap::new();
    for h in &hits {
        let section = h.heading.rsplit(" > ").next().unwrap_or("").to_string();
        by_note.entry(h.note_id).or_default().push(Part { line: h.line, end: h.end_line(), confidence: h.trace.confidence, section });
    }
    let before = hits.len();
    hits.retain(|h| h.trace.confidence >= MIN_CONFIDENCE);
    ex.dropped = before - hits.len();
    // The same fact in a tracker entry and a dated note: keep one, the dated
    // one when both are present (it carries the context the tracker lacks).
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut keep: Vec<Hit> = Vec::with_capacity(hits.len());
    for h in hits {
        let key = dedupe_key(&h.text);
        match seen.get(&key) {
            None => {
                seen.insert(key, keep.len());
                keep.push(h);
            }
            Some(&i) if !is_dated(&keep[i].path) && is_dated(&h.path) => keep[i] = h,
            Some(_) => {}
        }
    }
    hits = keep;
    hits.truncate(opts.k);
    for h in &mut hits {
        if let Some(mut parts) = by_note.get(&h.note_id).cloned() {
            parts.sort_by(|a, b| b.confidence.total_cmp(&a.confidence).then(a.line.cmp(&b.line)));
            // One entry per line: a chunk and its re-scored copy share it,
            // and after the sort they need not be adjacent.
            let mut seen_lines = std::collections::HashSet::new();
            parts.retain(|p| seen_lines.insert(p.line));
            parts.truncate(crate::limits::SEARCH_CHUNKS_PER_NOTE);
            h.parts = parts;
        }
    }
    // Decorate with file links (already fetched for the fused set).
    for h in &mut hits {
        if let Some(l) = links.get(&h.note_id) {
            h.files = l.iter().filter(|(k, _)| *k == crate::graph::Kind::File).map(|(_, t)| t.clone()).collect();
        }
    }
    Ok((hits, ex))
}

/// Top chunk ids by BM25, filtered by project inside the FTS query.
fn bm25_channel(conn: &Connection, scope: &Scope, fts_query: &str) -> Result<Vec<i64>> {
    let sql = format!(
        "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 AND project_id IN {} ORDER BY rank LIMIT {CHANNEL_TOP}",
        scope.placeholders(2)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let binds = std::iter::once(rusqlite::types::Value::Text(fts_query.to_string())).chain(scope.bind());
    let rows = stmt.query_map(params_from_iter(binds), |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub use crate::limits::{COSINE_CEIL, COSINE_FLOOR, GRAPH_BRIDGE_WEIGHT, GRAPH_DIRECT_WEIGHT, MIN_CONFIDENCE, SECTION_CUE_WEIGHT, WORKING_WEIGHT};

/// After ranking, with the checkout at hand: a hit whose note names a symbol
/// whose body changed since the note was saved is labelled and weighted by
/// `CODE_CHANGED_WEIGHT`, like a superseded note. Deterministic: the hash
/// then against the hash now. The search's order is kept.
pub fn mark_code_changed(conn: &Connection, root: &std::path::Path, hits: &mut [Hit]) -> Result<()> {
    let mut cache: HashMap<i64, Vec<String>> = HashMap::new();
    for h in hits.iter_mut() {
        let changed = match cache.get(&h.note_id) {
            Some(c) => c.clone(),
            None => {
                let c = crate::graph::code_changed(conn, h.project_id, root, h.note_id)?;
                cache.insert(h.note_id, c.clone());
                c
            }
        };
        if !changed.is_empty() {
            h.trace.score *= crate::limits::CODE_CHANGED_WEIGHT;
            h.code_changed = changed;
        }
    }
    // Keep the search's order (confidence quarters, then fused score): a
    // stable sort by the weighted score within a quarter only moves a
    // changed-code hit under its unchanged neighbours. Without the model
    // every hit is in one quarter and the score orders alone, as before.
    let bucket = |h: &Hit| if h.trace.cosine.is_some() { (h.trace.confidence * 4.0).floor() as i32 } else { 0 };
    hits.sort_by(|a, b| bucket(b).cmp(&bucket(a)).then(b.trace.score.total_cmp(&a.trace.score)));
    Ok(())
}

/// The words of a prompt that could name a topic: four letters or more, not
/// generic. The recall hook asks nothing of a prompt with fewer than
/// `RECALL_MIN_WORDS` of them.
pub fn content_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = fts::terms(text).into_iter().map(|t| t.to_ascii_lowercase()).filter(|t| t.len() >= 4 && !GENERIC_WORDS.contains(&t.as_str())).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Words that say nothing about which note is meant.
pub const STOPWORDS: &[&str] = &["a", "an", "the", "is", "are", "was", "were", "be", "do", "does", "did", "we", "i", "it", "to", "of", "in", "on", "for", "and", "or", "what", "which", "how", "why", "when", "where", "who", "that", "this", "with", "from", "by", "at", "as", "our", "my", "you", "me", "us", "any", "about", "have", "has", "had", "not", "no", "can", "could", "should", "would", "will"];

fn is_stopword(t: &str) -> bool {
    t.len() < 3 || STOPWORDS.contains(&t.to_ascii_lowercase().as_str())
}

/// Share of the query's content terms (lowercased) found in `text`.
/// The question's rare words: content terms found in at most
/// `DISTINCTIVE_MAX_SHARE` of the scope's chunks (one FTS count each, under
/// a millisecond). Empty when nothing is rare, so the gate stays off.
fn distinctive_terms(conn: &Connection, scope: &Scope, terms: &[String], acronyms: &[String]) -> Result<Vec<String>> {
    if terms.len() < 2 {
        return Ok(Vec::new());
    }
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM chunks WHERE project_id IN {}", scope.placeholders(1)),
        rusqlite::params_from_iter(scope.ids()),
        |r| r.get(0),
    )?;
    if total < 10 {
        return Ok(Vec::new());
    }
    let limit = ((total as f32) * crate::limits::DISTINCTIVE_MAX_SHARE).max(1.0) as i64;
    let mut out = Vec::new();
    let mut stmt = conn.prepare_cached("SELECT count(*) FROM chunks_fts WHERE chunks_fts MATCH ?1")?;
    for t in terms {
        if (t.len() < 4 && !acronyms.contains(t)) || GENERIC_WORDS.contains(&t.as_str()) {
            continue;
        }
        let q = format!("\"{}\"", t.replace('"', ""));
        let df: i64 = stmt.query_row([q], |r| r.get(0)).unwrap_or(0);
        // Absent from the store entirely is the rarest of all: the question
        // names a thing no note covers.
        if df <= limit {
            out.push(t.clone());
        }
    }
    Ok(out)
}

/// Everyday words that are rare in a small store without being distinctive:
/// a question's verbs and quantifiers. Never counted as rare.
const GENERIC_WORDS: &[&str] = &[
    "reject", "rejected", "tried", "decide", "decided", "decision", "measured", "level", "about", "above", "after", "again", "against", "along", "already", "always", "among", "another", "around", "because", "before", "behind", "being", "below", "between", "both", "called", "cannot", "changed", "changes", "check", "comes", "could", "doing", "during", "each", "either", "enough", "every", "exactly", "first", "found", "getting", "given", "gives", "going", "happen", "happens", "having", "inside", "instead", "keep", "keeps", "kept", "large", "last", "later", "least", "less", "little", "longer", "made", "make", "makes", "making", "many", "maybe", "might", "more", "most", "much", "needs", "never", "newer", "next", "nothing", "often", "older", "other", "others", "outside", "over", "part", "perhaps", "place", "please", "quite", "rather", "reach", "reached", "really", "remove", "removed", "rendered", "returns", "right", "same", "second", "seems", "several", "shall", "should", "since", "small", "some", "something", "still", "such", "take", "taken", "takes", "than", "their", "them", "then", "there", "these", "they", "thing", "things", "think", "those", "though", "through", "times", "today", "under", "until", "upon", "used", "uses", "using", "usually", "want", "wanted", "well", "went", "were", "what", "whatever", "where", "whether", "while", "within", "without", "work", "worked", "working", "works", "would", "years",
];

fn coverage(terms: &[String], text: &str) -> f32 {
    if terms.is_empty() {
        return 0.0;
    }
    let lower = text.to_ascii_lowercase();
    let found = terms.iter().filter(|t| lower.contains(stem(t))).count();
    found as f32 / terms.len() as f32
}

/// A word without its common English suffix, so "collapsed" is found in
/// "collapse" and "reindexing" in "reindex". Never shorter than four letters.
pub fn stem(word: &str) -> &str {
    for suf in ["ing", "ies", "ed", "es", "ly", "s"] {
        if let Some(base) = word.strip_suffix(suf) {
            if base.len() >= 4 {
                return base;
            }
        }
    }
    word
}

/// Which section a question is really after, from its wording: numbers want
/// Measured, "why" wants Decision, "tried" wants Rejected. None when unsure.
pub fn section_cue(query: &str) -> Option<&'static str> {
    let q = query.to_ascii_lowercase();
    let has = |ws: &[&str]| ws.iter().any(|w| q.contains(w));
    if has(&["how much", "how many", "how long", "how fast", " ms", " mb", " kb", "latency", "budget", "measured", "number", "percent", "faster", "slower", "size", "fit"]) || q.chars().any(|c| c.is_ascii_digit()) {
        Some("measured")
    } else if has(&["reject", "tried", "instead of", "dropped", "abandon", "why not", "alternative", "didn't work", "did not work"]) {
        Some("rejected")
    } else if has(&["open question", "what remains", "still open", "unfinished", "todo", "next step", "left to do"]) {
        Some("open")
    } else if has(&["why", "decide", "decision", "chose", "chosen", "reason"]) {
        Some("decision")
    } else {
        None
    }
}

/// Normalised head of a chunk: same first 160 letters means the same fact.
/// A tracker id in front (`B-7 `) is not part of the fact.
fn dedupe_key(text: &str) -> String {
    let body = match crate::chunk::entry_id(text) {
        Some(id) => text.split_once(id).map(|(_, rest)| rest).unwrap_or(text),
        None => text,
    };
    body.chars().filter(|c| c.is_alphanumeric()).map(|c| c.to_ascii_lowercase()).take(160).collect()
}

/// A dated note (`YYYY-MM-DD-topic.md`) rather than a living tracker.
fn is_dated(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.len() > 10 && name.as_bytes()[4] == b'-' && name.as_bytes()[7] == b'-' && name[..4].chars().all(|c| c.is_ascii_digit())
}

/// The sentence of `text` holding most query terms, with one neighbour each
/// side, and its line offset inside the chunk. The whole text when short.
pub fn focus(text: &str, query: &str, max_bytes: usize) -> (String, u32) {
    if text.len() <= max_bytes {
        return (text.to_string(), 0);
    }
    let terms: Vec<String> = fts::terms(query).iter().map(|t| t.to_ascii_lowercase()).collect();
    let sents: Vec<&str> = text.split_inclusive(['.', '\n', '!', '?']).collect();
    let mut best = 0;
    let mut best_n = 0;
    for (i, s) in sents.iter().enumerate() {
        let l = s.to_ascii_lowercase();
        let n = terms.iter().filter(|t| l.contains(t.as_str())).count();
        if n > best_n {
            best_n = n;
            best = i;
        }
    }
    let lo = best.saturating_sub(1);
    let hi = (best + 2).min(sents.len());
    let mut out: String = sents[lo..hi].concat().trim().to_string();
    if out.len() > max_bytes {
        let mut end = max_bytes;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    let line = sents[..lo].iter().map(|s| s.matches('\n').count() as u32).sum();
    (out, line)
}

/// Identifier-looking query terms that are rare in the scope: at most
/// `DISTINCTIVE_MAX_SHARE` of its chunks hold them. A common one is the
/// project's own vocabulary (its name, a core type): "AbhiGram" sits in
/// every note of AbhiGram, so finding it verbatim proves nothing, and as a
/// proof it had recall print three unrelated notes at confidence 1.00.
/// Under ten chunks every identifier counts, as for distinctive words.
fn rare_identifiers(conn: &Connection, scope: &Scope, query: &str) -> Result<Vec<String>> {
    let idents: Vec<String> = fts::terms(query).iter().filter(|t| fts::is_identifier(t)).map(|t| t.to_string()).collect();
    if idents.is_empty() {
        return Ok(idents);
    }
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM chunks WHERE project_id IN {}", scope.placeholders(1)),
        rusqlite::params_from_iter(scope.ids()),
        |r| r.get(0),
    )?;
    if total < 10 {
        return Ok(idents);
    }
    let limit = ((total as f32) * crate::limits::DISTINCTIVE_MAX_SHARE).max(1.0) as i64;
    let sql = format!(
        "SELECT count(*) FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid WHERE chunks_fts MATCH ?1 AND c.project_id IN {}",
        scope.placeholders(2)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut out = Vec::with_capacity(idents.len());
    for t in idents {
        let q = format!("\"{}\"", t.replace('"', ""));
        let df: i64 = stmt.query_row(rusqlite::params_from_iter(std::iter::once(rusqlite::types::Value::from(q)).chain(scope.ids().map(rusqlite::types::Value::from))), |r| r.get(0)).unwrap_or(0);
        if df <= limit {
            out.push(t);
        }
    }
    Ok(out)
}

/// True when one of the question's rare identifiers appears verbatim in the chunk.
fn exact_identifier_hit(conn: &Connection, idents: &[String], chunk_id: i64) -> Result<bool> {
    if idents.is_empty() {
        return Ok(false);
    }
    let text: String = conn.query_row("SELECT text FROM chunks WHERE id = ?1", [chunk_id], |r| r.get(0))?;
    Ok(idents.iter().any(|t| text.contains(t.as_str())))
}

/// A plain scan over the scope's vectors in `space`, keeping the top 100 by
/// dot product (vectors are unit length, so this is cosine).
/// Nearest chunks to `qvec` in scope. Two paths: an exact f16 scan for small
/// scopes, and for larger ones a bit-packed prefilter (one sign bit per
/// dimension, 64 bytes per vector, one contiguous blob per project) scored
/// by XOR + popcount, then an exact rerank of the best candidates. Returns
/// the ids and how many vectors the prefilter examined.
fn vector_channel(conn: &Connection, scope: &Scope, space: &Space, qvec: &[f32]) -> Result<(Vec<(f32, i64)>, usize)> {
    let dims = space.def.dims;
    let bytes_per = dims.div_ceil(8);
    let force_exact = std::env::var_os("MEMORYCASTLE_NO_BITS").is_some();
    // Gather the packed blobs for every project in scope, building missing ones.
    let mut packs: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(scope.members.len());
    let mut total = 0usize;
    if !force_exact {
        for &(pid, _) in &scope.members {
            let row: Option<(i64, Vec<u8>, Vec<u8>)> = conn
                .query_row(
                    "SELECT n, ids, bits FROM vector_bits WHERE space_id = ?1 AND project_id = ?2",
                    params![space.id, pid],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let (n, ids, bits) = match row {
                Some(r) => r,
                None => build_bits(conn, space, pid, dims)?,
            };
            total += n as usize;
            packs.push((ids, bits));
        }
    }
    if force_exact || total < BITS_MIN_ROWS {
        return Ok((exact_scan(conn, scope, space, qvec)?, 0));
    }
    // Query sign bits, as u64 words.
    let words = bytes_per.div_ceil(8);
    let mut q = vec![0u64; words];
    for (i, &x) in qvec.iter().enumerate() {
        if x > 0.0 {
            q[i / 64] |= 1 << (i % 64);
        }
    }
    // Hamming pass: keep the best BITS_CANDIDATES (smallest distance).
    let mut cand: Vec<(u32, i64)> = Vec::with_capacity(BITS_CANDIDATES + 1);
    let mut worst = u32::MAX;
    let mut row = vec![0u64; words];
    for (ids, bits) in &packs {
        let n = ids.len() / 8;
        for i in 0..n {
            let b = &bits[i * bytes_per..(i + 1) * bytes_per];
            for (w, chunk) in row.iter_mut().zip(b.chunks(8)) {
                let mut arr = [0u8; 8];
                arr[..chunk.len()].copy_from_slice(chunk);
                *w = u64::from_le_bytes(arr);
            }
            let d: u32 = row.iter().zip(&q).map(|(a, b)| (a ^ b).count_ones()).sum();
            if cand.len() < BITS_CANDIDATES || d < worst {
                let id = i64::from_le_bytes(ids[i * 8..(i + 1) * 8].try_into().unwrap());
                let pos = cand.partition_point(|(x, _)| *x <= d);
                cand.insert(pos, (d, id));
                if cand.len() > BITS_CANDIDATES {
                    cand.pop();
                }
                if cand.len() == BITS_CANDIDATES {
                    worst = cand[BITS_CANDIDATES - 1].0;
                }
            }
        }
    }
    // Exact rerank of the candidates: primary-key lookups, f16 dot.
    let mut stmt = conn.prepare_cached("SELECT vec FROM vectors WHERE space_id = ?1 AND chunk_id = ?2")?;
    let mut u16s: Vec<u16> = vec![0; dims];
    let mut floats: Vec<f32> = vec![0.0; dims];
    let mut top: Vec<(f32, i64)> = Vec::with_capacity(cand.len());
    for (_, id) in &cand {
        let blob: Vec<u8> = match stmt.query_row(params![space.id, id], |r| r.get(0)).optional()? {
            Some(b) => b,
            None => continue,
        };
        if blob.len() != dims * 2 {
            continue;
        }
        top.push((dot_f16(&blob, qvec, &mut u16s, &mut floats), *id));
    }
    top.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    top.truncate(CHANNEL_TOP);
    Ok((top, total))
}

/// Packs the sign bits of every vector of `project_id` into one blob and
/// stores it. One scan, same cost as a single exact search.
fn build_bits(conn: &Connection, space: &Space, project_id: i64, dims: usize) -> Result<(i64, Vec<u8>, Vec<u8>)> {
    let bytes_per = dims.div_ceil(8);
    let mut stmt = conn.prepare_cached("SELECT chunk_id, vec FROM vectors WHERE space_id = ?1 AND project_id = ?2 ORDER BY chunk_id")?;
    let mut rows = stmt.query(params![space.id, project_id])?;
    let mut ids: Vec<u8> = Vec::new();
    let mut bits: Vec<u8> = Vec::new();
    let mut n = 0i64;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let blob = r.get_ref(1)?.as_blob().map_err(rusqlite::Error::from)?;
        if blob.len() != dims * 2 {
            continue;
        }
        let start = bits.len();
        bits.resize(start + bytes_per, 0);
        for (i, pair) in blob.chunks_exact(2).enumerate() {
            // Sign bit of the f16 is the top bit of the second byte; positive and non-zero.
            let v = u16::from_le_bytes([pair[0], pair[1]]);
            if v & 0x8000 == 0 && v != 0 {
                bits[start + i / 8] |= 1 << (i % 8);
            }
        }
        ids.extend_from_slice(&id.to_le_bytes());
        n += 1;
    }
    if !crate::db::is_readonly(conn) {
        conn.execute(
            "INSERT OR REPLACE INTO vector_bits(space_id, project_id, n, ids, bits) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![space.id, project_id, n, ids, bits],
        )?;
    }
    Ok((n, ids, bits))
}

/// Drops the packed bits of a project; rebuilt on the next meaning search.
pub fn invalidate_bits(conn: &Connection, project_id: i64) -> Result<()> {
    conn.execute("DELETE FROM vector_bits WHERE project_id = ?1", [project_id])?;
    Ok(())
}

fn exact_scan(conn: &Connection, scope: &Scope, space: &Space, qvec: &[f32]) -> Result<Vec<(f32, i64)>> {
    let sql = format!(
        "SELECT chunk_id, vec FROM vectors WHERE space_id = ?1 AND project_id IN {}",
        scope.placeholders(2)
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let binds = std::iter::once(rusqlite::types::Value::Integer(space.id)).chain(scope.bind());
    let mut rows = stmt.query(params_from_iter(binds))?;
    let dims = space.def.dims;
    let mut top: Vec<(f32, i64)> = Vec::with_capacity(CHANNEL_TOP + 1);
    let mut threshold = f32::NEG_INFINITY;
    // Scratch for one row: the blob as aligned u16, then as f32 (SIMD convert).
    let mut bits: Vec<u16> = vec![0; dims];
    let mut floats: Vec<f32> = vec![0.0; dims];
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let blob = r.get_ref(1)?.as_blob().map_err(rusqlite::Error::from)?;
        if blob.len() != dims * 2 {
            continue;
        }
        let score = dot_f16(blob, qvec, &mut bits, &mut floats);
        if top.len() < CHANNEL_TOP || score > threshold {
            let pos = top.partition_point(|(s, _)| *s > score);
            top.insert(pos, (score, id));
            if top.len() > CHANNEL_TOP {
                top.pop();
            }
            if top.len() == CHANNEL_TOP {
                threshold = top[CHANNEL_TOP - 1].0;
            }
        }
    }
    Ok(top)
}

/// Dot product of a little-endian f16 blob with an f32 query. The blob is
/// copied into aligned u16s (a memcpy), converted with the crate's vectorised
/// path, then multiplied in four independent accumulators.
#[inline]
fn dot_f16(blob: &[u8], q: &[f32], bits: &mut [u16], floats: &mut [f32]) -> f32 {
    use half::slice::{HalfBitsSliceExt, HalfFloatSliceExt};
    for (b, pair) in bits.iter_mut().zip(blob.chunks_exact(2)) {
        *b = u16::from_le_bytes([pair[0], pair[1]]);
    }
    bits.reinterpret_cast::<half::f16>().convert_to_f32_slice(floats);
    let mut acc = [0.0f32; 4];
    for (f, x) in floats.chunks_exact(4).zip(q.chunks_exact(4)) {
        acc[0] += f[0] * x[0];
        acc[1] += f[1] * x[1];
        acc[2] += f[2] * x[2];
        acc[3] += f[3] * x[3];
    }
    acc[0] + acc[1] + acc[2] + acc[3]
}

fn fetch_hits(conn: &Connection, ids: &[i64]) -> Result<Vec<Hit>> {
    let sql = format!(
        "SELECT c.id, c.note_id, c.project_id, p.name, n.path, c.heading_path, n.saved_at, n.source, c.text, n.superseded_by, c.line, n.dated
         FROM chunks c JOIN notes n ON n.id = c.note_id JOIN projects p ON p.id = c.project_id
         WHERE c.id IN {}",
        crate::db::placeholders(1, ids.len())
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(ids.iter()), |r| {
        Ok(Hit {
            chunk_id: r.get(0)?,
            note_id: r.get(1)?,
            project_id: r.get(2)?,
            project: r.get(3)?,
            path: r.get(4)?,
            heading: r.get(5)?,
            saved_at: r.get(6)?,
            dated: r.get(11)?,
            code_changed: Vec::new(),
            parts: Vec::new(),
            source: r.get(7)?,
            text: r.get(8)?,
            superseded_by: r.get(9)?,
            line: r.get::<_, i64>(10)? as u32,
            files: Vec::new(),
            trace: Trace::default(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The store as a [`Memory`], with an optional warm embedder.
pub struct StoreMemory<'a> {
    pub conn: &'a Connection,
    pub embedder: Option<&'a mut (dyn Embedder + 'static)>,
}

impl Memory for StoreMemory<'_> {
    fn search(&mut self, scope: &Scope, query: &str, opts: &SearchOptions) -> Result<Vec<Hit>> {
        let model = match self.embedder.as_deref_mut() {
            Some(e) => ModelSource::Warm(e),
            None => ModelSource::None,
        };
        Ok(search(self.conn, scope, query, opts, model)?.0)
    }
}

/// The `rg` bar: ranks notes by how many distinct query terms they contain,
/// case-insensitively. No index, no model. The first thing to beat.
pub struct GrepMemory<'a> {
    pub conn: &'a Connection,
}

impl Memory for GrepMemory<'_> {
    fn search(&mut self, scope: &Scope, query: &str, opts: &SearchOptions) -> Result<Vec<Hit>> {
        let terms: Vec<String> = fts::terms(query).iter().map(|t| t.to_lowercase()).collect();
        let sql = format!(
            "SELECT n.id, n.project_id, p.name, n.path, n.saved_at, n.source, n.content
             FROM notes n JOIN projects p ON p.id = n.project_id
             WHERE n.deleted_at IS NULL AND n.project_id IN {}",
            scope.placeholders(1)
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut scored: Vec<(usize, Hit)> = Vec::new();
        let rows = stmt.query_map(params_from_iter(scope.bind()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        for row in rows {
            let (id, pid, pname, path, saved_at, source, content) = row?;
            let lower = content.to_lowercase();
            let n = terms.iter().filter(|t| lower.contains(t.as_str())).count();
            if n > 0 {
                scored.push((
                    n,
                    Hit {
                        chunk_id: id,
                        note_id: id,
                        project_id: pid,
                        project: pname,
                        path,
                        heading: String::new(),
                        saved_at,
                        dated: None,
                        source,
                        superseded_by: None,
                        files: Vec::new(),
                        text: String::new(),
                        line: 1,
                        trace: Trace { score: n as f32, weight: 1.0, ..Default::default() },
                        code_changed: Vec::new(),
                        parts: Vec::new(),
                    },
                ));
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.note_id.cmp(&b.1.note_id)));
        Ok(scored.into_iter().take(opts.k).map(|(_, h)| h).collect())
    }
}
