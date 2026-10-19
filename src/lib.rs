//! memorycastle: a per-project memory of decisions, rejected attempts,
//! measurements and preferences, stored in one SQLite file and searched
//! without an LLM.
//!
//! Module map:
//! - [`db`]      opening the store, schema, migrations
//! - [`project`] project identity from the git remote, the `projects` table, [`project::Scope`]
//! - [`chunk`]   heading chunking and the `text_indexed` normalisation
//! - [`fts`]     the quoted FTS5 query builder
//! - [`store`]   save, forget, status, reindex
//! - [`search`]  BM25, vector scan, reciprocal rank fusion, traces
//! - [`embed`]   the `Embedder` trait, spaces, the query cache, the ONNX runtime
//! - [`eval`]    the gold-set harness
//! - [`mcp`]     the stdio MCP server
//! - [`serve`]   the warm embedding server and its client

pub mod chunk;
pub mod code;
pub mod db;
pub mod embed;
pub mod error;
pub mod eval;
pub mod fts;
pub mod graph;
pub mod importlog;
pub mod install;
pub mod limits;
pub mod mcp;
pub mod draft;
pub mod project;
pub mod quality;
pub mod render;
pub mod search;
pub mod spans;
pub mod secrets;
pub mod serve;
pub mod store;
pub mod sys;

pub use error::{Error, Result};
pub use project::Repo;
pub mod app;
