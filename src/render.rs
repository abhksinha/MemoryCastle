//! The search answer as text, under one byte budget, for the CLI and the
//! MCP server alike: one block per note, the ranked `read:` spans, and the
//! ledger rows the call must leave behind.

use crate::limits::{SEARCH_CHUNKS_PER_NOTE, SEARCH_CHUNK_BYTES, SEARCH_OUTPUT_BYTES, SECTION_LINES};
use crate::search::Hit;
use crate::spans::ReadSpan;
use crate::{code, graph, store, Result};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub struct Options<'a> {
    pub query: &'a str,
    pub sections: bool,
    /// With `sections`: whole sections, not only the blocks that matched.
    pub whole: bool,
    /// The hit line and the note's Decision sentence per hit, then the read
    /// table: the default, about 50 tokens per hit.
    pub brief: bool,
    pub explain: bool,
    /// Print absolute paths for spans.
    pub abs: bool,
    pub repo_root: Option<&'a Path>,
}

pub struct Rendered {
    pub text: String,
    /// How much of each note was printed: "brief", "snippet" or "sections".
    pub mode: &'static str,
    /// Notes the output drew from.
    pub notes: Vec<i64>,
    /// Spans printed: (path, start, end, rank), for the guided-read ledger.
    pub served: Vec<(String, u32, u32, u32)>,
}

/// Renders hits and read spans. Records nothing; the caller writes the
/// ledger with `record`.
///
/// Layout, a fixed grammar one block per note, for eyes and parsers alike:
/// line 1 `n. <path>:<line>   <confidence>   <section>` (then a date for an
/// undated tracker, the project when hits span projects, `superseded by`);
/// line 2 the title and author; then `   <label>     <value>` lines with the
/// label padded to `LABEL_WIDTH` and continuations indented to `VALUE_COL`
/// (`files`, `text`, `decision`, `rejected`, `measured`, `open`, `code`,
/// `trace`); a blank line ends the block. The `read first` table follows,
/// rows lettered `a.` so they never look like hits, or `read first: none`.
/// `brief` (the default) prints line 1 and a `decision` field holding the
/// note's first Decision sentence, no blank lines: enough to pick a note.
pub fn search(conn: &Connection, hits: &[Hit], read_spans: &[ReadSpan], o: &Options) -> Result<Rendered> {
    let mut seen_notes: HashSet<i64> = HashSet::new();
    let mut notes = Vec::new();
    let mut out = String::new();
    let mut n = 0;
    let mut served: Vec<(String, u32, u32, u32)> = Vec::new();
    let mut overflow = false;
    let note_links = if o.sections {
        let mut ids: Vec<i64> = hits.iter().map(|h| h.note_id).collect();
        ids.sort_unstable();
        ids.dedup();
        graph::links_of(conn, &ids)?
    } else {
        HashMap::new()
    };
    // The project tag only matters when hits come from more than one.
    let multi = hits.iter().map(|h| h.project.as_str()).collect::<HashSet<_>>().len() > 1;
    for h in hits {
        if !seen_notes.insert(h.note_id) {
            continue;
        }
        n += 1;
        let siblings: Vec<&Hit> = hits.iter().filter(|x| x.note_id == h.note_id && x.chunk_id != h.chunk_id).take(SEARCH_CHUNKS_PER_NOTE - 1).collect();
        let mut block = String::new();
        let date = fmt_date(h.dated.unwrap_or(h.saved_at));
        let sup = h.superseded_by.as_ref().map(|p| format!("  superseded by {p}")).unwrap_or_default();
        // A tracker naming forty symbols had all forty on its hit line.
        let sup = match h.code_changed.len() {
            0 => sup,
            n if n <= crate::limits::CONTEXT_ITEMS => format!("{sup}  code changed since: {}", h.code_changed.join(", ")),
            n => format!("{sup}  code changed since: {} +{} more", h.code_changed[..crate::limits::CONTEXT_ITEMS].join(", "), n - crate::limits::CONTEXT_ITEMS),
        };
        let proj = if multi { format!("  [{}]", h.project) } else { String::new() };
        let (snippet, off) = if o.sections { (String::new(), 0) } else { crate::search::focus(&h.text, o.query, SEARCH_CHUNK_BYTES / 3) };
        let (title, section) = split_heading(&h.heading);
        // First line: what an agent needs to trust and cite the hit. The
        // date is in a dated note's file name; a tracker shows its save date.
        let when = if h.dated.is_some() { String::new() } else { format!("  {date}") };
        let start = h.line + off;
        let end = h.end_line().max(start);
        let range = if end > start { format!("{start}-{end}") } else { start.to_string() };
        // A 1.00 that only a verbatim name earned says so: a tracker line
        // naming both algorithms is a mention, not the decision asked for.
        let named = if h.trace.identifier && h.trace.judged + 0.05 < h.trace.confidence { format!("  name match, by meaning {:.2}", h.trace.judged) } else { String::new() };
        block.push_str(&format!("{n}. {}:{range}   {:.2}   {section}{when}{proj}{sup}{named}\n", h.path, h.trace.confidence));
        // The note's other scored parts, so the agent reads those lines and
        // not the whole note.
        if crate::store::note_bytes(conn, h.note_id)? >= crate::limits::NOTE_GUARD_MIN_BYTES {
            let ranges: Vec<(u32, u32)> = if h.parts.is_empty() { vec![(h.line, end)] } else { h.parts.iter().map(|p| (p.line, p.end)).collect() };
            served.extend(ranges.into_iter().map(|(a, b)| (h.path.clone(), a, b, 0)));
        }
        let lines_field = if h.parts.len() > 1 {
            let parts: Vec<String> = h.parts.iter().map(|p| {
                let r = if p.end > p.line { format!("{}-{}", p.line, p.end) } else { p.line.to_string() };
                format!("{r} {:.2} {}", p.confidence, if p.section.is_empty() { "-" } else { &p.section })
            }).collect();
            field("lines", &parts.join(", "))
        } else {
            String::new()
        };
        if o.brief {
            if let Some(d) = crate::store::decision_sentence(conn, h.note_id)? {
                block.push_str(&field("decision", &d));
            }
            block.push_str(&lines_field);
            notes.push(h.note_id);
            out.push_str(&block);
            continue;
        }
        block.push_str(&format!("   {title}   {}\n", h.source));
        let files = tidy_files(&h.files);
        if !files.is_empty() {
            let shown: Vec<&str> = files.iter().take(crate::limits::SEARCH_FILES_SHOWN).map(String::as_str).collect();
            let more = if files.len() > shown.len() { format!(" +{} more", files.len() - shown.len()) } else { String::new() };
            block.push_str(&field("files", &format!("{}{more}", shown.join(", "))));
        }
        block.push_str(&lines_field);
        if o.explain {
            block.push_str(&field("trace", &format!(
                "bm25 rank {}  vec rank {}  score {:.4}  weight {:.1}  graph {:.1}  working {:.2}  cosine {}  coverage {:.2}{}{}",
                h.trace.bm25_rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
                h.trace.vec_rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
                h.trace.score,
                h.trace.weight,
                h.trace.graph,
                h.trace.working,
                h.trace.cosine.map(|c| format!("{c:.3}")).unwrap_or_else(|| "-".into()),
                h.trace.coverage,
                if h.trace.via > 0 { format!("  via also {}", h.trace.via) } else { String::new() },
                if h.trace.learned > 0.0 { format!("  learned {:.2}", h.trace.learned) } else { String::new() }
            )));
        }
        if !o.sections {
            if !snippet.trim().is_empty() {
                block.push_str(&field("text", snippet.trim()));
            }
        } else {
            // Decision, Rejected and Measured are always printed; Open only
            // when a hit landed in it (that is where a task's instruction
            // lives). Chunks inside a printed section are not repeated.
            let in_open = std::iter::once(h).chain(siblings.iter().copied()).any(|c| split_heading(&c.heading).1.starts_with("Open"));
            let names: &[&str] = if in_open { &["Decision", "Rejected", "Measured", "Open"] } else { &["Decision", "Rejected", "Measured"] };
            let printed = |c: &Hit| names.iter().any(|s| split_heading(&c.heading).1.starts_with(s));
            for (j, c) in std::iter::once(h).chain(siblings.iter().copied()).enumerate() {
                if printed(c) {
                    continue;
                }
                let mut body = String::new();
                if j > 0 {
                    body.push_str(&format!("{}:{}\n", split_heading(&c.heading).1, c.line));
                }
                let mut used = 0;
                for line in c.text.lines() {
                    let room = SEARCH_CHUNK_BYTES.saturating_sub(used);
                    if room < 40 {
                        body.push_str("…\n");
                        break;
                    }
                    let shown = if line.len() > room {
                        let mut end = room;
                        while !line.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!("{}…", &line[..end])
                    } else {
                        line.to_string()
                    };
                    used += line.len();
                    body.push_str(shown.trim_end());
                    body.push('\n');
                }
                block.push_str(&field("text", body.trim_end()));
            }
            // The blocks (a bullet, a paragraph, a table) that hold a scored
            // part of the note; the rest as line ranges to read on demand.
            let rare = crate::search::rare_question_words(conn, h.project_id, o.query)?;
            let mut matched: Vec<(u32, u32)> = vec![(h.line, h.end_line())];
            matched.extend(h.parts.iter().filter(|p| p.confidence >= crate::limits::MIN_CONFIDENCE).map(|p| (p.line, p.end)));
            for name in names {
                let body = store::section_lines(conn, h.note_id, name)?;
                if body.is_empty() {
                    continue;
                }
                let shown = if o.whole { whole_section(&body) } else { matched_blocks(&body, &matched, *name == "Decision", &rare) };
                block.push_str(&field(&name.to_ascii_lowercase(), &shown));
            }
            // Where the code this note names lives now.
            if let Some(l) = note_links.get(&h.note_id) {
                let mut rows: Vec<(String, String)> = Vec::new();
                for (_, sym) in l.iter().filter(|(k, _)| *k == graph::Kind::Symbol) {
                    let files: Vec<&String> = l.iter().filter(|(k, _)| *k == graph::Kind::File).map(|(_, f)| f).collect();
                    if crate::spans::is_note_file(sym, &files) {
                        continue;
                    }
                    for d in crate::spans::note_defs(code::defs_named(conn, h.project_id, sym)?, &files).iter().take(2) {
                        let span = match o.repo_root {
                            Some(r) => code::current_span(conn, h.project_id, r, d)?,
                            None => Some((d.line, d.end)),
                        };
                        if let Some((a, b)) = span {
                            rows.push((sym.clone(), format!("{}:{a}-{b}", d.path)));
                            served.push((d.path.clone(), a, b, 0));
                        }
                    }
                    if rows.len() >= crate::limits::SEARCH_CODE_SPANS {
                        break;
                    }
                }
                let w = rows.iter().map(|(s, _)| s.len()).max().unwrap_or(0);
                let body: Vec<String> = rows.iter().map(|(sym, loc)| format!("{sym:w$}  {loc}")).collect();
                if !body.is_empty() {
                    block.push_str(&field("code", &body.join("\n")));
                }
            }
        }
        // Over budget, a note keeps its hit line and title rather than
        // vanish: on the Sonnet A/B the note that answered n1 ranked fourth
        // under two long `--sections` blocks and was never printed.
        if overflow || (out.len() + block.len() > SEARCH_OUTPUT_BYTES && n > 1) {
            if !overflow {
                out.push_str("more notes, hit line and title only (search one of them by its words to open it):\n");
                overflow = true;
            }
            let mut lines = block.lines();
            if let Some(first) = lines.next() {
                out.push_str(first);
                out.push('\n');
                out.push_str(&format!("   {title}\n\n"));
            }
            notes.push(h.note_id);
            continue;
        }
        notes.push(h.note_id);
        out.push_str(&block);
        if !o.brief {
            out.push('\n');
        }
    }
    if read_spans.is_empty() {
        if !out.is_empty() {
            out.push_str("read first: none\n");
        }
    } else {
        let in_root = o.repo_root.map(|r| std::env::current_dir().map(|c| c == r).unwrap_or(false)).unwrap_or(false);
        let total: usize = read_spans.iter().map(|s| s.bytes).sum();
        out.push_str(&format!("read first ({} spans, {:.1} KB; sed -n 'a,bp' path):\n", read_spans.len(), total as f64 / 1024.0));
        let rows: Vec<(String, String)> = read_spans
            .iter()
            .map(|s| {
                let path = if o.abs || !in_root { s.abs.as_deref().unwrap_or(&s.path) } else { &s.path };
                (format!("{path}:{}-{}", s.start, s.end), clip(&s.name, READ_NAME_WIDTH))
            })
            .collect();
        let lw = rows.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
        let nw = rows.iter().map(|(_, n)| n.chars().count()).max().unwrap_or(0);
        for (i, (s, (loc, name))) in read_spans.iter().zip(&rows).enumerate() {
            let letter = (b'a' + (i as u8).min(25)) as char;
            out.push_str(&format!("   {letter}. {loc:lw$}  {name:nw$}  {:.2}  {}\n", s.score, s.why));
            served.push((s.path.clone(), s.start, s.end, i as u32 + 1));
        }
    }
    let mode = if o.brief { "brief" } else if o.sections { "sections" } else { "snippet" };
    Ok(Rendered { text: out, mode, notes, served })
}

/// Widest a symbol name is shown in the `read first` table before `…`.
const READ_NAME_WIDTH: usize = 28;

/// The label column: three spaces, the label padded to `LABEL_WIDTH`, then
/// the value. Continuation lines are indented to the value column, so a
/// label line is `^   \S` and a continuation `^ {VALUE_COL}`: nothing a
/// note contains can read as a label or a hit.
pub const LABEL_WIDTH: usize = 10;
pub const VALUE_COL: usize = 3 + LABEL_WIDTH + 1;

/// Prose longer than this is wrapped at a space; the bytes are the same,
/// the terminal reads it. A line with no space before the limit is kept.
pub const WRAP_COLS: usize = 92;

fn field(label: &str, value: &str) -> String {
    let mut out = String::new();
    let mut first = true;
    for line in value.lines().flat_map(wrap) {
        if first {
            out.push_str(&format!("   {label:LABEL_WIDTH$} {line}\n"));
            first = false;
        } else {
            out.push_str(&format!("{:VALUE_COL$}{line}\n", ""));
        }
    }
    out
}

fn wrap(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while rest.chars().count() > WRAP_COLS {
        let cut = rest.char_indices().take(WRAP_COLS + 1).filter(|(_, c)| *c == ' ').map(|(i, _)| i).last();
        match cut {
            Some(i) if i > 0 => {
                out.push(rest[..i].to_string());
                rest = rest[i..].trim_start();
            }
            _ => break,
        }
    }
    out.push(rest.to_string());
    out
}

/// A chunk heading is `title > section > …`; the title and the rest.
fn split_heading(heading: &str) -> (&str, &str) {
    match heading.split_once(" > ") {
        Some((t, rest)) => (t, rest),
        None => (heading, ""),
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

/// Writes the ledger for a rendered search: spans served (so a following
/// read counts as guided) and bytes printed against the notes drawn from.
pub fn record(conn: &Connection, project_id: i64, r: &Rendered) -> Result<()> {
    // A benchmark loop is not an agent: `MEMORYCASTLE_NO_LEDGER=1` keeps
    // its calls out of every ledger, as `--explain` and `eval` already are.
    if std::env::var_os("MEMORYCASTLE_NO_LEDGER").is_some() {
        return Ok(());
    }
    for (f, a, b, rank) in &r.served {
        store::record_span(conn, project_id, f, *a, *b, *rank)?;
    }
    let mut stood = 0usize;
    for id in &r.notes {
        stood += store::note_bytes(conn, *id)?;
    }
    store::record_served_notes(conn, project_id, &r.notes, r.mode)?;
    store::record_usage(conn, project_id, "search", r.text.len(), stood)
}

/// One spelling per file: a bare name is dropped when a full path with that
/// name is also listed. Sorted, full paths first.
pub fn tidy_files(files: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in files {
        let bare = !f.contains('/');
        if bare && files.iter().any(|g| g.contains('/') && g.ends_with(&format!("/{f}"))) {
            continue;
        }
        if !out.contains(f) {
            out.push(f.clone());
        }
    }
    out.sort_by(|a, b| b.contains('/').cmp(&a.contains('/')).then(a.cmp(b)));
    out
}

/// The "no results" line for an empty hit list.
pub fn empty_line(ex: &crate::search::Explain) -> String {
    if ex.dropped > 0 {
        format!("no relevant note (best confidence {:.2}, below {:.2}); the store has nothing on this", ex.best_confidence, ex.floor.max(crate::limits::MIN_CONFIDENCE))
    } else {
        "no results".to_string()
    }
}

/// `YYYY-MM-DD` from a Unix timestamp (Howard Hinnant's civil-from-days).
pub fn fmt_date(ts: i64) -> String {
    let days = ts.div_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Unix timestamp at midnight of `YYYY-MM-DD` (days-from-civil), None when
/// the text is not a date.
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 10 {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..10].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || &s[4..5] != "-" || &s[7..8] != "-" {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400)
}

/// A section as printed by `--sections --whole`: its lines, up to `SECTION_LINES`.
fn whole_section(body: &[(u32, String)]) -> String {
    let mut shown: String = body.iter().take(SECTION_LINES).map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n");
    if body.len() > SECTION_LINES {
        shown.push_str(&format!("\n… {} more lines", body.len() - SECTION_LINES));
    }
    shown
}

/// The blocks of a section that overlap a matched line range, or hold one
/// of the question's `rare` words, the first block too when `lead` (a
/// Decision opens with the decision); every skipped run becomes `… lines
/// a-b`. A block is a top-level bullet or numbered item with its
/// continuation lines, or a run of non-blank lines.
fn matched_blocks(body: &[(u32, String)], matched: &[(u32, u32)], lead: bool, rare: &[String]) -> String {
    let mut blocks: Vec<&[(u32, String)]> = Vec::new();
    let mut start = 0;
    for i in 1..=body.len() {
        let ends = i == body.len() || {
            let l = body[i].1.as_str();
            let item = l.starts_with("- ") || l.starts_with("* ") || l.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
            l.trim().is_empty() || body[i - 1].1.trim().is_empty() || item
        };
        if ends {
            if body[start..i].iter().any(|(_, l)| !l.trim().is_empty()) {
                blocks.push(&body[start..i]);
            }
            start = i;
        }
    }
    let mut hits: Vec<bool> = blocks
        .iter()
        .enumerate()
        .map(|(k, b)| {
            (lead && k == 0)
                || matched.iter().any(|&(x, y)| x <= b[b.len() - 1].0 && b[0].0 <= y)
                || crate::search::has_any_word(&b.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n"), rare)
        })
        .collect();
    // A one-line block between two printed ones costs what its range would.
    for k in 1..blocks.len().saturating_sub(1) {
        if blocks[k].len() == 1 && hits[k - 1] && hits[k + 1] {
            hits[k] = true;
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut skipped: Option<(u32, u32)> = None;
    let mut printed = 0;
    for (b, &hit) in blocks.iter().zip(&hits) {
        let (a, z) = (b[0].0, b[b.len() - 1].0);
        if hit && printed < SECTION_LINES {
            if let Some((x, y)) = skipped.take() {
                out.push(format!("… lines {x}-{y}"));
            }
            for (_, l) in b.iter().filter(|(_, l)| !l.trim().is_empty()) {
                out.push(l.clone());
                printed += 1;
            }
        } else {
            skipped = Some(skipped.map_or((a, z), |(x, _)| (x, z)));
        }
    }
    if let Some((x, y)) = skipped {
        out.push(if printed == 0 { format!("lines {x}-{y}, none matched") } else { format!("… lines {x}-{y}") });
    }
    out.join("\n")
}


#[cfg(test)]
mod tests {
    #[test]
    fn sections_print_the_matched_blocks_and_ranges_for_the_rest() {
        let body: Vec<(u32, String)> = [
            "- First decision line.", "  continued.", "- Second, about tables.", "  more.", "- Third, about chunks.", "", "A paragraph", "over two lines.",
        ]
        .iter()
        .enumerate()
        .map(|(i, l)| (10 + i as u32, l.to_string()))
        .collect();
        let shown = matched_blocks(&body, &[(14, 14)], true, &[]);
        assert_eq!(shown, "- First decision line.\n  continued.\n… lines 12-13\n- Third, about chunks.\n… lines 16-17");
        assert_eq!(matched_blocks(&body, &[(90, 95)], false, &[]), "lines 10-17, none matched");
        // Lines 12-13 are two lines: still a range. A one-line block between printed ones is printed.
        let one: Vec<(u32, String)> = ["- a", "- b", "- c"].iter().enumerate().map(|(i, l)| (1 + i as u32, l.to_string())).collect();
        assert_eq!(matched_blocks(&one, &[(3, 3)], true, &[]), "- a\n- b\n- c");
    }

    use super::*;

    #[test]
    fn dates_round_trip_and_files_collapse() {
        assert_eq!(fmt_date(parse_date("2026-10-18").unwrap()), "2026-10-18");
        assert_eq!(parse_date("plan"), None);
        let f = tidy_files(&["blur.rs".into(), "crates/x/src/blur.rs".into(), "a.kt".into()]);
        assert_eq!(f, vec!["crates/x/src/blur.rs".to_string(), "a.kt".to_string()]);
    }
}
