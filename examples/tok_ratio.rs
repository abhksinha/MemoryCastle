//! Bytes per token of the built-in Gemma tokenizer for each file given, and
//! the saving in bytes versus in tokens for a served/stood-for pair.
use memorycastle::embed::tokenizer::{GemmaTokenizer, Scratch};
fn main() {
    let t = GemmaTokenizer::load(&memorycastle::embed::download::model_dir().join("tokenizer.json")).unwrap();
    let mut scratch = Scratch::default();
    let mut tot = Vec::new();
    for p in std::env::args().skip(1) {
        let text = std::fs::read_to_string(&p).unwrap_or_default();
        let mut toks = Vec::new();
        t.encode(&text, usize::MAX, &mut toks, &mut scratch);
        println!("{p}: {} bytes, {} tokens, {:.2} bytes/token", text.len(), toks.len(), text.len() as f64 / toks.len().max(1) as f64);
        tot.push((text.len(), toks.len()));
    }
    if tot.len() == 2 {
        let (sb, st) = tot[0];
        let (fb, ft) = tot[1];
        println!("saving by bytes {:.1}%  by tokens {:.1}%", 100.0 * (fb - sb) as f64 / fb as f64, 100.0 * (ft - st) as f64 / ft as f64);
    }
}
