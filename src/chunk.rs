//! Splitting a note into chunks and normalising text for the FTS index.
//!
//! Sections are split on Markdown headings, then by size. Size is counted in
//! words so the keyword path loads no tokenizer; 200 words of notes full of
//! identifiers is about 300 to 400 model tokens, the design's chunk budget.
//! A section split by size overlaps its neighbour by one sentence.

/// One section of a note, ready to insert.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub heading_path: String,
    pub text: String,
    /// 1-based line of the note where this chunk starts, for `note.md:line`.
    pub line: u32,
    /// 1-based line where it ends. The text cannot say: sentences of a
    /// section are joined with spaces, so three bullets read as one line.
    pub end: u32,
}

/// Bump when `split` changes what a chunk is; stores chunked by an older
/// splitter are rechunked once (vectors of unchanged text are kept).
pub const CHUNK_VERSION: &str = "5";

pub use crate::limits::CHUNK_TARGET_WORDS as TARGET_WORDS;
pub use crate::limits::CHUNK_MAX_WORDS as MAX_WORDS;

/// Title of a note: its first `# ` heading, else the file stem.
pub fn title_of(content: &str, fallback: &str) -> String {
    for line in content.lines() {
        if let Some(h) = line.strip_prefix("# ") {
            let h = h.trim();
            if !h.is_empty() {
                return h.to_string();
            }
        }
    }
    fallback.to_string()
}

/// Header lines: everything before the first `##`, at most 20 lines.
fn header(content: &str) -> impl Iterator<Item = &str> {
    content.lines().take(20).take_while(|l| !l.starts_with("## "))
}

/// The path this note declares it replaces: a header line such as
/// `Supersedes: journal/2026-10-01-x.md` or "This note supersedes
/// journal/2026-10-01-x.md." The newer note knows the past; the old note is
/// never edited.
pub fn supersedes(content: &str) -> Option<String> {
    for line in header(content) {
        let l = line.trim();
        let lower = l.to_ascii_lowercase();
        let Some(i) = lower.find("supersedes") else { continue };
        let rest = &l[i + "supersedes".len()..];
        let target = rest
            .trim_start_matches(|c: char| c == ':' || c.is_whitespace())
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '`' || c == '.' || c == ',' || c == '"');
        if target.contains('/') || target.ends_with(".md") {
            return Some(target.to_string());
        }
    }
    None
}

/// The `Author:` value from the header, e.g. `abhi+claude`; stops at `|`.
pub fn author(content: &str) -> Option<String> {
    for line in header(content) {
        let l = line.trim();
        let Some(i) = l.find("Author:") else { continue };
        let v = l[i + 7..].split('|').next().unwrap_or("").trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

/// The header's `Date:` value (`Author: x | Date: 2026-10-18`) as a timestamp.
pub fn dated(content: &str) -> Option<i64> {
    for line in header(content) {
        let l = line.trim();
        let Some(i) = l.find("Date:") else { continue };
        let v = l[i + 5..].split('|').next().unwrap_or("").trim();
        if let Some(ts) = crate::render::parse_date(v) {
            return Some(ts);
        }
    }
    None
}

/// True when the header has a `Files:` line, the codebase files the note is about.
pub fn has_files_line(content: &str) -> bool {
    header(content).any(|l| l.contains("Files:"))
}

/// Splits `content` into chunks. Headings inside fenced code are ignored.
pub fn split(content: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    // (level, text) for the open headings, deepest last.
    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut section = String::new();
    let mut section_line = 1u32;
    let mut in_fence = false;

    let flush = |stack: &[(u8, String)], section: &mut String, section_line: u32, out: &mut Vec<Chunk>| {
        // Leading blank lines belong to no chunk; count them into the line.
        let lead = section.len() - section.trim_start().len();
        let skipped = section[..lead].matches('\n').count() as u32;
        // Header lines (Author, Date, Files, Supersedes) are metadata, not a
        // fact to return; they are parsed separately.
        let stripped: String;
        let body = if stack.len() <= 1 && section.lines().any(is_header_line) {
            stripped = section.lines().filter(|l| !is_header_line(l)).collect::<Vec<_>>().join("\n");
            stripped.trim()
        } else {
            section.trim()
        };
        if !body.is_empty() {
            let heading_path = heading_path(stack);
            for (piece, off, end) in split_by_size(body) {
                let label = entry_id(&piece);
                let hp = match label {
                    Some(id) if !heading_path.ends_with(id) => {
                        if heading_path.is_empty() { id.to_string() } else { format!("{heading_path} > {id}") }
                    }
                    _ => heading_path.clone(),
                };
                out.push(Chunk { heading_path: hp, text: piece, line: section_line + skipped + off, end: section_line + skipped + end });
            }
        }
        section.clear();
    };

    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        if !in_fence {
            if let Some((level, text)) = parse_heading(line) {
                flush(&stack, &mut section, section_line, &mut out);
                while stack.last().is_some_and(|(l, _)| *l >= level) {
                    stack.pop();
                }
                stack.push((level, text));
                section_line = i as u32 + 2;
                continue;
            }
        }
        section.push_str(line);
        section.push('\n');
    }
    flush(&stack, &mut section, section_line, &mut out);
    // A note that is only headings still deserves one chunk.
    if out.is_empty() && !content.trim().is_empty() {
        out.push(Chunk { heading_path: heading_path(&stack), text: content.trim().to_string(), line: 1, end: content.trim_end().lines().count().max(1) as u32 });
    }
    out
}

fn is_header_line(l: &str) -> bool {
    let t = l.trim_start();
    ["Author:", "Date:", "Files:", "Supersedes:"].iter().any(|k| t.starts_with(k))
}

/// A tracker id at the start of a chunk (`B-42`, `F-S0b`, `R-02`), so the
/// chunk is labelled by the entry, not by a heading like "Low".
pub fn entry_id(text: &str) -> Option<&str> {
    let head = text.trim_start_matches(|c: char| c == '-' || c == '*' || c == '|' || c == '#' || c.is_whitespace());
    let tok = head.split(|c: char| c.is_whitespace() || c == '|' || c == ':' || c == '*' || c == ']' || c == ')').next()?;
    let (pre, post) = tok.split_once('-')?;
    let ok = (1..=2).contains(&pre.len())
        && pre.chars().all(|c| c.is_ascii_uppercase())
        && (1..=5).contains(&post.len())
        && post.chars().all(|c| c.is_ascii_alphanumeric())
        && post.chars().any(|c| c.is_ascii_digit());
    ok.then_some(tok)
}

fn heading_path(stack: &[(u8, String)]) -> String {
    let mut s = String::new();
    for (i, (_, h)) in stack.iter().enumerate() {
        if i > 0 {
            s.push_str(" > ");
        }
        s.push_str(h);
    }
    s
}

fn parse_heading(line: &str) -> Option<(u8, String)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    if text.is_empty() {
        return None;
    }
    Some((hashes as u8, text.to_string()))
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Packs sentences into pieces of about `TARGET_WORDS`, never above
/// `MAX_WORDS`. Consecutive pieces share one sentence. A body within
/// `MAX_WORDS` is returned whole.
/// Pieces of at most `TARGET_WORDS` with their line offset inside `body`.
/// A piece that starts inside a table gets the table's header rows again,
/// so every table chunk reads on its own.
fn split_by_size(body: &str) -> Vec<(String, u32, u32)> {
    if word_count(body) <= MAX_WORDS {
        return vec![(body.to_string(), 0, body.trim_end().matches('\n').count() as u32)];
    }
    let sentences = sentences(body);
    let line_of = |s: &str| -> u32 {
        let off = s.as_ptr() as usize - body.as_ptr() as usize;
        body[..off.min(body.len())].matches('\n').count() as u32
    };
    // The line a sentence ends on: its start line plus its own newlines.
    let end_of = |s: &str| -> u32 { line_of(s) + s.trim_end().matches('\n').count() as u32 };
    let mut pieces = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut cur_words = 0usize;
    let mut header: Option<(&str, &str)> = None; // (header row, separator row)
    let mut prev: Option<&str> = None;
    let mut flush = |cur: &mut Vec<&str>, cur_words: &mut usize, header: Option<(&str, &str)>| {
        let first = cur[0];
        let line = line_of(first);
        let end = end_of(cur[cur.len() - 1]);
        let mut text = String::new();
        if let Some((h, sep)) = header {
            if first.starts_with('|') && first != h && first != sep {
                text.push_str(h);
                text.push('\n');
                text.push_str(sep);
                text.push('\n');
            }
        }
        text.push_str(&cur.join(if cur.iter().all(|s| s.starts_with('|')) { "\n" } else { " " }));
        pieces.push((text, line, end));
        // One-sentence overlap, unless the last sentence is itself large or a table row.
        let last = *cur.last().unwrap();
        cur.clear();
        *cur_words = 0;
        if word_count(last) <= TARGET_WORDS / 4 && !last.starts_with('|') {
            cur.push(last);
            *cur_words = word_count(last);
        }
    };
    for s in &sentences {
        if s.starts_with('|') && s.contains("---") {
            if let Some(p) = prev.filter(|p| p.starts_with('|')) {
                header = Some((p, s));
            }
        }
        prev = Some(s);
        let w = word_count(s);
        if cur_words + w > TARGET_WORDS && !cur.is_empty() {
            flush(&mut cur, &mut cur_words, header);
        }
        cur.push(s);
        cur_words += w;
    }
    if !cur.is_empty() {
        flush(&mut cur, &mut cur_words, header);
    }
    pieces
}

/// Splits on sentence terminators followed by whitespace and on blank lines.
/// A run with no terminator (a table, a list of identifiers) is split on
/// line breaks, then on words, so no sentence exceeds `TARGET_WORDS`.
/// Returns trimmed, non-empty sentences borrowing from `body`.
fn sentences(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for raw in raw_sentences(body) {
        if word_count(raw) <= TARGET_WORDS {
            out.push(raw);
            continue;
        }
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if word_count(line) <= TARGET_WORDS {
                out.push(line);
            } else {
                // Hard split at word boundaries.
                let mut start = 0;
                let mut words = 0;
                for (i, c) in line.char_indices() {
                    if c.is_whitespace() {
                        words += 1;
                        if words >= TARGET_WORDS {
                            out.push(line[start..i].trim());
                            start = i;
                            words = 0;
                        }
                    }
                }
                let tail = line[start..].trim();
                if !tail.is_empty() {
                    out.push(tail);
                }
            }
        }
    }
    out
}

fn raw_sentences(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let end_here = match b {
            b'.' | b'!' | b'?' => i + 1 >= bytes.len() || bytes[i + 1].is_ascii_whitespace(),
            b'\n' => i + 1 < bytes.len() && bytes[i + 1] == b'\n',
            _ => false,
        };
        if end_here {
            let s = body[start..=i].trim();
            if !s.is_empty() {
                out.push(s);
            }
            start = i + 1;
        }
        i += 1;
    }
    let tail = body[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

const TRAIL: &[char] = &['.', ',', ';', ':', '!', '?', ')', ']', '}', '"', '\'', '`', '*', '>'];
const LEAD: &[char] = &['(', '[', '{', '"', '\'', '`', '*', '#', '-', '>'];

/// The indexed form of chunk text: every whitespace token loses wrapping
/// punctuation (`rotated.` -> `rotated`, `B-42,` -> `B-42`, `(foo)` -> `foo`)
/// while `.` inside an identifier stays (`StatusGl.kt`).
pub fn index_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for tok in text.split_whitespace() {
        let t = tok.trim_end_matches(TRAIL).trim_start_matches(LEAD);
        if t.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(t);
    }
    out
}

/// The body of the `## <name>` section (case-insensitive prefix match on the
/// heading), without the heading line, up to the next `## `. None if absent.
pub fn section<'a>(content: &'a str, name: &str) -> Option<&'a str> {
    let lower = name.to_ascii_lowercase();
    let mut start = None;
    let mut in_code = false;
    for (off, line) in line_offsets(content) {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || !line.starts_with("## ") {
            continue;
        }
        if let Some(s) = start {
            return Some(content[s..off].trim_end());
        }
        if line[3..].trim().to_ascii_lowercase().starts_with(&lower) {
            start = Some(off + line.len() + 1);
        }
    }
    start.map(|s| content[s.min(content.len())..].trim_end())
}

/// Every `## ` section of a note: (name, 1-based line of the heading, body).
/// Headings inside fenced code are not sections.
pub fn sections(content: &str) -> Vec<(String, u32, String)> {
    let mut out: Vec<(String, u32, String)> = Vec::new();
    let mut in_code = false;
    let mut cur: Option<(String, u32, usize)> = None;
    for (off, line) in line_offsets(content) {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code || !line.starts_with("## ") {
            continue;
        }
        if let Some((name, at, start)) = cur.take() {
            out.push((name, at, content[start..off].trim().to_string()));
        }
        let at = content[..off].matches('\n').count() as u32 + 1;
        cur = Some((line[3..].trim().to_string(), at, off + line.len() + 1));
    }
    if let Some((name, at, start)) = cur {
        out.push((name, at, content[start.min(content.len())..].trim().to_string()));
    }
    out
}

fn line_offsets(s: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut off = 0;
    s.split_inclusive('\n').map(move |l| {
        let o = off;
        off += l.len();
        (o, l.trim_end_matches('\n'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_make_paths_and_fences_are_ignored() {
        let doc = "# Title\nintro\n## Sub\nbody one\n```\n# not a heading\n```\n## Other\nbody two\n";
        let c = split(doc);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].heading_path, "Title");
        assert_eq!(c[0].text, "intro");
        assert_eq!(c[1].heading_path, "Title > Sub");
        assert!(c[1].text.contains("# not a heading"));
        assert_eq!(c[2].heading_path, "Title > Other");
    }

    #[test]
    fn long_sections_split_with_overlap() {
        let sentence = "Alpha beta gamma delta epsilon zeta eta theta iota kappa.";
        let body: String = std::iter::repeat_n(sentence, 100).collect::<Vec<_>>().join(" ");
        let pieces = split_by_size(&body);
        assert!(pieces.len() >= 3);
        for p in &pieces {
            assert!(word_count(&p.0) <= MAX_WORDS, "piece too long");
        }
        // overlap: last sentence of piece 0 is first sentence of piece 1
        let last = sentences(&pieces[0].0).last().unwrap().to_string();
        assert!(pieces[1].0.starts_with(&last));
    }

    #[test]
    fn tables_without_terminators_still_split() {
        // 900 table rows, no sentence terminator anywhere.
        let row = "| C-17 | open | retry storm on reconnect | ams-lite |";
        let body: String = std::iter::repeat_n(row, 900).collect::<Vec<_>>().join("\n");
        let pieces = split_by_size(&body);
        assert!(pieces.len() >= 20);
        for (p, _, _) in &pieces {
            assert!(word_count(p) <= MAX_WORDS, "piece has {} words", word_count(p));
        }
        // One enormous line is still cut at word boundaries.
        let line: String = std::iter::repeat_n("tok", 2000).collect::<Vec<_>>().join(" ");
        for (p, _, _) in split_by_size(&line) {
            assert!(word_count(&p) <= MAX_WORDS);
        }
    }

    #[test]
    fn index_text_strips_wrapping_punctuation() {
        assert_eq!(index_text("rotated. (B-42,) `StatusGl.kt` \"quoted\""), "rotated B-42 StatusGl.kt quoted");
    }

    #[test]
    fn heading_only_note_keeps_one_chunk() {
        let c = split("# Just a title\n## And a subtitle\n");
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].heading_path, "Just a title > And a subtitle");
        assert!(c[0].text.contains("Just a title"));
        assert!(split("   \n").is_empty());
    }

    #[test]
    fn supersedes_header() {
        assert_eq!(supersedes("# T\nSupersedes: journal/2026-10-01-x.md\n## A\n"), Some("journal/2026-10-01-x.md".into()));
        assert_eq!(supersedes("# T\nThis note supersedes `journal/x.md`.\n"), Some("journal/x.md".into()));
        assert_eq!(supersedes("# T\n## A\nSupersedes: journal/x.md\n"), None, "only the header counts");
        assert_eq!(supersedes("# T\nnothing supersedes the plan here\n"), None);
        assert!(has_files_line("# T\nAuthor: claude | Files: src/a.rs\n"));
        assert_eq!(author("# T\nAuthor: abhi+claude | Date: 2026-10-18\n"), Some("abhi+claude".into()));
        assert_eq!(author("# T\n## Author: nobody\n"), None);
        assert!(!has_files_line("# T\n## Files: later\n"));
    }

    #[test]
    fn title() {
        assert_eq!(title_of("x\n# Real Title\n", "fb"), "Real Title");
        assert_eq!(title_of("no heading", "fb"), "fb");
    }

    #[test]
    fn section_returns_body_until_next_heading() {
        let d = "# T\n## Context\nwhy\n## Decision\nchose A\nbecause\n```\n## not a heading\n```\n## Rejected\nB\n";
        assert_eq!(section(d, "decision"), Some("chose A\nbecause\n```\n## not a heading\n```"));
        assert_eq!(section(d, "Rejected"), Some("B"));
        assert_eq!(section(d, "Measured"), None);
    }

    #[test]
    fn chunks_carry_lines_and_entry_ids() {
        let d = "# T\n\n## Low\n\nB-42 status closes when typing. Long text.\n\n## Decision\nchose A\n";
        let c = split(d);
        assert_eq!(c[0].line, 5);
        assert_eq!(c[0].heading_path, "T > Low > B-42");
        assert_eq!(c[1].line, 8);
        assert_eq!(c[1].heading_path, "T > Decision");
        assert_eq!(entry_id("- **F-S0b** done"), Some("F-S0b"));
        assert_eq!(entry_id("Result is"), None);
        assert_eq!(entry_id("UTF-8 text"), None);
    }

    #[test]
    fn table_pieces_repeat_the_header() {
        let mut d = String::from("| id | what |\n|---|---|\n");
        for i in 0..120 {
            d.push_str(&format!("| R-{i:02} | row {i} with some words to fill the budget quickly |\n"));
        }
        let c = split(&d);
        assert!(c.len() > 1);
        for ch in &c[1..] {
            assert!(ch.text.starts_with("| id | what |\n|---|---|\n"), "{}", &ch.text[..60]);
        }
        assert!(c[1].line > 2);
    }
}

#[cfg(test)]
mod end_tests {
    use super::*;

    #[test]
    fn chunks_know_their_last_line() {
        let bullet = |i: usize| format!("- Bullet {i} {}.", "word ".repeat(18).trim_end());
        let rejected: Vec<String> = (1..=8).map(bullet).collect();
        let note = format!("# T\n\n## Decision\nOne line.\n\n## Rejected\n{}\n\n## Measured\nM.\n", rejected.join("\n"));
        let cs = split(&note);
        let rej: Vec<&Chunk> = cs.iter().filter(|c| c.heading_path.ends_with("Rejected")).collect();
        assert!(rej.len() >= 2, "{rej:?}");
        for c in &rej {
            // Every bullet a chunk holds lies inside its range.
            for (i, l) in note.lines().enumerate() {
                let n = i as u32 + 1;
                if l.starts_with("- Bullet") && c.text.contains(&l[2..12]) {
                    assert!(c.line <= n && n <= c.end, "{c:?} misses line {n}");
                }
            }
        }
        let m = cs.iter().find(|c| c.heading_path.ends_with("Measured")).unwrap();
        assert_eq!((m.line, m.end), (17, 17));
    }
}
