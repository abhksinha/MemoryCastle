use clap::{Args, Parser, Subcommand};
use memorycastle::graph;
use memorycastle::app::{Context, AGENT_SNIPPET};
use memorycastle::project::{self, ScopeFlags};
use memorycastle::search::{GrepMemory, Mode, SearchOptions, StoreMemory};
use memorycastle::{db, embed, eval, mcp, serve, store, Result};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "memorycastle", version, about = "Per-project memory for coding agents: decisions, rejected attempts, measurements, preferences", after_help = "Full usage guide for agents, as Markdown:  memcastle guide")]
struct Cli {
    /// Act on a named project instead of the git checkout you are in.
    #[arg(long, global = true)]
    project: Option<String>,
    /// Print results as JSON.
    #[arg(long, global = true)]
    json: bool,
    /// Suppress download progress.
    #[arg(short, long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Default)]
struct ScopeArgs {
    /// Include sibling projects in the same group (ranked at 0.6).
    #[arg(long)]
    related: bool,
    /// Also search projects whose name matches this glob.
    #[arg(long = "in", value_name = "GLOB")]
    project_glob: Option<String>,
    /// Search every project.
    #[arg(long)]
    all: bool,
}

impl ScopeArgs {
    fn flags(&self) -> ScopeFlags {
        ScopeFlags { related: self.related, project_glob: self.project_glob.clone(), all: self.all }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Add or update curated notes in the current project.
    Save {
        files: Vec<PathBuf>,
        /// Save every note under memorycastle/journal/ that changed since its last save.
        #[arg(long)]
        changed: bool,
        /// Save even if the file looks like it holds secrets.
        #[arg(long)]
        force: bool,
        /// Do not embed now; `reindex` will.
        #[arg(long)]
        keyword: bool,
    },
    /// Keyword first, meaning search when it pays.
    Search {
        query: String,
        #[arg(short, default_value_t = 5)]
        k: usize,
        #[command(flatten)]
        scope: ScopeArgs,
        /// Always run the meaning channel.
        #[arg(long, conflicts_with = "keyword")]
        meaning: bool,
        /// Never load the model.
        #[arg(long)]
        keyword: bool,
        /// Show which channel found each result and why the model did or did not run.
        #[arg(long)]
        explain: bool,
        /// After each hit, print the note's Decision, Rejected and Measured sections (once per note, bounded).
        #[arg(long)]
        sections: bool,
        /// Only the first line per hit (confidence, note:line, section, date) and the read table; about 25 tokens per hit.
        #[arg(long, conflicts_with = "sections")]
        brief: bool,
        /// Print `read:` spans with absolute paths (default: repo-relative, absolute when run outside the repo root).
        #[arg(long)]
        abs: bool,
    },
    /// Remove a note and everything derived from it.
    Forget {
        /// Note path or id. Omit with --project-all to clear a whole project.
        target: Option<String>,
        /// Hard-delete instead of keeping the note text for undo.
        #[arg(long)]
        purge: bool,
        /// Remove the whole current project's memory.
        #[arg(long)]
        project_all: bool,
        /// Check that nothing derived remains anywhere in the store.
        #[arg(long)]
        verify: bool,
    },
    /// Register, list, group or relink projects.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Rebuild chunks and vectors from the notes.
    Reindex {
        /// Embed into a new space and swap it in when done.
        #[arg(long = "space", value_parser = ["new"])]
        space: Option<String>,
        /// Chunks and FTS only.
        #[arg(long)]
        keyword: bool,
    },
    /// Counts, active space, file size and notes changed on disk since saved.
    Status,
    /// Check journal notes for shape problems (missing sections, no files, thin decision) without saving.
    Lint {
        /// Notes to check; default every note under memorycastle/journal/.
        files: Vec<String>,
    },
    /// Score every note of this project: spread (several topics), overlap (duplicate), served but never opened.
    Quality {
        /// Only notes with a verdict.
        #[arg(long)]
        flagged: bool,
    },
    /// Print the usage guide for agents (docs/GUIDE.md): when to search, how to read the answer, how to write a note.
    Guide,
    /// One line when this session edited code after its last saved note (called by the hooks at compaction, after `git commit`, and at stop); silent otherwise.
    Owed {
        /// Exit 2 the first time per session for this event (compact, stop) so the hook can block once; exit 0 after.
        #[arg(long)]
        once: Option<String>,
        /// Mid-session: print only when enough was edited (files or minutes) and not within the cooldown (for the prompt hook).
        #[arg(long)]
        remind: bool,
    },
    /// Recall for a prompt (called by the prompt hook): one line per note the
    /// store is confident about, each once per session; nothing otherwise.
    #[command(hide = true)]
    Recall {
        /// The prompt text; read from stdin when absent.
        text: Option<String>,
    },
    /// Draft a note from this session's edits when none was saved (called by the stop hook).
    #[command(hide = true)]
    Draft,
    /// Declare the agent session for this repo (called by the session-start hook; no output).
    #[command(hide = true)]
    Session {
        id: String,
        /// The agent running this session (claude, codex, gemini): fills `Author:` on notes it saves.
        #[arg(long)]
        agent: Option<String>,
    },
    /// Before a whole-file read: print the spans a lookup served for this
    /// file that nothing has read yet, once, and exit 2 so the hook can
    /// redirect the agent to them (no output otherwise; keyword path).
    #[command(hide = true)]
    Guard {
        path: String,
    },
    /// Record a file edit reported by an agent hook (no output; keyword path).
    #[command(hide = true)]
    Edited {
        path: String,
    },
    /// Record a file read reported by an agent hook (no output; keyword path).
    #[command(hide = true)]
    Read {
        /// Path the agent read, relative to the repo root or absolute.
        path: String,
        /// Bytes the tool returned.
        #[arg(long)]
        bytes: usize,
        /// Line range read, `a-b`, when the tool read a range.
        #[arg(long)]
        lines: Option<String>,
    },
    /// Import file reads from an agent's session log (Codex, Antigravity): `sed -n 'a,bp' f`, `cat f`,
    /// `head -n N f`; a read is guided when the log shows a memorycastle span for that file before it.
    #[command(hide = true)]
    ImportLog {
        /// A transcript file; or none with --all.
        file: Option<String>,
        /// The file is a Claude Code transcript (`~/.claude/projects/<dir>/<id>.jsonl`): Read and Bash
        /// tool calls with their result sizes and timestamps; imported once per transcript.
        #[arg(long)]
        claude: bool,
        /// The file is a Codex rollout (`~/.codex/sessions/.../rollout-*.jsonl`).
        #[arg(long)]
        codex: bool,
        /// Every Claude Code and Codex transcript on this machine recorded in this repo.
        #[arg(long)]
        all: bool,
    },
    /// Keep the model warm for a session; exits after idle.
    Serve {
        /// Idle time before exit, e.g. 5m, 90s.
        #[arg(long, default_value = "5m")]
        idle: String,
        /// Start in the background and return; nothing if a server already answers.
        #[arg(long)]
        detach: bool,
    },
    /// Serve search, save, status and project over MCP on stdio.
    Mcp,
    /// Run the gold set and record recall@k, latency and peak RSS.
    Eval {
        /// Gold set JSON; default <data dir>/gold.json.
        #[arg(long)]
        gold: Option<PathBuf>,
        /// Run the plain-grep baseline instead of the store.
        #[arg(long)]
        baseline: Option<String>,
        #[arg(long, conflicts_with = "keyword")]
        meaning: bool,
        #[arg(long)]
        keyword: bool,
        /// Fail on the design gates (recall@10 drop, keyword p50, keyword RSS).
        #[arg(long)]
        gate: bool,
    },
    /// Write the stored notes of the current project back into the repo (missing or changed files only).
    Export {
        /// Directory to write into; default the repo root (notes keep their relative paths).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Notes linked to a codebase file, symbol or note (live first, superseded last).
    Links {
        /// A file path (`src/store.rs`, or just `store.rs`), a symbol (`embed_missing`), or a note path.
        target: String,
        /// With a file: the notes about the definition enclosing this line (then the file), not every note about the file.
        #[arg(long)]
        at: Option<u32>,
        /// Also print the constants the definition uses and the lines that call it.
        #[arg(long)]
        context: bool,
        /// Print nothing when this target was already looked up in this session (for hooks).
        #[arg(long)]
        once: bool,
    },
    /// The supersede chain through a note, oldest first.
    History {
        /// Note path, e.g. memorycastle/journal/2026-10-18-topic.md
        note: String,
    },
    /// Write the project's interlinkage graph: nodes.tsv, edges.tsv, graph.html.
    Graph {
        /// Output directory; default <repo>/memorycastle/graph.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Also index the checkout (files, definitions, imports, mentions) and include it; incremental.
        #[arg(long)]
        code: bool,
        /// Put every definition and mention edge in graph.html (the TSVs always have them).
        #[arg(long)]
        full: bool,
    },
    /// Wire every coding agent on this machine to memorycastle: MCP entries, the Claude Code plugin
    /// and status line, the memory section in CLAUDE.md / AGENTS.md / GEMINI.md, the code index,
    /// past transcripts into the ledger, and the embedding server. Idempotent.
    Install {
        /// Agents, comma separated: claude, codex, gemini, antigravity (agy), cursor, copilot, opencode. Default: every agent detected.
        agents: Option<String>,
        /// Every agent detected on this machine (the default when no list is given).
        #[arg(long)]
        all: bool,
        /// Only this repo's files; skip machine-level config.
        #[arg(long)]
        repo_only: bool,
        /// Print what would change.
        #[arg(long)]
        dry_run: bool,
    },
    /// Reverse `install`: memory sections, MCP entries, the Claude plugin and status line. `--purge` also deletes the store and models.
    Uninstall {
        /// Agents, comma separated. Default: every agent detected.
        agents: Option<String>,
        /// Only this repo's instruction files.
        #[arg(long)]
        repo_only: bool,
        /// Also delete the data dir (store, models, runtime). Notes in repos stay.
        #[arg(long)]
        purge: bool,
        /// Print what would change.
        #[arg(long)]
        dry_run: bool,
    },
    /// Print the snippet for CLAUDE.md, AGENTS.md or GEMINI.md.
    Init,
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Register the current checkout.
    Add,
    List,
    /// Put a project in a group; `--related` searches the group.
    Group { name: String, group: Option<String> },
    /// Re-attach memory after a remote URL or path change.
    Relink,
}

fn main() {
    // A closed pipe (`| head`) ends the program quietly instead of panicking in println.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("memorycastle: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    if let Cmd::Init = cli.cmd {
        print!("{AGENT_SNIPPET}");
        return Ok(());
    }
    if let Cmd::Mcp = cli.cmd {
        return mcp::run(cli.quiet);
    }
    let cwd = std::env::current_dir()?;
    let mut ctx = Context::open(&cwd, cli.project.as_deref(), cli.quiet)?;
    match cli.cmd {
        Cmd::Save { files, changed, force, keyword } => {
            let mut files = files;
            if changed {
                let root = ctx.repo.as_ref().map(|r| r.root.clone()).ok_or("--changed needs a git checkout")?;
                for dir in ["memorycastle/journal", "journal"] {
                    let d = root.join(dir);
                    let Ok(rd) = std::fs::read_dir(&d) else { continue };
                    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")).collect();
                    v.sort();
                    files.extend(v);
                }
            }
            if files.is_empty() {
                return Err("nothing to save".into());
            }
            let (added, updated, unchanged, embedded) = ctx.save_files(&files, force, keyword, "user")?;
            if cli.json {
                println!("{}", serde_json::json!({"added": added, "updated": updated, "unchanged": unchanged, "embedded_chunks": embedded}));
            } else {
                println!("added {added}, updated {updated}, unchanged {unchanged}, embedded {embedded} chunks");
            }
        }
        Cmd::Search { query, k, scope, meaning, keyword, explain, sections, brief, abs } => {
            let mode = if meaning { Mode::Meaning } else if keyword { Mode::Keyword } else { Mode::Auto };
            let (hits, ex) = ctx.search(&query, &scope.flags(), &SearchOptions { k, mode })?;
            let read_spans = match (ctx.project.clone(), ctx.repo.as_ref().map(|r| r.root.clone())) {
                (Some(p), Some(r)) => {
                    memorycastle::code::refresh(&mut ctx.conn, p.id, &r)?;
                    memorycastle::spans::ranked(&ctx.conn, p.id, Some(&r), &query, &hits)?
                }
                _ => Vec::new(),
            };
            if cli.json {
                println!("{}", serde_json::json!({"hits": hits, "read": read_spans, "explain": if explain { Some(&ex) } else { None }}));
                return Ok(());
            }
            if explain {
                println!(
                    "fts: {}  bm25 hits: {} ({:.1} ms)  model: {} ({}){}  embed {:.0} ms  vec hits: {} (scan {:.1} ms)  dropped {} under {:.2}  distinctive: {}",
                    ex.fts_query.as_deref().unwrap_or("-"),
                    ex.bm25_hits,
                    ex.bm25_ms,
                    if ex.model_used { "yes" } else { "no" },
                    ex.model_reason,
                    if ex.query_cached { ", query cached" } else { "" },
                    ex.embed_ms,
                    ex.vec_hits,
                    ex.scan_ms,
                    ex.dropped,
                    memorycastle::limits::MIN_CONFIDENCE,
                    if ex.distinctive.is_empty() { "none".to_string() } else { ex.distinctive.join(" ") }
                );
            }
            if hits.is_empty() {
                println!("{}", memorycastle::render::empty_line(&ex));
            }
            let repo_root = ctx.repo.as_ref().map(|r| r.root.clone());
            let r = memorycastle::render::search(&ctx.conn, &hits, &read_spans, &memorycastle::render::Options { query: &query, sections, brief, explain, abs, repo_root: repo_root.as_deref() })?;
            print!("{}", r.text);
            // The ledger counts agent lookups only: `--explain` is a person
            // diagnosing ranking, and `eval` never records.
            if let (Some(p), false) = (&ctx.project, explain) {
                memorycastle::render::record(&ctx.conn, p.id, &r)?;
            }
        }
        Cmd::Forget { target, purge, project_all, verify } => {
            let pid = ctx.require_project()?.id;
            if project_all {
                let name = ctx.require_project()?.name.clone();
                store::forget_project(&mut ctx.conn, pid)?;
                println!("forgot project {name}");
            } else if let Some(t) = target {
                if store::forget(&mut ctx.conn, pid, &t, purge)? {
                    println!("forgot {t}{}", if purge { " (purged)" } else { "" });
                } else {
                    return Err(format!("no note '{t}' in this project").into());
                }
            } else if !verify {
                return Err("give a note path or id, or --project-all".into());
            }
            if verify {
                let left = store::verify_clean(&ctx.conn)?;
                if left == 0 {
                    println!("verified: nothing derived remains");
                } else {
                    return Err(format!("{left} derived rows remain; run reindex").into());
                }
            }
        }
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::Add => {
                let p = ctx.require_project()?;
                println!("{} ({:x}) paths: {}", p.name, p.id, p.paths.join(", "));
            }
            ProjectCmd::List => {
                let list = project::list(&ctx.conn)?;
                if cli.json {
                    println!("{}", serde_json::to_string(&list.iter().map(|p| serde_json::json!({"name": p.name, "id": p.id, "group": p.group, "paths": p.paths})).collect::<Vec<_>>())?);
                } else {
                    for p in list {
                        println!("{:<24} {:<12} {}", p.name, p.group.as_deref().unwrap_or("-"), p.paths.join(", "));
                    }
                }
            }
            ProjectCmd::Group { name, group } => {
                if !project::set_group(&ctx.conn, &name, group.as_deref())? {
                    return Err(format!("unknown project '{name}'").into());
                }
                println!("{name}: group {}", group.as_deref().unwrap_or("cleared"));
            }
            ProjectCmd::Relink => {
                let repo = ctx.repo.clone().ok_or("not inside a git repository")?;
                match project::relink(&mut ctx.conn, &repo)? {
                    Some((old, new)) => println!("relinked {old:x} -> {new:x}"),
                    None => println!("nothing to relink"),
                }
            }
        },
        Cmd::Reindex { space, keyword } => {
            let (chunks, embedded) = ctx.reindex(space.is_some(), keyword)?;
            println!("rebuilt {chunks} chunks, embedded {embedded}");
        }
        Cmd::Lint { files } => {
            let root = ctx.repo.as_ref().map(|r| r.root.clone()).unwrap_or_else(|| std::path::PathBuf::from("."));
            let list: Vec<std::path::PathBuf> = if files.is_empty() {
                let mut v: Vec<_> = std::fs::read_dir(root.join("memorycastle/journal")).map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")).collect()).unwrap_or_default();
                v.sort();
                v
            } else {
                files.iter().map(std::path::PathBuf::from).collect()
            };
            let mut problems = 0;
            for p in &list {
                let content = std::fs::read_to_string(p)?;
                let rel = p.strip_prefix(&root).unwrap_or(p).display();
                for w in store::lint(&content) {
                    println!("{rel}: {w}");
                    problems += 1;
                }
            }
            if cli.json {
                println!("{}", serde_json::json!({"notes": list.len(), "problems": problems}));
            } else {
                println!("{} notes, {} problems", list.len(), problems);
            }
        }
        Cmd::Install { agents, all: _, repo_only, dry_run } => {
            use memorycastle::install::{self, Agent};
            let chosen: Vec<Agent> = match agents.as_deref() {
                Some(list) => {
                    let mut v = Vec::new();
                    for a in list.split(',') {
                        v.push(Agent::parse(a).ok_or_else(|| format!("unknown agent '{a}' (claude, codex, gemini, antigravity, cursor, copilot, opencode)"))?);
                    }
                    v
                }
                None => Agent::ALL.into_iter().filter(|a| a.detected()).collect(),
            };
            if chosen.is_empty() {
                return Err("no coding agent detected; name one: memcastle install claude,codex,gemini,antigravity,cursor,copilot,opencode".into());
            }
            let o = install::Options { dry_run, repo_only };
            let verb = if dry_run { "would" } else { "did" };
            println!("agents: {}", chosen.iter().map(|a| a.name()).collect::<Vec<_>>().join(", "));
            if !repo_only {
                for a in &chosen {
                    for st in install::install_global(*a, &o)? {
                        println!("  [{}] {}", if st.done { verb } else { "ok" }, st.what);
                    }
                }
            }
            let Some(repo) = ctx.repo.as_ref().map(|r| r.root.clone()) else {
                println!("done: every agent now follows the memory rules in every repo (user-level instruction files). Inside a repo, `memcastle install` also adds the section to its CLAUDE.md/AGENTS.md for teammates and indexes its notes.");
                return Ok(());
            };
            let p = ctx.require_project()?.clone();
            println!("repo: {}", repo.display());
            let mut files_done: Vec<&str> = Vec::new();
            for a in &chosen {
                if files_done.contains(&a.instruction_file()) {
                    continue;
                }
                files_done.push(a.instruction_file());
                let st = install::install_repo(&repo, *a, &o)?;
                println!("  [{}] {}", if st.done { verb } else { "ok" }, st.what);
            }
            let journal = repo.join("memorycastle/journal");
            if !journal.is_dir() {
                if !dry_run {
                    std::fs::create_dir_all(&journal)?;
                }
                println!("  [{verb}] memorycastle/journal/ created");
            }
            if !dry_run {
                memorycastle::code::refresh(&mut ctx.conn, p.id, &repo)?;
                let (files, defs): (i64, i64) = ctx.conn.query_row(
                    "SELECT (SELECT count(*) FROM code_files WHERE project_id = ?1), (SELECT count(*) FROM code_defs WHERE project_id = ?1)",
                    [p.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                println!("  [did] code index: {files} files, {defs} definitions");
                let mut notes: Vec<PathBuf> = std::fs::read_dir(&journal).map(|d| d.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")).collect()).unwrap_or_default();
                notes.sort();
                if !notes.is_empty() {
                    let (added, updated, unchanged, embedded) = ctx.save_files(&notes, false, false, "user")?;
                    println!("  [did] notes: {added} added, {updated} updated, {unchanged} unchanged, {embedded} chunks embedded");
                }
                let mut imported = (0usize, 0usize, 0usize);
                for (path, kind) in memorycastle::importlog::transcripts_for(&repo) {
                    let (r, l) = match kind {
                        "codex" => memorycastle::importlog::import_codex(&ctx.conn, p.id, &repo, &path)?,
                        _ => memorycastle::importlog::import_claude(&ctx.conn, p.id, &repo, &path)?,
                    };
                    if r + l > 0 {
                        imported = (imported.0 + 1, imported.1 + r, imported.2 + l);
                    }
                }
                if imported.0 > 0 {
                    println!("  [did] ledger: {} past transcripts imported ({} reads, {} lookups)", imported.0, imported.1, imported.2);
                } else {
                    println!("  [ok] ledger: no new transcripts for this repo");
                }
                if cfg!(feature = "ort") {
                    match memorycastle::serve::spawn_detached() {
                        Ok(true) => println!("  [did] embedding server started (idle 30m)"),
                        Ok(false) => println!("  [ok] embedding server running"),
                        Err(e) => println!("  [--] embedding server not started: {e}"),
                    }
                }
            } else {
                println!("  [would] build the code index, save notes, import past transcripts, start the embedding server");
            }
            println!("done. Restart the agents so they load the new config.");
        }
        Cmd::Quality { flagged } => {
            let pid = ctx.require_project()?.id;
            let notes = memorycastle::quality::notes(&ctx.conn, pid)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&notes)?);
            } else {
                print!("{}", memorycastle::quality::render(&notes, flagged));
                let flagged_n = notes.iter().filter(|n| !n.flags.is_empty()).count();
                if notes.len() < memorycastle::limits::QUALITY_MIN_NOTES {
                    println!("{} notes, {flagged_n} flagged; percentile verdicts start at {} notes", notes.len(), memorycastle::limits::QUALITY_MIN_NOTES);
                } else {
                    println!("{} notes, {flagged_n} flagged", notes.len());
                }
            }
        }
        Cmd::Uninstall { agents, repo_only, purge, dry_run } => {
            use memorycastle::install::{self, Agent};
            let chosen: Vec<Agent> = match agents.as_deref() {
                Some(list) => list.split(',').map(|a| Agent::parse(a).ok_or_else(|| format!("unknown agent '{a}'"))).collect::<std::result::Result<_, _>>()?,
                None => Agent::ALL.into_iter().filter(|a| a.detected()).collect(),
            };
            let o = install::Options { dry_run, repo_only };
            let verb = if dry_run { "would" } else { "did" };
            println!("agents: {}", chosen.iter().map(|a| a.name()).collect::<Vec<_>>().join(", "));
            if !repo_only {
                for a in &chosen {
                    for st in install::uninstall_global(*a, &o)? {
                        println!("  [{}] {}", if st.done { verb } else { "ok" }, st.what);
                    }
                }
            }
            if let Some(repo) = ctx.repo.as_ref().map(|r| r.root.clone()) {
                println!("repo: {}", repo.display());
                let mut files_done: Vec<&str> = Vec::new();
                for a in &chosen {
                    if files_done.contains(&a.instruction_file()) {
                        continue;
                    }
                    files_done.push(a.instruction_file());
                    let st = install::uninstall_repo(&repo, *a, &o)?;
                    println!("  [{}] {}", if st.done { verb } else { "ok" }, st.what);
                }
                println!("  [ok] memorycastle/journal/ kept: the notes are yours; delete the directory if you want them gone");
            }
            if purge {
                let dir = memorycastle::sys::data_dir();
                let socket = memorycastle::serve::socket_path();
                if !dry_run {
                    drop(ctx);
                    let _ = std::fs::remove_file(&socket);
                    if dir.is_dir() {
                        std::fs::remove_dir_all(&dir)?;
                    }
                }
                println!("  [{verb}] {} deleted (store, models, runtime)", dir.display());
            } else {
                println!("  [ok] {} kept; add --purge to delete the store and models", memorycastle::sys::data_dir().display());
            }
            if let Ok(exe) = std::env::current_exe() {
                let dir = exe.parent().map(|p| p.display().to_string()).unwrap_or_default();
                println!("binaries left for you to remove: rm {dir}/memorycastle {dir}/memcastle   (or: cargo uninstall memorycastle)");
            }
            println!("restart the agents so they drop the old config.");
        }
        Cmd::Guide => {
            print!("{}", include_str!("../docs/GUIDE.md"));
            return Ok(());
        }
        Cmd::Recall { text } => {
            let text = match text {
                Some(t) => t,
                None => {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                    s
                }
            };
            let words = memorycastle::search::content_words(&text);
            if words.len() < memorycastle::limits::RECALL_MIN_WORDS {
                return Ok(());
            }
            let Some(pid) = ctx.project.as_ref().map(|p| p.id) else { return Ok(()) };
            let (hits, _) = ctx.search(&text, &ScopeFlags::default(), &SearchOptions { k: memorycastle::limits::RECALL_MAX_NOTES * 2, mode: Mode::Auto })?;
            let sid = store::current_session(&ctx.conn, pid)?;
            let mut out = String::new();
            let mut shown: Vec<i64> = Vec::new();
            for h in hits.iter().filter(|h| h.trace.confidence >= memorycastle::limits::RECALL_MIN_CONFIDENCE && h.superseded_by.is_none()) {
                if shown.contains(&h.note_id) || shown.len() >= memorycastle::limits::RECALL_MAX_NOTES {
                    continue;
                }
                if let Some(sid) = &sid {
                    let key = format!("recall:{sid}:{}", h.note_id);
                    if memorycastle::db::meta_get(&ctx.conn, &key)?.is_some() {
                        continue;
                    }
                    memorycastle::db::meta_set(&ctx.conn, &key, "1")?;
                }
                shown.push(h.note_id);
                let changed = if h.code_changed.is_empty() { String::new() } else { format!("  code changed since: {}", h.code_changed.join(", ")) };
                out.push_str(&format!("{}:{}  {:.2}  {}{}\n", h.path, h.line, h.trace.confidence, h.heading, changed));
            }
            if !out.is_empty() {
                print!("{out}");
                store::record_served_notes(&ctx.conn, pid, &shown, "recall")?;
                store::record_usage(&ctx.conn, pid, "recall", out.len(), 0)?;
            }
        }
        Cmd::Owed { once, remind } => {
            let Some(p) = &ctx.project else { return Ok(()) };
            if let Some(o) = memorycastle::draft::owed(&ctx.conn, p.id)? {
                if remind {
                    use memorycastle::limits::{OWED_REMIND_COOLDOWN_SECS, OWED_REMIND_FILES, OWED_REMIND_SECS};
                    let now = memorycastle::sys::now();
                    let enough = o.files.len() >= OWED_REMIND_FILES || now - o.first_edit >= OWED_REMIND_SECS;
                    let key = format!("reminded:{}", p.id);
                    let last: i64 = memorycastle::db::meta_get(&ctx.conn, &key)?.and_then(|v| v.parse().ok()).unwrap_or(0);
                    if !enough || now - last < OWED_REMIND_COOLDOWN_SECS {
                        return Ok(());
                    }
                    memorycastle::db::meta_set(&ctx.conn, &key, &now.to_string())?;
                }
                let since = match &o.last_note {
                    Some((path, at)) => format!("since {} ({} min ago)", path, (memorycastle::sys::now() - at).max(0) / 60),
                    None => "and saved no note this session".into(),
                };
                let line = format!("note owed: {} edited {since}: {}", o.files.len(), o.files.join(", "));
                if let Some(event) = once {
                    // Block once per session and event: the agent gets the reason
                    // and a second attempt passes, so nothing can wedge.
                    let key = format!("blocked:{event}:{}", p.id);
                    let session = store::current_session(&ctx.conn, p.id)?.unwrap_or_default();
                    if memorycastle::db::meta_get(&ctx.conn, &key)?.as_deref() != Some(session.as_str()) {
                        memorycastle::db::meta_set(&ctx.conn, &key, &session)?;
                        eprintln!("{line}");
                        std::process::exit(2);
                    }
                }
                println!("{line}");
            }
        }
        Cmd::Draft => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            if let Some(path) = memorycastle::draft::write(&ctx.conn, p.id, &repo.root)? {
                println!("{}", path.strip_prefix(&repo.root).unwrap_or(&path).display());
            }
        }
        Cmd::Session { id, agent } => {
            if let Some(p) = &ctx.project {
                store::set_session(&ctx.conn, p.id, &id, agent.as_deref())?;
            }
        }
        Cmd::ImportLog { file, claude, codex, all } => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            if all {
                let mut n = (0usize, 0usize, 0usize);
                for (path, kind) in memorycastle::importlog::transcripts_for(&repo.root) {
                    let (r, l) = match kind {
                        "codex" => memorycastle::importlog::import_codex(&ctx.conn, p.id, &repo.root, &path)?,
                        _ => memorycastle::importlog::import_claude(&ctx.conn, p.id, &repo.root, &path)?,
                    };
                    if r + l > 0 {
                        n = (n.0 + 1, n.1 + r, n.2 + l);
                    }
                }
                if !cli.quiet {
                    eprintln!("memorycastle: {} transcripts imported ({} reads, {} lookups)", n.0, n.1, n.2);
                }
                return Ok(());
            }
            let file = file.ok_or("give a transcript file, or --all")?;
            if codex {
                let (r, l) = memorycastle::importlog::import_codex(&ctx.conn, p.id, &repo.root, std::path::Path::new(&file))?;
                if !cli.quiet {
                    eprintln!("memorycastle: {r} reads and {l} lookups imported from {file}");
                }
                return Ok(());
            }
            if claude {
                let (r, l) = memorycastle::importlog::import_claude(&ctx.conn, p.id, &repo.root, std::path::Path::new(&file))?;
                if !cli.quiet {
                    eprintln!("memorycastle: {r} reads and {l} lookups imported from {file}");
                }
                return Ok(());
            }
            let text = std::fs::read_to_string(&file)?;
            let n = memorycastle::importlog::import(&ctx.conn, p.id, &repo.root, &text)?;
            if !cli.quiet {
                eprintln!("memorycastle: {n} reads imported from {file}");
            }
        }
        Cmd::Guard { path } => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            let rel = match std::path::Path::new(&path).strip_prefix(&repo.root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) if std::path::Path::new(&path).is_absolute() => return Ok(()),
                Err(_) => path.clone(),
            };
            let spans = store::unread_spans(&ctx.conn, p.id, &rel)?;
            if spans.is_empty() {
                return Ok(());
            }
            let ranges: Vec<String> = spans.iter().map(|(a, b)| format!("{a}-{b}")).collect();
            eprintln!(
                "memorycastle: a lookup already pointed at {rel} lines {}; read those ranges first (sed -n 'a,bp' {rel}, or Read with offset and limit). Reading the whole file is allowed on the next call.",
                ranges.join(", ")
            );
            std::process::exit(2);
        }
        Cmd::Edited { path } => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            let rel = match std::path::Path::new(&path).strip_prefix(&repo.root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) if std::path::Path::new(&path).is_absolute() => return Ok(()),
                Err(_) => path.clone(),
            };
            if !rel.starts_with("memorycastle/") {
                store::record_edit(&ctx.conn, p.id, &repo.root, &rel)?;
            }
        }
        Cmd::Read { path, bytes, lines } => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            let rel = match std::path::Path::new(&path).strip_prefix(&repo.root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) if std::path::Path::new(&path).is_absolute() => return Ok(()),
                Err(_) => path.clone(),
            };
            if rel.starts_with("memorycastle/") {
                return Ok(());
            }
            let range = lines.as_deref().and_then(|l| {
                let (a, b) = l.split_once('-')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            });
            store::record_read_lines(&ctx.conn, p.id, &repo.root, &rel, bytes, range, None)?;
        }
        Cmd::Status => {
            let root = ctx.repo.as_ref().map(|r| r.root.clone());
            let st = store::status(&mut ctx.conn, ctx.project.as_ref(), root.as_deref(), &ctx.db_path)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                match &st.project {
                    Some(p) => println!("project: {p}{}", st.group.as_ref().map(|g| format!(" (group {g})")).unwrap_or_default()),
                    None => println!("project: none (not in a git checkout); store-wide counts"),
                }
                println!("notes: {}  chunks: {}  vectors: {}  projects: {}", st.notes, st.chunks, st.vectors, st.projects);
                println!("space: {}", st.space.as_deref().unwrap_or("none (keyword only)"));
                println!("store: {} ({:.1} MB)", ctx.db_path.display(), st.db_bytes as f64 / 1_048_576.0);
                if !st.notes_newer_on_disk.is_empty() {
                    println!("changed on disk since saved: {}", st.notes_newer_on_disk.join(", "));
                }
                if st.links > 0 {
                    println!("links: {}", st.links);
                }
                if !st.dangling_files.is_empty() {
                    let list: Vec<String> = st.dangling_files.iter().map(|(f, n)| format!("{f} ({n})")).collect();
                    println!("notes about files that no longer exist: {}", list.join(", "));
                }
                let observed_line = |label: &str, o: &store::Observed| {
                    println!(
                        "observed ({label}): {}-{} tokens saved ({:.0}-{:.0}%) over {} reads guided by a printed span ({:.1} KB read of {:.1} KB) and {} files edited unread after a lookup ({:.1} KB); floor is a 40-line read, net of {} tokens of lookup output; {} file reads seen, {} guided reads followed by another read of the same file",
                        fmt_tokens(o.saved_tokens_min),
                        fmt_tokens(o.saved_tokens),
                        o.saved_pct_min.round(),
                        o.saved_pct.round(),
                        o.guided_reads,
                        o.guided_bytes as f64 / 1024.0,
                        o.guided_whole as f64 / 1024.0,
                        o.avoided_reads,
                        o.avoided_whole as f64 / 1024.0,
                        fmt_tokens(o.lookup_tokens),
                        o.reads,
                        o.rereads
                    );
                };
                if st.usage.session.is_some() {
                    observed_line("this session", &st.usage.observed_session);
                }
                if st.usage.recent_sessions > 1 {
                    observed_line(&format!("last {} sessions", st.usage.recent_sessions), &st.usage.observed_recent);
                }
                if st.usage.observed.reads > 0 {
                    observed_line("all time", &st.usage.observed);
                }
                if st.usage.observed.reads > 0 {
                    println!("{}", memorycastle::limits::BENCHMARK_LINE);
                }
                if st.usage.served_notes > 0 {
                    println!(
                        "label log: {} notes served, {} opened after ({:.0}%)",
                        st.usage.served_notes,
                        st.usage.opened_notes,
                        100.0 * st.usage.opened_notes as f64 / st.usage.served_notes as f64
                    );
                }
                if st.usage.reads_by_rank.iter().any(|&n| n > 0) {
                    let parts: Vec<String> = st.usage.reads_by_rank.iter().enumerate().filter(|(_, &n)| n > 0).map(|(r, n)| if r == 0 { format!("links/code {n}") } else { format!("#{r} {n}") }).collect();
                    println!("guided reads by span rank: {}", parts.join(", "));
                }
                if st.usage.calls == 0 {
                    println!("ceiling: none yet (counted per search and links call)");
                } else {
                    println!(
                        "ceiling: {} tokens ({:.0}%) over {} search/links calls, if every note and file drawn from had been read whole (an upper bound, not a measurement); last 24 h: {:.0}% over {} calls",
                        fmt_tokens(st.usage.saved_tokens),
                        st.usage.saved_pct.round(),
                        st.usage.calls,
                        st.usage.saved_pct_today.round(),
                        st.usage.calls_today
                    );
                }
                if let Some(e) = &st.last_eval {
                    println!("last eval: {e}");
                }
                for f in &st.stale_instructions {
                    println!("warning: {f}");
                }
            }
        }
        Cmd::Serve { idle, detach } => {
            if detach {
                if serve::spawn_detached()? && !cli.quiet {
                    eprintln!("memorycastle: embedding server started");
                }
                return Ok(());
            }
            let idle = parse_duration(&idle)?;
            let e = embed::open_default(cli.quiet)?;
            eprintln!("memorycastle: serving embeddings on {} (idle {}s)", serve::socket_path().display(), idle.as_secs());
            serve::run(e, idle)?;
        }
        Cmd::Eval { gold, baseline, meaning, keyword, gate } => {
            let gold_path = gold.unwrap_or_else(|| memorycastle::sys::data_dir().join("gold.json"));
            let set = eval::load_gold(&gold_path)?;
            let mode = if meaning { Mode::Meaning } else if keyword { Mode::Keyword } else { Mode::Auto };
            let previous: Option<eval::Report> = db::meta_get(&ctx.conn, "last_eval")?.and_then(|v| serde_json::from_str(&v).ok());
            let report = match baseline.as_deref() {
                Some("grep") => eval::run(&ctx.conn, &set, &mut GrepMemory { conn: &ctx.conn }, mode, "grep")?,
                Some(other) => return Err(format!("unknown baseline '{other}' (try grep)").into()),
                None => {
                    let label = match mode { Mode::Keyword => "keyword", Mode::Meaning => "meaning", Mode::Auto => "auto" };
                    if mode != Mode::Keyword && cfg!(feature = "ort") {
                        ctx.embedder()?;
                    }
                    let (conn, warm) = ctx.parts();
                    let mut mem = StoreMemory { conn, embedder: warm };
                    eval::run(conn, &set, &mut mem, mode, label)?
                }
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
            if baseline.is_none() {
                db::meta_set(&ctx.conn, "last_eval", &serde_json::to_string(&report)?)?;
            }
            if gate {
                let fails = eval::gate(&report, previous.as_ref(), mode == Mode::Keyword);
                if !fails.is_empty() {
                    return Err(format!("gate failed: {}", fails.join("; ")).into());
                }
                println!("gates passed");
            }
        }
        Cmd::Links { target, at, context, once } => {
            let pid = ctx.require_project()?.id;
            // A line inside a file: the enclosing definition is the unit the
            // agent is editing, so notes about it come first and the file's
            // own notes are shown only when the symbol has none.
            let mut target = target;
            if let (Some(line), true) = (at, target.contains('/') || target.contains('.')) {
                if let Some(r) = ctx.repo.as_ref().map(|r| r.root.clone()) {
                    memorycastle::code::refresh(&mut ctx.conn, pid, &r)?;
                }
                if let Some(d) = memorycastle::code::def_enclosing(&ctx.conn, pid, &target, line)? {
                    graph::ensure_links(&mut ctx.conn, pid)?;
                    if !graph::notes_for(&ctx.conn, pid, graph::Kind::Symbol, &d.name)?.is_empty() {
                        if !cli.json {
                            println!("about: {} ({}:{}-{} holds line {line})", d.name, d.path, d.line, d.end);
                        }
                        target = d.name;
                    }
                }
            }
            if once {
                if let Some(sid) = store::current_session(&ctx.conn, pid)? {
                    let key = format!("links:{sid}:{target}");
                    if memorycastle::db::meta_get(&ctx.conn, &key)?.is_some() {
                        return Ok(());
                    }
                    memorycastle::db::meta_set(&ctx.conn, &key, "1")?;
                }
            }
            graph::ensure_links(&mut ctx.conn, pid)?;
            if let Some(r) = ctx.repo.as_ref().map(|r| r.root.clone()) {
                memorycastle::code::refresh(&mut ctx.conn, pid, &r)?;
            }
            let kind = if target.ends_with(".md") && target.contains('/') { graph::Kind::Note } else if target.contains('/') || target.contains('.') { graph::Kind::File } else { graph::Kind::Symbol };
            let mut notes = graph::notes_for(&ctx.conn, pid, kind, &target)?;
            if notes.is_empty() && kind == graph::Kind::File {
                notes = graph::notes_for(&ctx.conn, pid, graph::Kind::Symbol, &target)?;
            }
            let defs = if kind == graph::Kind::Symbol { memorycastle::code::defs_named(&ctx.conn, pid, &target)? } else { Vec::new() };
            if cli.json {
                println!("{}", serde_json::json!({"notes": notes, "definitions": defs}));
            } else if notes.is_empty() && defs.is_empty() {
                println!("no notes link to {target}");
            } else {
                let root = ctx.repo.as_ref().map(|r| r.root.clone());
                let mut out = String::new();
                let mut stood = 0usize;
                for d in &defs {
                    let span = match &root {
                        Some(r) => memorycastle::code::current_span(&ctx.conn, pid, r, d)?,
                        None => Some((d.line, d.end)),
                    };
                    match span {
                        Some((a, b)) if b > a => out.push_str(&format!("defined: {}:{}-{}  ({})\n", d.path, a, b, d.kind)),
                        Some((a, _)) => out.push_str(&format!("defined: {}:{}  ({})\n", d.path, a, d.kind)),
                        None => out.push_str(&format!("defined: {}  ({}; moved since the last `graph --code`)\n", d.path, d.kind)),
                    }
                    if let Some(r) = &root {
                        stood += std::fs::metadata(r.join(&d.path)).map(|m| m.len() as usize).unwrap_or(0).min(memorycastle::limits::USAGE_FILE_CAP);
                    }
                    if let Some((a, b)) = span {
                        store::record_span(&ctx.conn, pid, &d.path, a, b, 0)?;
                    }
                }
                for n in &notes {
                    let sup = n.superseded_by.as_ref().map(|p| format!("  (superseded by {p})")).unwrap_or_default();
                    let via = if !n.via.is_empty() && n.via != target { format!("  as {}", n.via) } else { String::new() };
                    out.push_str(&format!("{}  {}  {}  ({}){}{}\n", n.path, n.title, fmt_date(n.dated.unwrap_or(n.saved_at)), n.author, sup, via));
                    stood += store::note_bytes(&ctx.conn, n.id)?;
                }
                if context {
                    if let Some(r) = &root {
                        for line in memorycastle::spans::context(&ctx.conn, pid, r, &defs)? {
                            // `read: path:a-b ...` lines are served spans too.
                            if let Some(rest) = line.strip_prefix("read: ") {
                                let loc = rest.split("  ").next().unwrap_or("");
                                if let Some((path, range)) = loc.rsplit_once(':') {
                                    if let Some((a, b)) = range.split_once('-') {
                                        if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                                            store::record_span(&ctx.conn, pid, path, a, b, 0)?;
                                        }
                                    }
                                }
                            }
                            out.push_str(&format!("   {line}\n"));
                        }
                    }
                }
                print!("{out}");
                store::record_usage(&ctx.conn, pid, "links", out.len(), stood)?;
            }
        }
        Cmd::History { note } => {
            let pid = ctx.require_project()?.id;
            let chain = graph::history(&ctx.conn, pid, &note)?;
            if cli.json {
                println!("{}", serde_json::to_string(&chain)?);
            } else if chain.is_empty() {
                println!("no note at {note}");
            } else {
                for (i, n) in chain.iter().enumerate() {
                    let mark = if n.superseded_by.is_some() { "superseded" } else { "live" };
                    println!("{}{}  {}  {}  [{}]", if i > 0 { "  -> " } else { "" }, n.path, n.title, fmt_date(n.saved_at), mark);
                }
            }
        }
        Cmd::Graph { dir, code, full } => {
            let pid = ctx.require_project()?.id;
            let t = std::time::Instant::now();
            let built = graph::ensure_links(&mut ctx.conn, pid)?;
            let root = ctx.repo.as_ref().map(|r| r.root.clone());
            let code_stats = if code {
                let r = root.clone().ok_or("--code needs a git checkout")?;
                let st = memorycastle::code::index(&mut ctx.conn, pid, &r)?;
                if !cli.json {
                    println!("code index: {} files seen, {} indexed, {} unchanged, {} removed; {} definitions, {} imports, {} mentions in {:.0} ms", st.files_seen, st.files_indexed, st.files_unchanged, st.files_removed, st.defs, st.imports, st.mentions, st.ms);
                }
                Some(st)
            } else {
                None
            };
            let g = graph::build(&ctx.conn, pid, root.as_deref())?;
            let dir = match dir {
                Some(d) => d,
                None => ctx.repo.as_ref().map(|r| r.root.join("memorycastle/graph")).ok_or("not inside a git repository; pass --dir")?,
            };
            let bytes = graph::write(&g, &dir, full)?;
            let notes = g.nodes.iter().filter(|n| n.kind == "note").count();
            let files = g.nodes.iter().filter(|n| n.kind == "file").count();
            let symbols = g.nodes.iter().filter(|n| n.kind == "symbol").count();
            let missing_files = g.nodes.iter().filter(|n| n.kind == "file" && !n.exists).count();
            let unlocated = g.nodes.iter().filter(|n| n.kind == "symbol" && !n.exists).count();
            if cli.json {
                println!("{}", serde_json::json!({"dir": dir, "notes": notes, "files": files, "symbols": symbols, "missing_files": missing_files, "unlocated_symbols": unlocated, "edges": g.edges.len(), "communities": g.communities, "bytes": bytes, "ms": t.elapsed().as_secs_f64() * 1000.0, "links_built": built, "code": code_stats}));
            } else {
                println!("{}: {notes} notes, {files} files ({missing_files} not in the checkout), {symbols} symbols ({unlocated} not found in the files their notes name), {} edges, {} communities; {:.1} KB in {:.1} ms", dir.display(), g.edges.len(), g.communities, bytes as f64 / 1024.0, t.elapsed().as_secs_f64() * 1000.0);
            }
        }
        Cmd::Export { dir } => {
            let pid = ctx.require_project()?.id;
            let root = match dir {
                Some(d) => d,
                None => ctx.repo.as_ref().map(|r| r.root.clone()).ok_or("not inside a git repository; pass --dir")?,
            };
            let n = store::export(&ctx.conn, pid, &root)?;
            println!("wrote {n} notes into {}", root.display());
        }
        Cmd::Init | Cmd::Mcp => unreachable!(),
    }
    Ok(())
}

fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().map_err(|_| format!("bad duration '{s}'"))?;
    Ok(match unit {
        "" | "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        "h" => Duration::from_secs(n * 3600),
        _ => return Err(format!("bad duration unit in '{s}'").into()),
    })
}

/// YYYY-MM-DD from a unix timestamp, without a date crate.
fn fmt_date(ts: i64) -> String {
    memorycastle::render::fmt_date(ts)
}

/// 950 -> "950", 12_345 -> "12k", 1_234_567 -> "1.2M".
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{}k", n / 1000)
    } else {
        n.to_string()
    }
}
