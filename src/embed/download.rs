//! First-use downloads of the model files and the ONNX Runtime library, each
//! streamed to disk while hashed, verified against a pinned SHA-256, then
//! renamed into place. A failed or partial download leaves only a `.part` file.

use crate::{err, sys, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct Pinned {
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// The quantised exports of EmbeddingGemma 2 that `memorycastle` knows how
/// to fetch. All are `onnx-community/embeddinggemma-2-ONNX` files: weights
/// stay packed at run time (`MatMulNBits`, `GatherBlockQuantized`), so the
/// file size is close to the resident size before prepacking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelVariant {
    /// 8-bit weights, 314 MB.
    Int8,
    /// 4-bit weights, fp32 activations, 174 MB.
    Q4,
    /// 4-bit weights, fp16 activations, 157 MB.
    Q4F16,
}

impl ModelVariant {
    pub fn from_precision(p: &str) -> Option<ModelVariant> {
        match p {
            "int8" => Some(ModelVariant::Int8),
            "q4" => Some(ModelVariant::Q4),
            "q4f16" => Some(ModelVariant::Q4F16),
            _ => None,
        }
    }

    pub fn precision(self) -> &'static str {
        match self {
            ModelVariant::Int8 => "int8",
            ModelVariant::Q4 => "q4",
            ModelVariant::Q4F16 => "q4f16",
        }
    }

    fn stem(self) -> &'static str {
        match self {
            ModelVariant::Int8 => "model_quantized",
            ModelVariant::Q4 => "model_q4",
            ModelVariant::Q4F16 => "model_q4f16",
        }
    }

    pub fn graph_name(self) -> String {
        format!("{}.onnx", self.stem())
    }

    pub fn data_name(self) -> String {
        format!("{}.onnx_data", self.stem())
    }

    pub fn graph(self) -> Pinned {
        match self {
            ModelVariant::Int8 => Pinned {
                url: "https://huggingface.co/onnx-community/embeddinggemma-2-ONNX/resolve/main/onnx/model_quantized.onnx",
                sha256: "d06edd601f851c633a2519304cbeb8dc6170d7ceb61b436625c17fb9b6e74953",
                size: 495_165,
            },
            ModelVariant::Q4 => Pinned {
                url: "https://huggingface.co/onnx-community/embeddinggemma-2-ONNX/resolve/main/onnx/model_q4.onnx",
                sha256: "f9eeba97acddf139b8ee2ddf04bc30dceafa88de93fadf74d7644e0d61a477a9",
                size: 490_742,
            },
            ModelVariant::Q4F16 => Pinned {
                url: "https://huggingface.co/onnx-community/embeddinggemma-2-ONNX/resolve/main/onnx/model_q4f16.onnx",
                sha256: "53feeced79582d661e30adeaa9829cea90d92b1c78347e26fd123773580f71d0",
                size: 495_298,
            },
        }
    }

    pub fn data(self) -> Pinned {
        match self {
            ModelVariant::Int8 => Pinned {
                url: "https://huggingface.co/onnx-community/embeddinggemma-2-ONNX/resolve/main/onnx/model_quantized.onnx_data",
                sha256: "278a7ff1248c3618e4bd11a607fc54f7bdc7778854230f3956d3f86bd9db4f3b",
                size: 313_724_928,
            },
            ModelVariant::Q4 => Pinned {
                url: "https://huggingface.co/onnx-community/embeddinggemma-2-ONNX/resolve/main/onnx/model_q4.onnx_data",
                sha256: "c3975f2d1ab7a1878ae31a7d7a9b7804a827aff3800b60dfceafce21cac3df49",
                size: 174_028_800,
            },
            ModelVariant::Q4F16 => Pinned {
                url: "https://huggingface.co/onnx-community/embeddinggemma-2-ONNX/resolve/main/onnx/model_q4f16.onnx_data",
                sha256: "c39fbaa1fb4221f04beb82786a06b81999c514fd39e84fcd58ef79413c87fa56",
                size: 156_862_464,
            },
        }
    }
}

pub const TOKENIZER: Pinned = Pinned {
    url: "https://huggingface.co/onnx-community/embeddinggemma-2-ONNX/resolve/main/tokenizer.json",
    sha256: "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4",
    size: 32_170_510,
};

pub const ORT_VERSION: &str = "1.31.0";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const ORT_ARCHIVE: Pinned = Pinned {
    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.0/onnxruntime-osx-arm64-1.31.0.tgz",
    sha256: "1031b36dd3bdaa8d976a6130d9cf2a05e9be320b93ae804f8427b91dc3432afc",
    size: 36_549_009,
};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const ORT_ARCHIVE: Pinned = Pinned {
    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.0/onnxruntime-linux-x64-1.31.0.tgz",
    sha256: "cc5c72baf5ae5c8238a6841f0897227be2d02826b9cf98eaf02fdefaeeb03a57",
    size: 11_408_003,
};
#[cfg(not(any(
    all(target_os = "macos", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
pub const ORT_ARCHIVE: Pinned = Pinned { url: "", sha256: "", size: 0 };

/// One directory for every variant: the graph and data files have distinct
/// names and the tokenizer is shared.
pub fn model_dir() -> PathBuf {
    sys::data_dir().join("models/embeddinggemma-2")
}

pub fn runtime_dir() -> PathBuf {
    sys::data_dir().join(format!("runtime/onnxruntime-{ORT_VERSION}"))
}

/// The dynamic library name inside the extracted archive.
pub fn dylib_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

/// Ensures `dir/name` exists with the pinned size, downloading if needed.
/// Size is the cheap check for a file already on disk; the hash is verified
/// once, at download time.
pub fn ensure_file(dir: &Path, name: &str, pin: &Pinned, quiet: bool) -> Result<PathBuf> {
    let path = dir.join(name);
    if let Ok(md) = std::fs::metadata(&path) {
        if md.len() == pin.size {
            return Ok(path);
        }
    }
    if pin.url.is_empty() {
        return Err("no pinned download for this platform; set MEMORYCASTLE_ORT_DYLIB or build with another runtime".into());
    }
    std::fs::create_dir_all(dir)?;
    let part = dir.join(format!("{name}.part"));
    if !quiet {
        eprintln!("memorycastle: downloading {name} ({:.1} MB)", pin.size as f64 / 1e6);
    }
    let digest = download_to(pin.url, &part)?;
    if digest != pin.sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(err!("{name}: checksum mismatch (got {digest}); download removed"));
    }
    std::fs::rename(&part, &path)?;
    Ok(path)
}

/// Streams `url` into `dest`, returning the hex SHA-256 of the bytes written.
fn download_to(url: &str, dest: &Path) -> Result<String> {
    let resp = ureq::get(url)
        .header("User-Agent", "memorycastle")
        .call()
        .map_err(|e| err!("download {url}: {e}"))?;
    let mut body = resp.into_body().into_reader();
    let mut file = std::io::BufWriter::new(std::fs::File::create(dest)?);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }
    file.flush()?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Returns the path of the ONNX Runtime dynamic library, honouring
/// `MEMORYCASTLE_ORT_DYLIB`, else downloading the pinned release once.
pub fn ensure_runtime(quiet: bool) -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("MEMORYCASTLE_ORT_DYLIB") {
        return Ok(PathBuf::from(p));
    }
    let dir = runtime_dir();
    let lib = dir.join(dylib_name());
    if lib.exists() {
        return Ok(lib);
    }
    let archive = ensure_file(&dir, "archive.tgz", &ORT_ARCHIVE, quiet)?;
    let tmp = dir.join("extract");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&tmp)
        .status()?;
    if !status.success() {
        return Err("tar failed to extract the ONNX Runtime archive".into());
    }
    let found = find_file(&tmp, dylib_name()).ok_or_else(|| err!("{} not found in archive", dylib_name()))?;
    // Keep the real file, not the symlink, and the versioned sibling it points to.
    let real = std::fs::canonicalize(&found)?;
    std::fs::copy(&real, &lib)?;
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_file(&archive);
    Ok(lib)
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}
