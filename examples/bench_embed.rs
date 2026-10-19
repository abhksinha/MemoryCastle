//! Measures load time, warm latency and RSS for ONNX session options.
//! Usage: bench_embed <opt_level> <threads> <memory_pattern 0|1> <prepack 0|1> <arena 0|1> [batch]
//! The model variant comes from MEMORYCASTLE_MODEL (int8, q4, q4f16).
use memorycastle::embed::ort_rt::{OrtEmbedder, OrtOptions};
use memorycastle::embed::{Embedder, SpaceDef};
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let opts = OrtOptions {
        opt_level: a.get(1).and_then(|s| s.parse().ok()).unwrap_or(1),
        threads: a.get(2).and_then(|s| s.parse().ok()).unwrap_or(4),
        memory_pattern: a.get(3).map(|s| s == "1").unwrap_or(true),
        prepack: a.get(4).map(|s| s == "1").unwrap_or(true),
        arena: a.get(5).map(|s| s == "1").unwrap_or(true),
        ep: std::env::var("MEMORYCASTLE_ORT_EP").unwrap_or_else(|_| "cpu".into()),
    };
    let batch_n: usize = a.get(6).and_then(|s| s.parse().ok()).unwrap_or(16);
    let def = SpaceDef::gemma2_default();
    let t0 = Instant::now();
    let mut e = OrtEmbedder::open_with(&def, true, &opts).unwrap();
    let load = t0.elapsed();
    let q = def.query_text("which bug tracked the picture orientation problem");
    let mut out = Vec::new();
    e.embed(&[q.as_str()], &mut out).unwrap();
    let t1 = Instant::now();
    for _ in 0..10 {
        out.clear();
        e.embed(&[q.as_str()], &mut out).unwrap();
    }
    let warm = t1.elapsed() / 10;
    let doc = def.doc_text("Decisions > Status export", &"We rotated the status JPEG export 90 degrees because the GL framebuffer is landscape. ".repeat(12));
    let docs: Vec<&str> = (0..batch_n).map(|_| doc.as_str()).collect();
    let after_load_rss = memorycastle::sys::peak_rss_bytes() / 1_048_576;
    let t2 = Instant::now();
    out.clear();
    e.embed(&docs, &mut out).unwrap();
    let batch = t2.elapsed();
    println!(
        "ep={} model={} opt={} thr={} mp={} prepack={} arena={} batch={} | load={:.0?} warm_query={:.1?} batch_embed={:.2?} | rss_after_queries={} MB peak_rss={} MB",
        opts.ep, def.precision, opts.opt_level, opts.threads, opts.memory_pattern as u8, opts.prepack as u8, opts.arena as u8, batch_n,
        load, warm, batch, after_load_rss,
        memorycastle::sys::peak_rss_bytes() / 1_048_576
    );
}
