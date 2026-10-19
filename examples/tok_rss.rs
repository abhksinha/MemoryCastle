use memorycastle::embed::tokenizer::{GemmaTokenizer, Scratch};
use memorycastle::sys::current_rss_bytes as rss;
fn main() {
    let t0 = std::time::Instant::now();
    let t = GemmaTokenizer::load(&memorycastle::embed::download::model_dir().join("tokenizer.json")).unwrap();
    let load = t0.elapsed();
    let text = "We rotated the status JPEG export 90 degrees because the GL framebuffer is landscape. Tried CropRect first, rejected. ".repeat(20);
    let mut out = Vec::new();
    let mut sc = Scratch::default();
    let t1 = std::time::Instant::now();
    for _ in 0..100 { t.encode(&text, 4096, &mut out, &mut sc); }
    println!("tokenizer: load {:?}, rss {} MB, encode {} chars -> {} tokens in {:?}", load, rss() / 1_048_576, text.len(), out.len(), t1.elapsed() / 100);
}
