//! Every bound that shapes memory, in one place, each with the bytes it
//! controls. Nothing in the crate grows without a constant here. Change a
//! number, re-run `cargo test limits` and the benchmarks, then update the
//! budget table in CLAUDE.md.

// ---------------------------------------------------------------- notes --

/// Words per chunk the splitter aims for. Sets tokens per model call and so
/// the activation peak (attention grows with the square of the token count).
pub const CHUNK_TARGET_WORDS: usize = 200;
/// Hard cap on words per chunk; longer runs are split on lines, then words.
pub const CHUNK_MAX_WORDS: usize = 280;
/// Tokens the embedder will accept for one chunk; longer input is truncated.
/// 280 identifier-heavy words measured at about 600 tokens, so 1024 leaves room.
pub const EMBED_MAX_TOKENS: usize = 1024;
/// Symbol edges one note may contribute to `links`. Bounds the graph row count
/// per note (file and note edges are bounded by the note's own header lines).
pub const GRAPH_MAX_SYMBOLS_PER_NOTE: usize = 64;
/// Header lines scanned for `Author:`, `Files:`, `Supersedes:`.
pub const HEADER_LINES: usize = 20;
/// Lines of each Decision / Rejected / Measured section printed by `search --sections`,
/// so an agent reads the answer instead of the whole note.
pub const SECTION_LINES: usize = 30;
/// Bytes of chunk text printed per note in search output: the best chunk, whole
/// when it fits, so the agent does not open the file for the rest.
pub const SEARCH_CHUNK_BYTES: usize = 1200;
/// Total bytes one `search` call prints (about 1.5k tokens): one call is meant
/// to be enough, so the agent stops searching and answers.
pub const SEARCH_OUTPUT_BYTES: usize = 6000;
/// Chunks of one note shown in a search block (best ranked first).
pub const SEARCH_CHUNKS_PER_NOTE: usize = 3;
/// Relevance. Confidence is the best of a rescaled cosine and term coverage;
/// a hit under `MIN_CONFIDENCE` is not shown and "no relevant note" is said.
/// Cosine rescale, measured on the gold set (BENCHMARKS.md): off-topic
/// questions reach 0.54 to 0.65 against their best chunk, the right note
/// scores 0.67 to 0.83. Floor 0.60 and ceiling 0.80 map that to 0 to 1;
/// 0.30 sits between the two populations. Coverage decides only when the
/// model did not run, or when an identifier in the question is in the chunk.
pub const COSINE_FLOOR: f32 = 0.60;
pub const COSINE_CEIL: f32 = 0.80;
pub const MIN_CONFIDENCE: f32 = 0.30;
/// A chunk whose heading matches what the question asks for (Measured for a
/// number question) gets this on its score.
pub const SECTION_CUE_WEIGHT: f32 = 1.6;
/// Cosine confidence is multiplied by this when the chunk holds none of the
/// question's content words: a near miss in the same area. 0.80 becomes
/// 0.28, under `MIN_CONFIDENCE`; 0.90 stays.
pub const NO_COVERAGE_FACTOR: f32 = 0.3;
/// A question word is distinctive when at most this share of the scope's
/// chunks contain it. Such words must appear in a hit for the model's
/// cosine to count in full.
pub const DISTINCTIVE_MAX_SHARE: f32 = 0.1;
/// Graph rerank: a note that links a file or symbol the question names is
/// about it, whatever the words say. Bridge: the question's symbol is
/// defined (code index) in a file the note links.
pub const GRAPH_DIRECT_WEIGHT: f32 = 1.5;
pub const GRAPH_BRIDGE_WEIGHT: f32 = 1.2;
/// A note naming a symbol whose definition body changed since the note was
/// saved (`note_code`, schema v20) is labelled `code changed` and its score
/// multiplied by this: still found, ranked under a note the code still
/// matches, and the agent told to check it against the lines.
pub const CODE_CHANGED_WEIGHT: f32 = 0.7;
/// Recall on every prompt (the `UserPromptSubmit` hook): a prompt with fewer
/// content words than this asks nothing; a hit under this confidence is not
/// injected; at most this many notes, each once per session. About 15
/// tokens per line injected, zero when the store has nothing.
pub const RECALL_MIN_WORDS: usize = 3;
/// 0.50 is cosine 0.70: above the off-topic peak (0.65) with margin, and
/// under the right note's median (0.75), so a task stated in the agent's
/// words still recalls the note that rejected its approach.
pub const RECALL_MIN_CONFIDENCE: f32 = 0.50;
pub const RECALL_MAX_NOTES: usize = 3;
/// Mid-session reminder (the same prompt hook): when this many files were
/// edited since the last note, or the oldest such edit is this old, one
/// `note owed` line is printed, then not again for the cooldown. Rare and
/// true, so the agent does not learn to skip it.
pub const OWED_REMIND_FILES: usize = 5;
pub const OWED_REMIND_SECS: i64 = 30 * 60;
pub const OWED_REMIND_COOLDOWN_SECS: i64 = 30 * 60;
/// Code spans printed per note in `search --sections` (symbols the note names).
pub const SEARCH_CODE_SPANS: usize = 4;
/// Files listed per hit in search output; the rest is "+n more".
pub const SEARCH_FILES_SHOWN: usize = 4;
/// Ranked `read:` lines printed after search hits: the spans to read first.
pub const READ_SPANS: usize = 5;
/// When the best span scores at least `SPAN_STRONG` (a definition the
/// question or a top note names), spans under `SPAN_WEAK` (name-contains
/// guesses at 0.5) are dropped: measured, 5 of 9 guided reads were followed
/// by a whole-file read, and the guesses were half the printed lines.
pub const SPAN_STRONG: f32 = 0.85;
pub const SPAN_WEAK: f32 = 0.55;
/// Sessions the "last n sessions" observed line sums over (the ones with
/// the most recent reads, this one included).
pub const RECENT_SESSIONS: usize = 7;
/// The controlled A/B (same questions, memory vs grep agent, Codex,
/// 2026-10-18, AbhiGram): the one figure with no counterfactual guess.
pub const BENCHMARK_LINE: &str = "benchmark (controlled A/B, Codex on AbhiGram, 2026-10-18): retrieval tokens 63% less on average (44%, 55%, 91%), whole run 32% to 53% less; Antigravity six-turn sessions 34% less cost";
/// Constants and callers listed per definition by `links --context`.
pub const CONTEXT_ITEMS: usize = 4;
/// Lines of context either side of a `uses` line in `read:` output.
pub const USE_CONTEXT_LINES: u32 = 3;
/// Bytes per token used to report savings, measured with the Gemma tokenizer
/// (`examples/tok_ratio.rs`): journal prose 3.6, Rust 3.0, Kotlin 3.25. One
/// fixed divisor keeps the keyword path free of any tokenizer and the number
/// the same on every machine.
pub const BYTES_PER_TOKEN: f64 = 3.2;
/// Most bytes one file counts for in the savings ledger. Agents do not read a
/// 400 KB file whole (Claude Code's Read stops at 2,000 lines; Codex reaches
/// for `sed -n`), so a span printed for a huge file stands in for this much.
pub const USAGE_FILE_CAP: usize = 32 * 1024;
/// A file read within this many seconds after a span was printed for it
/// counts as a read guided by memorycastle.
pub const SPAN_FOLLOW_SECS: i64 = 600;
/// Lines an agent reads of a file it opens whole: Claude Code's Read tool
/// returns at most this many and says so; beyond it the agent greps and
/// reads a range. The observed ceiling counts a guided read's whole file up
/// to this many lines plus one `RANGE_READ_LINES` range, never more: no
/// agent pages through 4,500 lines, so counting them would be invention.
pub const WHOLE_READ_LINES: usize = 2000;
/// Conservative baseline for a guided read: without the span the agent would
/// have grepped and then read about this many lines around the hit, not the
/// whole file. The floor of the reported range uses it; the ceiling uses the
/// whole file.
pub const RANGE_READ_LINES: usize = 40;
/// A `## Decision` shorter than this is a label, not a decision; `save` warns.
pub const DECISION_MIN_WORDS: usize = 8;
/// A `## Decision` longer than this, or with more bullets than
/// `DECISION_MAX_BULLETS`, holds several decisions; one chunk embeds one
/// idea cleanly, so `save` says to split.
pub const DECISION_MAX_WORDS: usize = 120;
pub const DECISION_MAX_BULLETS: usize = 3;
/// Title words that name a task, not a decision. The title is embedded
/// with every chunk, so "Expert review batch" tells the model nothing.
pub const TASK_TITLE_WORDS: &[&str] = &["batch", "review", "misc", "various", "changes", "fixes", "cleanup", "wip"];
/// Notes a store needs before the embedding quality percentiles mean
/// anything; under it, numbers are printed without verdicts.
pub const QUALITY_MIN_NOTES: usize = 30;
/// Percentile bounds for the soft quality scores: a note in the weakest
/// `QUALITY_LOW_PCT` of spread is flagged, a chunk above the
/// `QUALITY_HIGH_PCT` of nearest-neighbour cosine is a duplicate.
pub const QUALITY_LOW_PCT: f64 = 0.20;
pub const QUALITY_HIGH_PCT: f64 = 0.95;
/// `quality` compares every chunk pair up to this many chunks (6.5 M dot
/// products at 3,619, about a second). Above it only the
/// `QUALITY_RECENT_NOTES` most recently saved notes are scored, each
/// against the whole store; `save` scores just the saved note the same way.
pub const QUALITY_MAX_CHUNKS: usize = 5000;
pub const QUALITY_RECENT_NOTES: usize = 200;
/// An open (sections or the file after a served first line) counts only
/// this many seconds after the serve: an agent takes longer than this to
/// read a line and make the next call, a benchmark loop does not.
pub const OPEN_MIN_SECS: i64 = 3;
/// A note served this many times and never opened (sections, links or
/// the file) is not earning its place; `quality` lists it.
pub const SERVED_NEVER_FOLLOWED: i64 = 5;
/// A note longer than this is a dump, not a memory; `save` says so.
pub const NOTE_MAX_WORDS: usize = 1500;
const _: () = { assert!(SEARCH_CHUNK_BYTES * 3 < SEARCH_OUTPUT_BYTES); assert!(NOTE_MAX_WORDS > CHUNK_MAX_WORDS * 3); assert!(DECISION_MIN_WORDS < DECISION_MAX_WORDS); assert!(DECISION_MAX_WORDS <= CHUNK_TARGET_WORDS, "a decision must fit one chunk"); };

// --------------------------------------------------------------- search --

/// Hits each channel (BM25, vector) contributes to fusion. Working set of a
/// search: 2 × this × (score, id) = 3.2 KB before hit text is fetched.
pub const CHANNEL_TOP: usize = 100;
/// Vectors in scope below which the exact f16 scan runs; above it the bit
/// prefilter runs first. At 512 rows the exact scan is under 0.5 ms.
pub const BITS_MIN_ROWS: usize = 512;
/// Candidates the Hamming pass keeps for exact rerank. Each costs one
/// primary-key lookup (about 3 µs) and one 1 KB blob read.
pub const BITS_CANDIDATES: usize = 300;
/// Bytes per vector in the bit pack: one sign bit per dimension.
pub const fn bits_bytes_per_vector(dims: usize) -> usize {
    dims.div_ceil(8)
}
/// Resident bytes of one project's bit pack held during a search:
/// ids (8 B) plus sign bits per vector. 50,000 chunks of 512 d = 3.6 MB.
pub const fn bits_pack_bytes(vectors: usize, dims: usize) -> usize {
    vectors * (8 + bits_bytes_per_vector(dims))
}
/// Query vectors remembered per space, evicted oldest first. Each row holds
/// the query text and dims × 2 bytes: 256 × (1 KB + text) is under 0.5 MB.
pub const QUERY_CACHE_MAX: i64 = 256;
/// Reciprocal rank fusion constant; rank 1 scores 1/61.
pub const RRF_K: f32 = 60.0;
/// Weight of a note that a newer note superseded.
pub const SUPERSEDED_WEIGHT: f32 = 0.3;
/// Weight of a sibling project's hits under `--related`.
pub const SIBLING_WEIGHT: f32 = 0.6;

// --------------------------------------------------------------- model --

/// Embedding dimensions kept after Matryoshka truncation: 512 × 2 B = 1 KB
/// per chunk on disk, 1 KB moved per exact comparison.
pub const EMBED_DIMS: usize = 512;
/// Intra-op threads for ONNX Runtime. Four: a query is 95 ms against 135 ms
/// at two, reindex wall time is the same.
pub const ORT_THREADS: usize = 4;
/// Chunks embedded per model call. One: batching bought no throughput on the
/// CPU and peak memory grew linearly with the batch.
pub const EMBED_BATCH: usize = 1;
/// Seconds the MCP server and `serve` keep the model loaded after the last
/// request before releasing its 270 MB.
pub const IDLE_UNLOAD_SECS: u64 = 300;

// ------------------------------------------------------------- budgets --

/// Peak resident memory the keyword path may reach, from the measured 8 MB at
/// 3,619 chunks. Gate in `memorycastle eval`.
pub const BUDGET_KEYWORD_RSS_MB: u64 = 30;
/// Peak resident memory of a meaning search with the model loaded.
pub const BUDGET_MEANING_RSS_MB: u64 = 300;
/// Peak resident memory of a reindex (activations dominate).
pub const BUDGET_REINDEX_RSS_MB: u64 = 700;

// Relations between the bounds, proven at compile time.
const _: () = {
    assert!(CHUNK_TARGET_WORDS < CHUNK_MAX_WORDS);
    assert!(CHUNK_MAX_WORDS * 3 < EMBED_MAX_TOKENS, "max words at about 3 tokens each must fit the token cap");
    assert!(BITS_MIN_ROWS > BITS_CANDIDATES, "the prefilter must drop rows to be worth running");
    assert!(BITS_CANDIDATES * EMBED_DIMS * 2 < 1024 * 1024, "exact rerank moves under 1 MB");
    // Scratch per search, independent of store size: two channel lists, the
    // candidate list, one f16 row, the query vector and its f32 scratch.
    assert!(2 * CHANNEL_TOP * 16 + BITS_CANDIDATES * 16 + EMBED_DIMS * 2 + EMBED_DIMS * 4 * 2 < 16 * 1024);
    // The bit pack scales at 72 bytes per vector: 50k chunks under 4 MB, a million under 80 MB.
    assert!(bits_pack_bytes(1, EMBED_DIMS) == 72);
    assert!(bits_pack_bytes(50_000, EMBED_DIMS) < 4 * 1024 * 1024);
    assert!(bits_pack_bytes(1_000_000, EMBED_DIMS) < 80 * 1024 * 1024);
};

/// Files looked up after one shell command that changed them (`memcastle
/// changed`): more than this on the first call of a session is a dirty
/// checkout, not the command's work, and is skipped.
pub const CHANGED_MAX_FILES: usize = 3;
/// Characters of the Decision sentence printed under a recall line.
pub const RECALL_DECISION_CHARS: usize = 220;
