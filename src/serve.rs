//! A warm embedding server over a Unix socket, and the client that uses it.
//!
//! `memorycastle serve --idle 5m` holds one loaded model. A CLI search that
//! needs a vector asks the server instead of loading its own copy, so heavy
//! sessions pay for the weights once. The protocol is one JSON line per
//! request (`{"texts":[...]}`) and one per reply (`{"vectors":[[...]]}`).
//! The server embeds; search logic stays in the caller.

use crate::embed::Embedder;
use crate::{err, sys, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// Unix socket paths are limited to about 100 bytes, so the socket lives in
/// the system temp dir, named by a hash of the data dir so two homes do not
/// share one server.
pub fn socket_path() -> PathBuf {
    let home = sys::data_dir();
    let id = crate::project::project_id_of(&home.to_string_lossy());
    std::env::temp_dir().join(format!("memorycastle-{:x}.sock", id as u32))
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Request {
    texts: Vec<String>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Reply {
    #[serde(skip_serializing_if = "Option::is_none")]
    vectors: Option<Vec<Vec<f32>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Starts `memorycastle serve --idle 30m` as a detached child when no server
/// answers on the socket. Returns whether one was started. The child owns
/// its process group and has no terminal, so the shell that called this
/// (an agent hook) returns at once.
pub fn spawn_detached() -> Result<bool> {
    if RemoteEmbedder::connect(crate::limits::EMBED_DIMS).is_some() {
        return Ok(false);
    }
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["serve", "--idle", "30m"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map_err(|e| err!("serve: {e}"))?;
    Ok(true)
}

/// Runs until `idle` passes with no request. The model is loaded once, up front.
pub fn run(mut embedder: Box<dyn Embedder>, idle: Duration) -> Result<()> {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(&path)?;
    let (tx, rx) = mpsc::channel::<UnixStream>();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            if tx.send(s).is_err() {
                break;
            }
        }
    });
    let dims = embedder.dims();
    let mut floats: Vec<f32> = Vec::new();
    loop {
        let stream = match rx.recv_timeout(idle) {
            Ok(s) => s,
            Err(_) => break,
        };
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(req) => {
                floats.clear();
                let texts: Vec<&str> = req.texts.iter().map(String::as_str).collect();
                match embedder.embed(&texts, &mut floats) {
                    Ok(()) => Reply {
                        vectors: Some(floats.chunks_exact(dims).map(|c| c.to_vec()).collect()),
                        error: None,
                    },
                    Err(e) => Reply { vectors: None, error: Some(e.to_string()) },
                }
            }
            Err(e) => Reply { vectors: None, error: Some(e.to_string()) },
        };
        let mut out = serde_json::to_vec(&reply)?;
        out.push(b'\n');
        let mut stream = reader.into_inner();
        let _ = stream.write_all(&out);
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// An [`Embedder`] that talks to a running `serve`. `connect` returns `None`
/// quickly when no server is listening.
pub struct RemoteEmbedder {
    dims: usize,
}

impl RemoteEmbedder {
    pub fn connect(dims: usize) -> Option<RemoteEmbedder> {
        let path = socket_path();
        if !path.exists() {
            return None;
        }
        // Probe once; a stale socket file is removed.
        match UnixStream::connect(&path) {
            Ok(_) => Some(RemoteEmbedder { dims }),
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }
}

impl Embedder for RemoteEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }
    fn embed(&mut self, texts: &[&str], out: &mut Vec<f32>) -> Result<()> {
        let mut stream = UnixStream::connect(socket_path())?;
        stream.set_read_timeout(Some(Duration::from_secs(120)))?;
        let req = Request { texts: texts.iter().map(|t| t.to_string()).collect() };
        let mut body = serde_json::to_vec(&req)?;
        body.push(b'\n');
        stream.write_all(&body)?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        let reply: Reply = serde_json::from_str(&line)?;
        if let Some(e) = reply.error {
            return Err(err!("serve: {e}"));
        }
        let vectors = reply.vectors.unwrap_or_default();
        if vectors.len() != texts.len() {
            return Err("serve: wrong number of vectors".into());
        }
        for v in vectors {
            let mut v = v;
            crate::embed::truncate_normalise(&mut v, self.dims);
            out.extend_from_slice(&v);
        }
        Ok(())
    }
}
