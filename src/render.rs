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
    /// Only the first line of each hit and the read table.
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
/// Layout, one block per note. The first line is what an agent needs to
/// trust and cite the hit and nothing else: `n. <confidence>  <path>:<line>
/// > <section>  <date>`. Then the title (with the author), `files:`,
/// `text:` or the printed sections, `code:` spans; the `read first` list is
/// numbered with aligned columns. `brief` stops after the first line.
pub fn search(conn: &Connection, hits: &[Hit], read_spans: &[ReadSpan], o: &Options) -> Result<Rendered> {
    let mut seen_notes: HashSet<i64> = HashSet::new();
    let mut notes = Vec::new();
    let mut out = String::new();
    let mut n = 0;
    let mut served: Vec<(String, u32, u32, u32)> = Vec::new();
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
        let proj = if multi { format!("  [{}]", h.project) } else { String::new() };
        let (snippet, off) = if o.sections { (String::new(), 0) } else { crate::search::focus(&h.text, o.query, SEARCH_CHUNK_BYTES / 3) };
        let (title, section) = split_heading(&h.heading);
        // First line: what an agent needs to trust and cite the hit. The
        // date is in a dated note's file name; a tracker shows its save date.
        let when = if h.dated.is_some() { String::new() } else { format!("  {date}") };
        block.push_str(&format!("{n}. {:.2}  {}:{} > {section}{when}{proj}{sup}\n", h.trace.confidence, h.path, h.line + off));
        if o.brief {
            notes.push(h.note_id);
            out.push_str(&block);
            continue;
        }
        block.push_str(&format!("   {title}  ({})\n", h.source));
        let files = tidy_files(&h.files);
        if !files.is_empty() {
            let shown: Vec<&str> = files.iter().take(crate::limits::SEARCH_FILES_SHOWN).map(String::as_str).collect();
            let more = if files.len() > shown.len() { format!(" +{} more", files.len() - shown.len()) } else { String::new() };
            block.push_str(&format!("   files: {}{more}\n", shown.join(", ")));
        }
        if o.explain {
            block.push_str(&format!(
                "   trace: bm25 rank {}  vec rank {}  score {:.4}  weight {:.1}  graph {:.1}  cosine {}  coverage {:.2}\n",
                h.trace.bm25_rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
                h.trace.vec_rank.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
                h.trace.score,
                h.trace.weight,
                h.trace.graph,
                h.trace.cosine.map(|c| format!("{c:.3}")).unwrap_or_else(|| "-".into()),
                h.trace.coverage
            ));
        }
        if !o.sections {
            if !snippet.trim().is_empty() {
                block.push_str("   text:\n");
                for line in snippet.lines() {
                    block.push_str(&format!("     {line}\n"));
                }
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
                if j == 0 {
                    block.push_str("   text:\n");
                } else {
                    block.push_str(&format!("   text:  {}:{}\n", split_heading(&c.heading).1, c.line));
                }
                let mut used = 0;
                for line in c.text.lines() {
                    let room = SEARCH_CHUNK_BYTES.saturating_sub(used);
                    if room < 40 {
                        block.push_str("     …\n");
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
                    block.push_str(&format!("     {shown}\n"));
                }
            }
            for name in names {
                if let Some(body) = store::section_body(conn, h.note_id, name)? {
                    let total = body.lines().count();
                    block.push_str(&format!("   {}:\n", name.to_ascii_lowercase()));
                    for line in body.lines().take(SECTION_LINES) {
                        block.push_str(&format!("     {line}\n"));
                    }
                    if total > SECTION_LINES {
                        block.push_str(&format!("     … {} more lines\n", total - SECTION_LINES));
                    }
                }
            }
            // Where the code this note names lives now.
            if let Some(l) = note_links.get(&h.note_id) {
                let mut rows: Vec<(String, String)> = Vec::new();
                for (_, sym) in l.iter().filter(|(k, _)| *k == graph::Kind::Symbol) {
                    for d in code::defs_named(conn, h.project_id, sym)?.iter().take(2) {
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
                for (i, (sym, loc)) in rows.iter().enumerate() {
                    let key = if i == 0 { "code: " } else { "      " };
                    block.push_str(&format!("   {key} {sym:w$}  {loc}\n"));
                }
            }
        }
        if out.len() + block.len() > SEARCH_OUTPUT_BYTES && n > 1 {
            out.push_str(&format!("   … {} more notes within budget; narrow the question or raise -k\n", seen_notes.len() - n + 1));
            break;
        }
        notes.push(h.note_id);
        out.push_str(&block);
    }
    if !read_spans.is_empty() {
        let in_root = o.repo_root.map(|r| std::env::current_dir().map(|c| c == r).unwrap_or(false)).unwrap_or(false);
        let total: usize = read_spans.iter().map(|s| s.bytes).sum();
        out.push_str(&format!("read first ({} spans, {:.1} KB in all; sed -n 'a,bp' path):\n", read_spans.len(), total as f64 / 1024.0));
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
            out.push_str(&format!("   {}. {loc:lw$}  {name:nw$}  {:.2}  {}\n", i + 1, s.score, s.why));
            served.push((s.path.clone(), s.start, s.end, i as u32 + 1));
        }
    }
    let mode = if o.brief { "brief" } else if o.sections { "sections" } else { "snippet" };
    Ok(Rendered { text: out, mode, notes, served })
}

/// Widest a symbol name is shown in the `read first` table before `…`.
const READ_NAME_WIDTH: usize = 28;

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
        format!("no relevant note (best confidence {:.2}, below {:.2}); the store has nothing on this", ex.best_confidence, crate::limits::MIN_CONFIDENCE)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip_and_files_collapse() {
        assert_eq!(fmt_date(parse_date("2026-10-18").unwrap()), "2026-10-18");
        assert_eq!(parse_date("plan"), None);
        let f = tidy_files(&["blur.rs".into(), "crates/x/src/blur.rs".into(), "a.kt".into()]);
        assert_eq!(f, vec!["crates/x/src/blur.rs".to_string(), "a.kt".to_string()]);
    }
}
