//! A compact Gemma tokenizer (SentencePiece-style BPE with byte fallback).
//!
//! The reference `tokenizers` crate parses the 32 MB `tokenizer.json` into
//! hash maps and costs about 380 MB resident. This module converts that file
//! once into `tokenizer.bin`, a few contiguous arrays (about 14 MB), and
//! encodes with binary searches over them: no hash maps, no per-token
//! allocations beyond the output.
//!
//! Behaviour matches the reference for ordinary text. One deliberate
//! difference: special tokens such as `<bos>` written inside a note are
//! encoded as text, never as control tokens. Memory is data.

use crate::{err, Result};
use std::collections::BinaryHeap;
use std::path::Path;

const MAGIC: &[u8; 4] = b"MCTK";
const VERSION: u32 = 1;
const SPACE_PIECE: &str = "\u{2581}";

/// The tokenizer tables, backed by one buffer.
pub struct GemmaTokenizer {
    buf: Vec<u8>,
    n_vocab: usize,
    offsets: usize,
    pieces: usize,
    sorted_ids: usize,
    n_merges: usize,
    merges: usize,
    byte_ids: usize,
    pub bos: u32,
    pub eos: u32,
}

#[inline]
fn u32_at(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]])
}

impl GemmaTokenizer {
    /// Loads `tokenizer.bin` next to `tokenizer.json`, building it on first use.
    pub fn load(json_path: &Path) -> Result<GemmaTokenizer> {
        let bin_path = json_path.with_extension("bin");
        let buf = match std::fs::read(&bin_path) {
            Ok(b) if b.len() > 8 && &b[..4] == MAGIC && u32_at(&b, 4) == VERSION => b,
            _ => {
                let b = build(json_path)?;
                let tmp = bin_path.with_extension("bin.part");
                std::fs::write(&tmp, &b)?;
                std::fs::rename(&tmp, &bin_path)?;
                b
            }
        };
        Self::from_buf(buf)
    }

    fn from_buf(buf: Vec<u8>) -> Result<GemmaTokenizer> {
        let mut p = 8;
        let n_vocab = u32_at(&buf, p) as usize;
        p += 4;
        let pieces_len = u32_at(&buf, p) as usize;
        p += 4;
        let offsets = p;
        p += (n_vocab + 1) * 4;
        let pieces = p;
        p += pieces_len;
        let sorted_ids = p;
        p += n_vocab * 4;
        let n_merges = u32_at(&buf, p) as usize;
        p += 4;
        let merges = p;
        p += n_merges * 16;
        let byte_ids = p;
        p += 256 * 4;
        let bos = u32_at(&buf, p);
        let eos = u32_at(&buf, p + 4);
        p += 8;
        if p != buf.len() {
            return Err("tokenizer.bin is corrupt; delete it to rebuild".into());
        }
        Ok(GemmaTokenizer { buf, n_vocab, offsets, pieces, sorted_ids, n_merges, merges, byte_ids, bos, eos })
    }

    #[inline]
    fn piece(&self, id: u32) -> &[u8] {
        let a = u32_at(&self.buf, self.offsets + id as usize * 4) as usize;
        let b = u32_at(&self.buf, self.offsets + id as usize * 4 + 4) as usize;
        &self.buf[self.pieces + a..self.pieces + b]
    }

    /// Id of an exact piece, by binary search over the piece-sorted id list.
    pub fn piece_id(&self, bytes: &[u8]) -> Option<u32> {
        let (mut lo, mut hi) = (0usize, self.n_vocab);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let id = u32_at(&self.buf, self.sorted_ids + mid * 4);
            match self.piece(id).cmp(bytes) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(id),
            }
        }
        None
    }

    /// `(rank, merged_id)` for an adjacent pair, by binary search over the
    /// key-sorted merge table.
    #[inline]
    fn merge(&self, left: u32, right: u32) -> Option<(u32, u32)> {
        let key = ((left as u64) << 32) | right as u64;
        let (mut lo, mut hi) = (0usize, self.n_merges);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let at = self.merges + mid * 16;
            let k = ((u32_at(&self.buf, at) as u64) << 32) | u32_at(&self.buf, at + 4) as u64;
            if k < key {
                lo = mid + 1;
            } else if k > key {
                hi = mid;
            } else {
                return Some((u32_at(&self.buf, at + 8), u32_at(&self.buf, at + 12)));
            }
        }
        None
    }

    /// Encodes `text` as `<bos> ... <eos>`, at most `max_len` ids. `out` is
    /// cleared first; `scratch` is reused between calls.
    pub fn encode(&self, text: &str, max_len: usize, out: &mut Vec<u32>, scratch: &mut Scratch) {
        out.clear();
        scratch.norm.clear();
        for c in text.chars() {
            if c == ' ' {
                scratch.norm.push_str(SPACE_PIECE);
            } else {
                scratch.norm.push(c);
            }
        }
        // Initial symbols: one per char, byte fallback for chars not in the vocab.
        let syms = &mut scratch.syms;
        syms.clear();
        let mut tmp = [0u8; 4];
        for c in scratch.norm.chars() {
            let s = c.encode_utf8(&mut tmp);
            match self.piece_id(s.as_bytes()) {
                Some(id) => syms.push(Sym { id, prev: u32::MAX, next: u32::MAX }),
                None => {
                    for &b in s.as_bytes() {
                        let id = u32_at(&self.buf, self.byte_ids + b as usize * 4);
                        syms.push(Sym { id, prev: u32::MAX, next: u32::MAX });
                    }
                }
            }
        }
        let n = syms.len();
        for (i, s) in syms.iter_mut().enumerate() {
            s.prev = if i == 0 { u32::MAX } else { i as u32 - 1 };
            s.next = if i + 1 == n { u32::MAX } else { i as u32 + 1 };
        }
        // Merge by rank with a lazily invalidated heap.
        let heap = &mut scratch.heap;
        heap.clear();
        for i in 0..n.saturating_sub(1) {
            if let Some((rank, merged)) = self.merge(syms[i].id, syms[i + 1].id) {
                heap.push(Pending { rank, pos: i as u32, left: syms[i].id, right: syms[i + 1].id, merged });
            }
        }
        while let Some(p) = heap.pop() {
            let i = p.pos as usize;
            let next = syms[i].next;
            if syms[i].id != p.left || next == u32::MAX || syms[next as usize].id != p.right {
                continue; // stale
            }
            syms[i].id = p.merged;
            let after = syms[next as usize].next;
            syms[next as usize].id = u32::MAX; // dead
            syms[i].next = after;
            if after != u32::MAX {
                syms[after as usize].prev = i as u32;
                if let Some((rank, merged)) = self.merge(p.merged, syms[after as usize].id) {
                    heap.push(Pending { rank, pos: i as u32, left: p.merged, right: syms[after as usize].id, merged });
                }
            }
            let prev = syms[i].prev;
            if prev != u32::MAX {
                if let Some((rank, merged)) = self.merge(syms[prev as usize].id, p.merged) {
                    heap.push(Pending { rank, pos: prev, left: syms[prev as usize].id, right: p.merged, merged });
                }
            }
        }
        out.push(self.bos);
        let budget = max_len.saturating_sub(2);
        let mut i = if n == 0 { u32::MAX } else { 0 };
        while i != u32::MAX && out.len() - 1 < budget {
            out.push(syms[i as usize].id);
            i = syms[i as usize].next;
        }
        out.push(self.eos);
    }
}

#[derive(Clone, Copy)]
struct Sym {
    id: u32,
    prev: u32,
    next: u32,
}

#[derive(PartialEq, Eq)]
struct Pending {
    rank: u32,
    pos: u32,
    left: u32,
    right: u32,
    merged: u32,
}

// Lowest rank first, then leftmost position, as the reference does.
impl Ord for Pending {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.rank.cmp(&self.rank).then(o.pos.cmp(&self.pos))
    }
}
impl PartialOrd for Pending {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// Buffers reused across `encode` calls.
#[derive(Default)]
pub struct Scratch {
    norm: String,
    syms: Vec<Sym>,
    heap: BinaryHeap<Pending>,
}

/// Converts `tokenizer.json` into the binary table. Runs once.
fn build(json_path: &Path) -> Result<Vec<u8>> {
    use std::collections::HashMap;

    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Merge {
        Pair(String, String),
        Line(String),
    }
    #[derive(serde::Deserialize)]
    struct Model {
        vocab: HashMap<String, u32>,
        merges: Vec<Merge>,
    }
    #[derive(serde::Deserialize)]
    struct File {
        model: Model,
    }

    let text = std::fs::read_to_string(json_path).map_err(|e| err!("{}: {e}", json_path.display()))?;
    let file: File = serde_json::from_str(&text).map_err(|e| err!("tokenizer.json: {e}"))?;
    drop(text);
    let vocab = file.model.vocab;
    let n = vocab.len();
    let mut pieces: Vec<&str> = vec![""; n];
    for (p, &id) in &vocab {
        if id as usize >= n {
            return Err("tokenizer.json: vocab ids are not dense".into());
        }
        pieces[id as usize] = p;
    }
    let mut sorted: Vec<u32> = (0..n as u32).collect();
    sorted.sort_by(|&a, &b| pieces[a as usize].as_bytes().cmp(pieces[b as usize].as_bytes()));

    let mut merges: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(file.model.merges.len());
    for (rank, m) in file.model.merges.iter().enumerate() {
        let (l, r) = match m {
            Merge::Pair(l, r) => (l.as_str(), r.as_str()),
            Merge::Line(s) => s.split_once(' ').ok_or_else(|| err!("bad merge line"))?,
        };
        let merged = format!("{l}{r}");
        let (Some(&li), Some(&ri), Some(&mi)) = (vocab.get(l), vocab.get(r), vocab.get(merged.as_str())) else {
            continue;
        };
        merges.push((li, ri, rank as u32, mi));
    }
    merges.sort_by_key(|m| ((m.0 as u64) << 32) | m.1 as u64);

    let get = |s: &str| vocab.get(s).copied().ok_or_else(|| err!("tokenizer.json has no '{s}' token"));
    let bos = get("<bos>")?;
    let eos = get("<eos>")?;
    let unk = get("<unk>")?;
    let mut byte_ids = [unk; 256];
    for (b, slot) in byte_ids.iter_mut().enumerate() {
        if let Some(&id) = vocab.get(&format!("<0x{b:02X}>")) {
            *slot = id;
        }
    }

    let pieces_len: usize = pieces.iter().map(|p| p.len()).sum();
    let mut out = Vec::with_capacity(8 + 8 + (n + 1) * 4 + pieces_len + n * 4 + 4 + merges.len() * 16 + 256 * 4 + 8);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(n as u32).to_le_bytes());
    out.extend_from_slice(&(pieces_len as u32).to_le_bytes());
    let mut off = 0u32;
    for p in &pieces {
        out.extend_from_slice(&off.to_le_bytes());
        off += p.len() as u32;
    }
    out.extend_from_slice(&off.to_le_bytes());
    for p in &pieces {
        out.extend_from_slice(p.as_bytes());
    }
    for id in &sorted {
        out.extend_from_slice(&id.to_le_bytes());
    }
    out.extend_from_slice(&(merges.len() as u32).to_le_bytes());
    for m in &merges {
        out.extend_from_slice(&m.0.to_le_bytes());
        out.extend_from_slice(&m.1.to_le_bytes());
        out.extend_from_slice(&m.2.to_le_bytes());
        out.extend_from_slice(&m.3.to_le_bytes());
    }
    for id in &byte_ids {
        out.extend_from_slice(&id.to_le_bytes());
    }
    out.extend_from_slice(&bos.to_le_bytes());
    out.extend_from_slice(&eos.to_le_bytes());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_path() -> Option<std::path::PathBuf> {
        let p = super::super::download::model_dir().join("tokenizer.json");
        p.exists().then_some(p)
    }

    /// Parity with the reference implementation, when the model files are present.
    #[test]
    fn matches_reference_tokenizer() {
        let Some(path) = json_path() else {
            eprintln!("skipped: no tokenizer.json downloaded");
            return;
        };
        let ours = GemmaTokenizer::load(&path).unwrap();
        let theirs = tokenizers::Tokenizer::from_file(&path).unwrap();
        let samples = [
            "task: search result | query: which bug tracked the picture orientation problem",
            "title: Decisions > Status export format | text: We rotated the status JPEG export 90 degrees because the GL framebuffer is landscape. Tried CropRect first, rejected: it clipped the overlay. Fixed in B-42, file ui/status/StatusGl.kt.",
            "Hindi: नोटो सैन्स देवनागरी ठीक से रेंडर होता है लेकिन कर्निंग गलत है।",
            "code:\n```rust\nfn main() { let x = snake_case_fn(2026-10-18); }\n```\n\n\n   indented   spaces\t\ttabs",
            "emoji 🚀🧠 and symbols © ∑ ≠ → «quotes» and a zero-width\u{200b}joiner",
            "",
            " ",
            "a",
            "the the the the the the the the the the",
        ];
        let mut out = Vec::new();
        let mut scratch = Scratch::default();
        for s in samples {
            ours.encode(s, 4096, &mut out, &mut scratch);
            let reference = theirs.encode(s, true).unwrap();
            assert_eq!(out, reference.get_ids(), "mismatch for {s:?}");
        }
        // Truncation keeps bos and eos.
        ours.encode(samples[1], 10, &mut out, &mut scratch);
        assert_eq!(out.len(), 10);
        assert_eq!(out[0], ours.bos);
        assert_eq!(*out.last().unwrap(), ours.eos);
    }
}
