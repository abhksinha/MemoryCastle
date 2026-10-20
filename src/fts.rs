//! The quoted FTS5 query builder.
//!
//! FTS5 syntax cannot take `B-42` or `StatusGl.kt` unquoted, so user text is
//! split exactly the way `unicode61 tokenchars '_-'` splits it and every term
//! becomes a phrase string. User text is never passed to `MATCH` raw.

/// Splits `text` into the terms the tokenizer would produce: runs of
/// alphanumerics plus `_` and `-`. Terms with no alphanumeric are dropped.
pub fn terms(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        let keep = c.is_alphanumeric() || c == '_' || c == '-';
        match (keep, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                push_term(&mut out, &text[s..i]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        push_term(&mut out, &text[s..]);
    }
    out
}

fn push_term<'a>(out: &mut Vec<&'a str>, t: &'a str) {
    if t.chars().any(char::is_alphanumeric) {
        out.push(t);
    }
}

/// `"B-42" OR "StatusGl" OR "kt"`: every term a phrase, embedded quotes
/// doubled, joined with OR so a question still ranks notes that hold only
/// some of its words (BM25 weighs the rare ones). Returns `None` when the
/// text has no searchable term.
pub fn build(text: &str) -> Option<String> {
    let ts = terms(text);
    if ts.is_empty() {
        return None;
    }
    let mut q = String::with_capacity(text.len() + ts.len() * 3);
    for (i, t) in ts.iter().enumerate() {
        if i > 0 {
            q.push_str(" OR ");
        }
        q.push('"');
        for c in t.chars() {
            if c == '"' {
                q.push('"');
            }
            q.push(c);
        }
        q.push('"');
        // A plain word also matches its other forms through its stem as a
        // prefix ("batching" finds "Batches"), beside the exact term so an
        // exact match still scores on both.
        let lower = t.to_ascii_lowercase();
        if t.chars().all(|c| c.is_ascii_alphabetic()) && !is_identifier(t) {
            let s = crate::search::stem(&lower);
            if s.len() < lower.len() {
                q.push_str(" OR \"");
                q.push_str(s);
                q.push_str("\"*");
            }
        }
    }
    Some(q)
}

/// True when a term looks like an identifier rather than a word: it has an
/// underscore, a hyphen next to a digit, a mix of letters and digits, or an
/// inner case change (`CropRect`).
pub fn is_identifier(term: &str) -> bool {
    let has_digit = term.bytes().any(|b| b.is_ascii_digit());
    let has_alpha = term.chars().any(char::is_alphabetic);
    if term.contains('_') {
        return true;
    }
    if has_digit && has_alpha {
        return true;
    }
    if term.contains('-') && has_digit {
        return true;
    }
    let mut prev_lower = false;
    for c in term.chars() {
        if c.is_uppercase() && prev_lower {
            return true;
        }
        prev_lower = c.is_lowercase();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_every_term() {
        assert_eq!(build("B-42").unwrap(), "\"B-42\"");
        assert_eq!(build("snake_case here").unwrap(), "\"snake_case\" OR \"here\"");
        assert_eq!(build("2026-10-18").unwrap(), "\"2026-10-18\"");
        assert_eq!(build("ui/status/StatusGl.kt").unwrap(), "\"ui\" OR \"status\" OR \"statu\"* OR \"StatusGl\" OR \"kt\"");
        assert_eq!(build("say \"hi\" now").unwrap(), "\"say\" OR \"hi\" OR \"now\"");
        assert_eq!(build("-- ... ???"), None);
        assert_eq!(build("").as_deref(), None);
    }

    #[test]
    fn a_plain_word_also_matches_by_its_stem() {
        assert_eq!(build("batching").unwrap(), "\"batching\" OR \"batch\"*");
        // Identifiers and short words stay exact.
        assert_eq!(build("embed_missing").unwrap(), "\"embed_missing\"");
        assert_eq!(build("uses").unwrap(), "\"uses\"");
    }

    #[test]
    fn identifiers() {
        assert!(is_identifier("B-42"));
        assert!(is_identifier("snake_case"));
        assert!(is_identifier("CropRect"));
        assert!(is_identifier("2026-10-18"));
        assert!(is_identifier("StatusGl"));
        assert!(!is_identifier("rotated"));
        assert!(!is_identifier("Mars"));
        assert!(!is_identifier("read-only"));
    }
}
