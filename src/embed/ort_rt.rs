//! EmbeddingGemma 2 (text only, int8) through ONNX Runtime, loaded with
//! `dlopen` only on the path that needs a vector.

use super::download::{self, ModelVariant};
use super::tokenizer::{GemmaTokenizer, Scratch};
use super::{Embedder, SpaceDef};
use crate::{err, Result};
use ort::session::Session;
use ort::value::TensorRef;

/// Longest input in tokens. Chunks are at most 400 words; queries are short.
use crate::limits::EMBED_MAX_TOKENS as MAX_TOKENS;
/// Width of the (empty) media feature inputs the export still declares.
const MEDIA_DIM: i64 = 512;

/// Session knobs, exposed so the harness can measure them.
#[derive(Debug, Clone)]
pub struct OrtOptions {
    /// 0 = no graph optimisation, 1 = basic, 2 = extended, 3 = all.
    pub opt_level: u8,
    pub threads: usize,
    pub memory_pattern: bool,
    /// Let ONNX Runtime repack weights into kernel-friendly layouts (a copy).
    pub prepack: bool,
    /// Use the CPU memory arena (keeps freed activations for reuse).
    pub arena: bool,
    /// Execution provider: "cpu" (default) or "coreml" (macOS; GPU and Neural Engine).
    pub ep: String,
}

impl Default for OrtOptions {
    /// Defaults, each overridable for measurement with `MEMORYCASTLE_ORT_*`
    /// (`THREADS`, `OPT`, `MEMPATTERN`, `PREPACK`, `ARENA`).
    fn default() -> Self {
        let env = |k: &str| std::env::var(format!("MEMORYCASTLE_ORT_{k}")).ok();
        let flag = |k: &str, d: bool| env(k).map(|v| v != "0").unwrap_or(d);
        let threads = env("THREADS")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(crate::limits::ORT_THREADS).min(4));
        OrtOptions {
            opt_level: env("OPT").and_then(|v| v.parse().ok()).unwrap_or(1),
            threads,
            memory_pattern: flag("MEMPATTERN", true),
            prepack: flag("PREPACK", true),
            arena: flag("ARENA", true),
            ep: env("EP").unwrap_or_else(|| "cpu".into()),
        }
    }
}

pub struct OrtEmbedder {
    session: Session,
    tokenizer: GemmaTokenizer,
    dims: usize,
    /// Scratch buffers reused across calls.
    scratch: Scratch,
    tokens: Vec<Vec<u32>>,
    ids: Vec<i64>,
    mask: Vec<i64>,
}

impl OrtEmbedder {
    pub fn open(def: &SpaceDef, quiet: bool) -> Result<OrtEmbedder> {
        Self::open_with(def, quiet, &OrtOptions::default())
    }

    pub fn open_with(def: &SpaceDef, quiet: bool, opts: &OrtOptions) -> Result<OrtEmbedder> {
        let dylib = download::ensure_runtime(quiet)?;
        let dir = download::model_dir();
        let variant = ModelVariant::from_precision(&def.precision)
            .ok_or_else(|| err!("unknown model precision '{}' (int8, q4, q4f16)", def.precision))?;
        let model = download::ensure_file(&dir, &variant.graph_name(), &variant.graph(), quiet)?;
        download::ensure_file(&dir, &variant.data_name(), &variant.data(), quiet)?;
        let tok_path = download::ensure_file(&dir, "tokenizer.json", &download::TOKENIZER, quiet)?;

        // ONNX Runtime allows one environment per process: a model opened
        // again (the MCP server's second cold search, `--also`, save then
        // search) reuses it instead of failing.
        static ENV: std::sync::Mutex<Option<ort::environment::Environment>> = std::sync::Mutex::new(None);
        let mut slot = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let env = match slot.as_ref() {
            Some(env) => env.clone(),
            None => {
                let env = ort::init_from(&dylib)
                    .map_err(|e| err!("load ONNX Runtime from {}: {e}", dylib.display()))?
                    .with_name("memorycastle")
                    .build()
                    .map_err(|e| err!("ONNX Runtime init: {e}"))?;
                slot.insert(env).clone()
            }
        };
        drop(slot);
        use ort::session::builder::GraphOptimizationLevel as G;
        let level = match opts.opt_level {
            0 => G::Disable,
            1 => G::Level1,
            2 => G::Level2,
            _ => G::Level3,
        };
        let mut builder = Session::builder(&env)
            .map_err(|e| err!("{e}"))?
            .with_intra_threads(opts.threads)
            .map_err(|e| err!("{e}"))?
            .with_optimization_level(level)
            .map_err(|e| err!("{e}"))?
            .with_memory_pattern(opts.memory_pattern)
            .map_err(|e| err!("{e}"))?;
        if opts.ep == "coreml" {
            #[cfg(target_os = "macos")]
            {
                use ort::ep::coreml::{CoreML, ComputeUnits, ModelFormat};
                let ep = CoreML::default()
                    .with_model_format(ModelFormat::MLProgram)
                    .with_compute_units(ComputeUnits::All)
                    .with_model_cache_dir(download::model_dir().join("coreml-cache").to_string_lossy())
                    .build();
                builder = builder.with_execution_providers([ep]).map_err(|e| err!("coreml: {e}"))?;
            }
            #[cfg(not(target_os = "macos"))]
            return Err("coreml execution provider is only available on macOS".into());
        }
        if !opts.prepack {
            builder = builder.with_config_entry("session.disable_prepacking", "1").map_err(|e| err!("{e}"))?;
        }
        if !opts.arena {
            use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
            let info = MemoryInfo::new(AllocationDevice::CPU, 0, AllocatorType::Device, MemoryType::Default)
                .map_err(|e| err!("{e}"))?;
            builder = builder.with_allocator(info).map_err(|e| err!("{e}"))?;
        }
        let session = builder
            .commit_from_file(&model)
            .map_err(|e| err!("load model {}: {e}", model.display()))?;

        let tokenizer = GemmaTokenizer::load(&tok_path)?;
        Ok(OrtEmbedder {
            session,
            tokenizer,
            dims: def.dims,
            scratch: Scratch::default(),
            tokens: Vec::new(),
            ids: Vec::new(),
            mask: Vec::new(),
        })
    }
}

impl Embedder for OrtEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }

    fn embed(&mut self, texts: &[&str], out: &mut Vec<f32>) -> Result<()> {
        if texts.is_empty() {
            return Ok(());
        }
        let batch = texts.len();
        self.tokens.resize_with(batch, Vec::new);
        for (t, buf) in texts.iter().zip(self.tokens.iter_mut()) {
            self.tokenizer.encode(t, MAX_TOKENS, buf, &mut self.scratch);
        }
        let width = self.tokens[..batch].iter().map(Vec::len).max().unwrap_or(1).max(1);
        self.ids.clear();
        self.mask.clear();
        self.ids.resize(batch * width, 0);
        self.mask.resize(batch * width, 0);
        for (b, toks) in self.tokens[..batch].iter().enumerate() {
            let row = b * width;
            for (i, &id) in toks.iter().enumerate() {
                self.ids[row + i] = id as i64;
                self.mask[row + i] = 1;
            }
        }
        let shape = [batch as i64, width as i64];
        let ids = TensorRef::from_array_view((shape, self.ids.as_slice())).map_err(|e| err!("{e}"))?;
        let mask = TensorRef::from_array_view((shape, self.mask.as_slice())).map_err(|e| err!("{e}"))?;
        // The export keeps the multimodal merge; text-only means zero media tokens.
        // Rows of media features. Zero is the honest value; some execution
        // providers refuse zero-element inputs, so a provider may ask for one
        // zero row instead (no media token in input_ids means nothing is merged).
        let rows: i64 = std::env::var("MEMORYCASTLE_MEDIA_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let none: Vec<f32> = vec![0.0; (rows * MEDIA_DIM) as usize];
        let none = none.as_slice();
        let image = TensorRef::from_array_view(([rows, MEDIA_DIM], none)).map_err(|e| err!("{e}"))?;
        let video = TensorRef::from_array_view(([rows, MEDIA_DIM], none)).map_err(|e| err!("{e}"))?;
        let audio = TensorRef::from_array_view(([rows, MEDIA_DIM], none)).map_err(|e| err!("{e}"))?;
        let outputs = self
            .session
            .run(ort::inputs![
                "input_ids" => ids,
                "attention_mask" => mask,
                "image_features" => image,
                "video_features" => video,
                "audio_features" => audio
            ])
            .map_err(|e| err!("inference: {e}"))?;
        let value = outputs
            .get("sentence_embedding")
            .ok_or_else(|| err!("model has no sentence_embedding output"))?;
        let (shape, data) = value.try_extract_tensor::<f32>().map_err(|e| err!("{e}"))?;
        let full = shape.last().copied().unwrap_or(0) as usize;
        if full < self.dims || data.len() != batch * full {
            return Err(err!("unexpected embedding shape {:?}", shape));
        }
        for b in 0..batch {
            let row = &data[b * full..b * full + self.dims];
            let norm = row.iter().map(|x| x * x).sum::<f32>().sqrt();
            if !norm.is_finite() || norm == 0.0 {
                return Err("model produced a non-finite vector".into());
            }
            out.extend(row.iter().map(|x| x / norm));
        }
        Ok(())
    }
}
