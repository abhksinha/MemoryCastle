//! Every bound that shapes memory, in one place, each with the bytes it
//! controls. Nothing in the crate grows without a constant here. Change a
//! number, re-run `cargo test limits` and the benchmarks, then update the
//! budget table in CLAUDE.md.

// ---------------------------------------------------------------- notes --

/// Words per chunk the splitter aims for. Small chunks embed one point each:
/// on four gold sets (171 answerable questions, 60 of them written blind)
/// 60 words found 162 in the top 3 against 153 at 200, with 40, 80 and 100
/// lower and absent questions unchanged (2026-10-22). It also bounds tokens
/// per model call, so the activation peak.
pub const CHUNK_TARGET_WORDS: usize = 60;
/// Hard cap on words per chunk; longer runs are split on lines, then words.
pub const CHUNK_MAX_WORDS: usize = 90;
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
/// A note at least this long gets its printed line ranges recorded as served
/// spans, so a whole read of it is stopped once with the ranges. Shorter
/// notes are read whole: the median note is 2 KB, and a stopped read costs a
/// turn that a few hundred bytes do not pay for.
pub const NOTE_GUARD_MIN_BYTES: usize = 3000;
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
/// The floor when the model ran. The false answers to absent questions sat
/// at 0.33 to 0.35 (2026-10-23): at 0.36 they went from 8 of 44 to 2, notes
/// printed fell 14%, and with one `--also` wording right answers rose (164 to
/// 166 and 163 to 165 of 187); without it they fell 155 to 150. Keyword-only
/// confidence is a share of words (one of three is 0.33), so it keeps 0.30.
pub const MIN_CONFIDENCE_MODEL: f32 = 0.36;
/// A chunk whose heading matches what the question asks for (Measured for a
/// number question) gets this on its score.
pub const SECTION_CUE_WEIGHT: f32 = 1.6;
/// Cosine confidence is multiplied by this when the chunk holds none of the
/// question's content words: a near miss in the same area. 0.80 becomes
/// 0.28, under `MIN_CONFIDENCE`; 0.90 stays.
pub const NO_COVERAGE_FACTOR: f32 = 0.3;
/// Other wordings of a question a search takes (`--also`). One was measured
/// (2026-10-23): blind rewordings by another model, merged per note, gained
/// 3 and 6 of 187 gold questions on two samples with no absent question
/// answered; two gained more on blind questions but answered two absent ones
/// and lost four on rushtex.
pub const SEARCH_ALSO_MAX: usize = 2;
/// A learned question (`learned.rs`) brings its note in when the new
/// question is at least this close. Measured on the gold sets (2026-10-23):
/// a question and a rewording of it, median 0.88; the best question about a
/// different note, 90th percentile 0.79, none at 0.86; absent questions to
/// any answerable one, at most 0.69. At 0.86, 60% of rewordings match.
pub const LEARNED_MIN_COSINE: f32 = 0.86;
/// Sign bits that may differ before a learned question's exact cosine is
/// computed (512 dims). Every gold question pair at cosine 0.86 or more
/// differed in at most 133; 0.5% of all pairs are within 140.
pub const LEARNED_BITS_MAX: u32 = 140;
/// A code file read or edited after a search credits a printed note only
/// when it is at least this specific: `ln(notes / reach) / ln(notes)`, so
/// with 100 notes a file at most 10 notes cover.
pub const LEARN_FILE_MIN_SPECIFIC: f32 = 0.5;
/// Learned questions kept per note, newest first.
pub const LEARNED_PER_NOTE: usize = 8;
/// A note opened this long after a search, in the same session, answered it.
pub const LEARN_WINDOW_SECS: i64 = 600;
/// Logged searches are kept this long once derived; learned rows stay.
pub const SEARCH_LOG_DAYS: i64 = 30;
/// A question word is distinctive when at most this share of the scope's
/// chunks contain it. Such words must appear in a hit for the model's
/// cosine to count in full.
pub const DISTINCTIVE_MAX_SHARE: f32 = 0.1;

/// A folded `--sections` block is printed when it holds a question word in
/// at most this share of the project's chunks (`render::matched_blocks`).
/// Measured on 182 gold questions: 1% kept the Sonnet A/B's n2 bullet for
/// 45 more bytes per answer than none; the 10% distinctive bar cost 166 and
/// pushed three answer notes out of full sections.
pub const RARE_BLOCK_MAX_SHARE: f32 = 0.01;
/// When the model ran, the fused score is multiplied by this plus the
/// rescaled cosine (0 to 1): a chunk BM25 ranked first on everyday words
/// but the model put far down (cosine near the floor) scores at most
/// half of one the model agrees with. Measured 2026-10-20 on three
/// journals: recall@3 +0.016 and +0.021 on two, MRR +0.004 and +0.026,
/// the third unchanged, absent answers unchanged. (Muting BM25 when the
/// question has no rare word was tried the same day and fired on 1 of 68
/// questions: in a small store almost every content word is rare.)
pub const COSINE_SCORE_BASE: f32 = 0.5;
/// Graph rerank: a note that links a file or symbol the question names is
/// about it, whatever the words say. Bridge: the question's symbol is
/// defined (code index) in a file the note links.
pub const GRAPH_DIRECT_WEIGHT: f32 = 1.5;
pub const GRAPH_BRIDGE_WEIGHT: f32 = 1.2;
/// Most a note linked to a file the session is working on is weighted
/// (`Context::working_set`), under `GRAPH_DIRECT_WEIGHT`: a file the question
/// names is stronger evidence than a file the session touched. Scaled per
/// file by its specificity (`search::working_factors`). Measured 2026-10-22
/// on four gold sets (182 questions), working set = the answer note's files,
/// another question's, or either plus the project's three most edited
/// files: at 1.2 MRR rose on three sets (0.747 to 0.803) and the wrong files
/// lost nothing; 1.3 lost 2. Without the specificity discount 1.2 lost 3.
pub const WORKING_WEIGHT: f32 = 1.2;
/// The working set: edits and reads of this session in the last 30 min,
/// edits weighing 1.0 and reads 0.5, halved every 10 min, top 10 files.
pub const WORKING_WINDOW_SECS: i64 = 1800;
pub const WORKING_HALF_LIFE_SECS: f32 = 600.0;
pub const WORKING_MAX_FILES: usize = 10;
pub const WORKING_READ_WEIGHT: f32 = 0.5;
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
/// Recall searches at most this many content words of a prompt, after code
/// fences, stack frames and identifier lines are dropped
/// (`search::recall_query`): a 60-word chunk is matched by a question, not
/// by a 2,000-word paste whose vector is a blur.
pub const RECALL_QUERY_WORDS: usize = 80;
/// The bar a note must clear in recall when it has been ignored: recalled
/// `SERVED_NEVER_FOLLOWED` times since it was last opened, or recalled in
/// the last `RECALL_REPEAT_SECS` and not opened since (`quality::recall_bar`).
/// 0.70 is cosine 0.74, the right note's median: an ignored note comes back
/// only when the prompt is squarely about it.
pub const RECALL_IGNORED_CONFIDENCE: f32 = 0.70;
/// A note recalled within this window and not opened since is not recalled
/// again in another session under `RECALL_IGNORED_CONFIDENCE`.
pub const RECALL_REPEAT_SECS: i64 = 24 * 3600;
/// Mid-session reminder (the same prompt hook): when this many files were
/// edited since the last note, or the oldest such edit is this old, one
/// `note owed` line is printed, then not again for the cooldown. Rare and
/// true, so the agent does not learn to skip it.
pub const OWED_REMIND_FILES: usize = 5;
pub const OWED_REMIND_SECS: i64 = 30 * 60;
pub const OWED_REMIND_COOLDOWN_SECS: i64 = 30 * 60;
/// The edit gate's area search (`gate::check`, first edit in a directory
/// or a new file): hits shown, and the top confidence that turns the
/// context into a block. 0.60 is cosine 0.72, between the off-topic peak
/// and the right note's median, a little stricter than recall.
pub const GATE_SEARCH_K: usize = 3;
pub const GATE_SEARCH_BLOCK: f32 = 0.60;
/// Lines the edit gate hands the agent per edit, the rest replaced by a
/// pointer: a field report counted the outline of a 2,001-line function and
/// "+10 more notes" on every edit, spent context that brought compaction
/// sooner. About 25 lines is a links answer for one file with its notes.
pub const GATE_MAX_LINES: usize = 25;
/// Characters of the Rejected line a tripwire quotes: enough for the
/// measurement, under a line of context.
pub const TRIP_LINE_CHARS: usize = 220;
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
/// 2026-10-18, Courier): the one figure with no counterfactual guess.
pub const BENCHMARK_LINE: &str = "benchmark (controlled A/B, Codex on Courier, 2026-10-18): retrieval tokens 63% less on average (44%, 55%, 91%), whole run 32% to 53% less; Antigravity six-turn sessions 34% less cost";
/// A definition wider than this is too wide to read as one span: `links
/// --at` narrows to the innermost block holding the line (a match arm, an
/// `if`, a loop) and `status` lists such definitions, since a note that
/// points at them points at nothing readable.
pub const SPAN_WIDE: u32 = 150;
/// Inner blocks `links <symbol>` lists under a definition wider than
/// `SPAN_WIDE`, widest first, so a mid-function edit has a range to read.
pub const WIDE_BLOCKS_SHOWN: usize = 6;
/// Notes `links <file>` lists before saying "+N more": a hub file names
/// dozens, nobody reads a list that long, and `--at <line>` narrows it.
pub const LINKS_NOTES: usize = 5;
/// Notes in a `links` list printed with their first Decision sentence under
/// the title: a list of titles gets skimmed (2026-10-19), a decision gets read,
/// and the newest are the ones most likely to hold for the code today.
pub const LINKS_DECISIONS: usize = 2;
/// A call neighbour with more callers than this (a utility like
/// `current_session`) says nothing about the function being edited and does
/// not rank a note: on this repo 95% of functions have at most 8 callers.
pub const NEIGHBOUR_MAX_CALLERS: usize = 8;
/// Confidence of a call edge by how it is written, measured 2026-10-22 on
/// 532 signature changes in the history of three repos (Courier,
/// memorycastle, rushtex; 3,544 edges): the share of recorded call sites
/// updated in the same commit, divided by the best class's 64% (not every
/// signature change touches its callers). `self.f(` 64%, bare `f(` with one
/// definition 59%, `x.f(` with one definition 47%, `module::f(` naming the
/// definition's file or type 45%, `Type.f(` naming it 33%, bare `f(` with
/// several definitions 31%, a qualifier naming something else 13%, `x.f(`
/// with several definitions 13%. A bare `f(` with several definitions splits
/// by where it is: in the definition's own file 70% (scoping resolves it
/// there), from a file with its own `f` 19%, from any other file 0 of 59.
pub const CALL_CONF_SELF: f32 = 1.0;
pub const CALL_CONF_SAME_FILE: f32 = 1.0;
pub const CALL_CONF_BARE_UNIQUE: f32 = 0.9;
pub const CALL_CONF_METHOD_UNIQUE: f32 = 0.7;
pub const CALL_CONF_QUALIFIED: f32 = 0.7;
pub const CALL_CONF_TYPE: f32 = 0.5;
/// `x.f(` where `x` is declared (`code_fields`) with a type defined in the
/// definition's file, or is that type's name in lower case. Set at the
/// method-unique class pending a co-change measurement of its own.
pub const CALL_CONF_RECEIVER: f32 = 0.7;
/// Call sites `links --context` prints before "+N more" (`--all` lifts it).
pub const CONTEXT_SITES: usize = 12;
pub const CALL_CONF_OTHER: f32 = 0.2;
/// Edges under this are counted, not listed, and do not make neighbours.
pub const CALL_MIN_CONF: f32 = 0.3;
/// Changed functions a saved note does not name that `save` lists: a task
/// changed a median 15 functions (2026-10-22), so the list is the most
/// changed few, not all of them.
pub const SAVE_UNNAMED_MAX: usize = 5;
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
/// Pending chunks a save (or `sync` at session start) embeds inline, about
/// 3 s; more go to a detached `memorycastle embed`.
pub const SAVE_EMBED_MAX: usize = 32;
/// A background embed that set `embedding_at` longer ago than this is taken
/// as dead and another may start (duplicate work is harmless: vectors are
/// written with INSERT OR REPLACE).
pub const EMBED_BACKGROUND_STALE_SECS: i64 = 900;
/// A `## Decision` longer than this, or with more bullets than
/// `DECISION_MAX_BULLETS`, holds several decisions, so `save` says to split.
/// It spans two chunks at most; nothing needs it in one (the decision line
/// printed is read from the note, `store::decision_sentence`).
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
const _: () = { assert!(SEARCH_CHUNK_BYTES * 3 < SEARCH_OUTPUT_BYTES); assert!(NOTE_MAX_WORDS > CHUNK_MAX_WORDS * 3); assert!(DECISION_MIN_WORDS < DECISION_MAX_WORDS); assert!(DECISION_MAX_WORDS <= CHUNK_MAX_WORDS * 2, "a decision spans at most two chunks"); };

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
/// changed`, each also gets the tripwire); every changed file is recorded as
/// an edit. More than this on the first call of a session with no declared
/// start is a dirty checkout, not the command's work, and is skipped.
pub const CHANGED_MAX_FILES: usize = 3;
/// Commands that rewrite files nobody decided on (formatters, builds,
/// code generators): `changed` records their edits and prints nothing. A
/// command matches when its first word, after `cd .. &&`, `env A=b`, `time`
/// or `nice`, starts one of these.
pub const BULK_COMMANDS: &[&str] = &[
    "cargo fmt", "rustfmt", "prettier", "npx prettier", "black", "ruff format", "gofmt", "goimports", "swiftformat", "ktlint", "clang-format",
    "npm run build", "npm run format", "pnpm build", "yarn build", "make", "cmake", "ninja", "tsc", "webpack", "vite build", "npm install", "npm ci", "pnpm install", "yarn install", "cargo update", "go generate", "go mod tidy", "flutter pub get", "pod install", "./gradlew", "gradle",
];
/// Directories whose files are build output or vendored: never looked up,
/// never recorded as edits, whatever `.gitignore` says.
pub const GENERATED_DIRS: &[&str] = &["target/", "node_modules/", "build/", "dist/", ".next/", "__pycache__/", ".gradle/", "Pods/", "vendor/", ".venv/"];
/// Characters of the Decision sentence printed under a recall line.
pub const RECALL_DECISION_CHARS: usize = 220;

/// Definitions `links` prints for a name with no definition, found by the
/// words of the name (`code::find`, `code::nearest_line`).
pub const CODE_FIND_K: usize = 3;
/// Distinct question words a definition must hold (name, signature or the
/// comment above it) to be printed: one shared word is chance.
pub const CODE_FIND_MIN_WORDS: usize = 2;
/// Bytes of the comment above a definition kept in its find words.
pub const CODE_FIND_DOC_BYTES: usize = 400;
