use clap::{Args, Parser, Subcommand};
use memorycastle::graph;
use memorycastle::app::{Context, AGENT_SNIPPET};
use memorycastle::project::{self, ScopeFlags};
use memorycastle::search::{GrepMemory, Mode, SearchOptions, StoreMemory};
use memorycastle::{db, embed, eval, mcp, serve, store, Result};
use std::path::PathBuf;
use std::time::Duration;

const LINKS_HELP: &str = "\
links: section | use case | command
before edit | function holding a line, its notes | memcastle links <file> --at <line>
find code | where defined, callers, callees, notes | memcastle links <symbol>
change signature | one span per call site, and constants | memcastle links <symbol> --context
every caller | also under 0.3; every call site with --context | memcastle links <symbol> --all
task | only the notes that matter for a change (sends them to Jev) | memcastle links <symbol> --for '<task>'
judged gate | edits judged against your last prompt (sends notes to Jev) | memcastle project judge on
one of many | the definition in that file or directory | memcastle links crates/server/src/auth.rs::refresh
json | notes, definitions, scored callers, call sites | memcastle --json links <symbol> --context
new file | its notes, or what it defines and who uses it | memcastle links <file>
history | how a decision changed | memcastle history <note.md>
read as: path:a-b = read those lines, not the file
read as: caller path:line 0.9 = chance this call reaches this definition: 1.0 same file, 0.2 lowest
read as: caller under 0.7 = check the call site; it may reach another definition
read as: +N by name only, under 0.3 = calls too unlikely to list, counted only; --all lists them
read as: read: path:a-b caller calls name 0.7 = one call site, the lines around it, its confidence
read as: too wide: read path:a-b = function over 150 lines, read the block shown
read as: blocks: a-b header = inner blocks of a function over 150 lines, read the one you edit
read as: lines 9-11 Decision = note sections naming it; read those lines, not the note
read as: decision <sentence> = the note's first Decision sentence (first two notes)
read as: nodes.store( = a field's declared type (nodes: Nodes) points at Nodes::store
read as: as foo::bar = spelling the note used, or the neighbour it was ranked by
read as: superseded by X = decision reversed, act on X
read as: no notes link to X, nothing after = not indexed; check spelling or run graph --code
limits: edges are by name; a call through a trait or a macro is missed
combine: printed range | sed -n 'a,bp' path, or Read with offset and limit
combine: trait, macro or callback call | grep -rn \"name(\" or a language server
combine: no notes link to X, still after graph --code | grep -rn X, then read the code
combine: changing a signature | links <symbol> --context, then build; the compiler lists all
combine: config key, string, flag | grep -rn; the index holds definitions and calls only
";

const SEARCH_HELP: &str = "\
search: section | use case | command
decide | a choice or a fix in an area with history | memcastle search \"<question>\"
other words | a why or design question: one more wording, merged | memcastle search \"<question>\" --also \"<repo's words>\"
full sections | the matched bullets of Decision, Rejected, Measured | memcastle search \"<question>\" --sections
whole sections | every line of each section | memcastle search \"<question>\" --whole
past work | user says \"like we did X\", or resuming | memcastle search \"<X in its own words>\" --sections
sister repo | question about a repo in a declared group | memcastle search \"<question>\" --related
ranking | why a hit ranked there, stage timings | memcastle search \"<question>\" --explain
learned | questions searches learned, teach or drop one | memcastle learned; learned add <note.md> \"<q>\"
skip | routine edits, reading code, running tests | use links before an edit
read as: note.md:13-14 0.71 Rejected = lines that matched, confidence 0 to 1, section
read as: confidence = 1.00 exact identifier; 0.70 and up answer; 0.36 to 0.69 verify; under 0.36 dropped (0.30 without the model)
read as: verify = read the hit's lines and check them against the code
read as: lines 13-14 0.71 Rejected, 9-10 0.53 Decision = note parts best first; read those lines
read as: decision <sentence> = the note's first Decision sentence
read as: code changed since: <symbols> = that code changed after the note; run links <symbol> first
read as: superseded by X = decision reversed, act on X
read as: … lines 27-51 = bullets that did not match; sed -n '27,51p' note.md if needed
read as: learned 0.88 (--explain) = an earlier question this close was answered by this note
read as: read after a similar question = the code an earlier asker of this question read next
read as: read first a. path:a-b name score why = code spans to read first, sed -n 'a,bp' path
read as: no relevant note = store has nothing; work from the code and say so
cite as: memorycastle/journal/<note>.md:<line>
combine: printed range | sed -n 'a,bp' path, or Read with offset and limit
combine: no relevant note | grep -rn the names you know, then read the code
combine: hit at 0.36 to 0.69 | read its lines, then check the code it names
";

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
enum LearnedCmd {
    /// Teach a note a question: `learned add <note.md> "<question>"`.
    Add { note: String, question: String },
    /// Drop a learned question by its id (the number `learned` prints).
    Forget { id: i64 },
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
    #[command(after_long_help = SEARCH_HELP)]
    Search {
        /// The question in plain words; name the symbol or file when you know it.
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
        /// After each hit, print the note's Decision, Rejected and Measured sections: the bullets and paragraphs that matched, the rest as line ranges.
        #[arg(long)]
        sections: bool,
        /// With --sections: every line of each section (up to 30), not only the blocks that matched.
        #[arg(long)]
        whole: bool,
        /// The default: one line per hit (confidence, note:line, section) plus the note's Decision sentence, then the read table; about 50 tokens per hit.
        #[arg(long, hide = true, conflicts_with = "sections")]
        brief: bool,
        /// The 3-line block per hit (title, files, a text snippet) instead of the brief line.
        #[arg(long, conflicts_with = "sections")]
        full: bool,
        /// Print `read:` spans with absolute paths (default: repo-relative, absolute when run outside the repo root).
        #[arg(long)]
        abs: bool,
        /// The same question in other words (the repo's own terms, the likely mechanism); hits are merged per note. Repeat for a second wording.
        #[arg(long)]
        also: Vec<String>,
    },
    /// Questions the store learned from searches followed by an open of another note; `add` teaches one, `forget` drops one.
    Learned {
        #[command(subcommand)]
        cmd: Option<LearnedCmd>,
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
    Status {
        /// One short line for a status bar: `<repo> | N notes | ~S (P%) this session, ~A (Q%) all sessions tokens saved`.
        /// The Claude Code status line prints this; for Codex, Gemini CLI or Cursor put it in tmux:
        /// `set -g status-right '#(cd #{pane_current_path} && memcastle status --line)'`.
        #[arg(long)]
        line: bool,
        /// The agent session to report as "this session" (the status line passes Claude's `session_id`);
        /// default: the session last declared in this repo.
        #[arg(long)]
        session: Option<String>,
    },
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
        /// Print, on stderr, the query searched and every note held back and why.
        #[arg(long)]
        explain: bool,
    },
    /// Hook entry point for Codex, Gemini CLI and Cursor (written by `install`): reads the agent's JSON, answers in its shape.
    #[command(hide = true)]
    Hook {
        /// codex, gemini (also antigravity) or cursor.
        agent: String,
        /// session-start, prompt, before-edit, before-grep, after-edit, after-bash, pre-compact or stop.
        event: String,
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
        /// The range a ranged read covers (`a-b`): it passes when it holds
        /// a served span, since that is the read the span asked for.
        #[arg(long)]
        lines: Option<String>,
    },
    /// Before an edit (called by the edit and shell hooks): the first edit
    /// of a file this session exits 2 with what `links` knows about it, so
    /// the hook can stop the edit once; the first edit in a directory, or a
    /// new file, searches the area too. Prints context on stdout otherwise.
    #[command(hide = true)]
    Gate {
        /// The file about to be written (absolute or relative to the cwd).
        path: Option<String>,
        /// The line being edited, when the tool says.
        #[arg(long)]
        at: Option<u32>,
        /// Forget the files looked up this session (after compaction).
        #[arg(long)]
        reset: bool,
        /// Read the Claude Code PreToolUse JSON on stdin (file, line, new
        /// text) and answer in its shape: the plugin's edit hook is this flag
        /// alone, so what the gate does changes with the binary.
        #[arg(long)]
        hook: bool,
        /// The edit's new text on stdin (for `hook <agent>`): checked by the
        /// rejected-approach tripwire.
        #[arg(long)]
        new_stdin: bool,
        /// Replay the rejected-approach tripwire over every commit since the
        /// first note: one line per trip (`commit path name note:line`).
        #[arg(long)]
        replay: bool,
        /// Read a Claude Code PreToolUse Grep or Bash event on stdin: a
        /// search for one name the code index defines is answered once per
        /// session with `links <name>` (exit 2); anything else passes.
        #[arg(long)]
        grep_hook: bool,
    },
    /// Record a file edit reported by an agent hook (no output; keyword path).
    #[command(hide = true)]
    Edited {
        path: String,
        /// When the edit happened (Unix seconds), when not now: the file's mtime, from `changed`.
        #[arg(long)]
        mtime: Option<i64>,
    },
    /// After a shell command: the files the checkout changed since the last
    /// call this session, each looked up once (`links --once`) and recorded
    /// as an edit. The hook for edits made through the shell rather than the
    /// agent's edit tool.
    #[command(hide = true)]
    Changed {
        /// The shell command that ran: a formatter or build (`BULK_COMMANDS`)
        /// gets its edits recorded and no lookup printed.
        #[arg(long)]
        command: Option<String>,
    },
    /// Ingest what arrived on disk: save every journal note edited since
    /// its last save and refresh the code index, incrementally. One line
    /// when something changed, nothing otherwise. The session-start hook
    /// and the after-bash hook (after a pull, merge or checkout) call it,
    /// so a note written by hand or pulled from a teammate is searchable
    /// before the first prompt, with no step to remember.
    #[command(hide = true)]
    Sync,
    /// Embed every pending chunk, all projects (started detached by a save
    /// with more than `SAVE_EMBED_MAX` pending).
    #[command(hide = true)]
    Embed,
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
        /// Fail when recall@3 is under this (the retrieval gate CI runs).
        #[arg(long)]
        gate_recall: Option<f64>,
        /// Fail when more than this many absent questions are answered.
        #[arg(long)]
        gate_absent: Option<usize>,
        /// Run the gold set through another build too (a binary path or a git
        /// ref, built in a temporary worktree), both on a snapshot of the
        /// store, and print the questions whose rank moved.
        #[arg(long)]
        compare: Option<String>,
    },
    /// Write the stored notes of the current project back into the repo (missing or changed files only).
    Export {
        /// Directory to write into; default the repo root (notes keep their relative paths).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Where a symbol is defined, who calls it and what it calls, plus every note about it; for a file, its notes, or what it defines and who uses it when no note names it. Works with no notes: the code index answers.
    #[command(after_long_help = LINKS_HELP)]
    Links {
        /// A file path (`src/store.rs`, or just `store.rs`), a symbol (`embed_missing`), or a note path.
        target: String,
        /// With a file: the function holding this line (narrowed to the block holding it when the function is longer than 150 lines) and the notes about that function, not every note about the file.
        #[arg(long)]
        at: Option<u32>,
        /// Also print the constants the definition uses and the exact lines that call it, as `read: path:a-b` spans.
        #[arg(long)]
        context: bool,
        /// Print nothing when this target was already looked up in this session (for hooks).
        #[arg(long)]
        once: bool,
        /// Every caller and callee, callers under 0.3 listed too, and with --context every call site.
        #[arg(long)]
        all: bool,
        /// The task at hand: a judge (TypeSafe AI's Jev, key in TYPESAFE_API_KEY) scores each note for it and only the notes that matter print, best first. Sends the task and the notes' text to api.typesafe.ai; without a key or on any error the usual list prints.
        #[arg(long = "for", value_name = "TASK")]
        for_task: Option<String>,
        /// The task at hand, judged locally: the notes are ordered by a search of it and those under the search's floor are left out (the edit gate passes the session's last prompt when the judge is off).
        #[arg(long, hide = true, value_name = "TASK")]
        task: Option<String>,
    },
    /// The unresolved items of every live note's Open section, newest first; with a file or symbol, only the notes about it.
    Open {
        /// A file or symbol; omitted, every note in the project.
        target: Option<String>,
        /// Every item, not only the newest 30.
        #[arg(long)]
        all: bool,
        /// The open items of the last note with any that this session saved (the compaction hook prints it).
        #[arg(long, hide = true)]
        resume: bool,
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
    Init {
        /// Write it into this file instead (the section upserted, a one-line lead under the file's first heading); `>>` only appends.
        #[arg(long, value_name = "FILE")]
        into: Option<PathBuf>,
    },
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
    /// The edit gate for this project, for every agent: `context` (the default) adds what is
    /// known to the first edit of a file, `block` stops that edit once with it, `off` says nothing.
    /// `MEMORYCASTLE_GATE` overrides it. With no mode, prints the current one.
    Gate {
        #[arg(value_parser = ["block", "context", "off"])]
        mode: Option<String>,
    },
    /// The judge in the edit gate for this project: `on` sends the session's last prompt, the edit
    /// and the file's notes to TypeSafe AI's Jev (key in TYPESAFE_API_KEY) and shows only the notes
    /// that matter for it, plus any whose Rejected section the edit may bring back; `off` (the
    /// default) keeps everything local. `MEMORYCASTLE_JUDGE` overrides it. With no mode, prints it.
    Judge {
        #[arg(value_parser = ["on", "off"])]
        mode: Option<String>,
    },
}

/// The first `CONTEXT_ITEMS` entries, then "+N more".
fn short_list(v: &[String]) -> String {
    let k = memorycastle::limits::CONTEXT_ITEMS;
    let mut s = v.iter().take(k).cloned().collect::<Vec<_>>().join(", ");
    if v.len() > k {
        s.push_str(&format!(" +{} more", v.len() - k));
    }
    s
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
    if let Cmd::Init { into } = &cli.cmd {
        match into {
            None => print!("{AGENT_SNIPPET}"),
            Some(path) => {
                let step = memorycastle::install::install_snippet(path, &path.display().to_string(), &memorycastle::install::Options::default())?;
                println!("{}", step.what);
            }
        }
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
        Cmd::Search { query, k, scope, meaning, keyword, explain, sections, whole, brief, full, abs, also } => {
            let mode = if meaning { Mode::Meaning } else if keyword { Mode::Keyword } else { Mode::Auto };
            // Brief is the default; sections, the full block and explain (which needs the trace field) opt out.
            let brief = brief || !(sections || whole || full || explain);
            let (hits, ex) = ctx.search_also(&query, &also, &scope.flags(), &SearchOptions { k, mode, working: Vec::new() })?;
            let span_query = std::iter::once(query.as_str()).chain(also.iter().map(String::as_str)).collect::<Vec<_>>().join(" ");
            let read_spans = match (ctx.project.clone(), ctx.repo.as_ref().map(|r| r.root.clone())) {
                (Some(p), Some(r)) => {
                    memorycastle::code::refresh(&mut ctx.conn, p.id, &r)?;
                    memorycastle::spans::ranked(&ctx.conn, p.id, Some(&r), &span_query, &hits, &ex.learned_spans)?
                }
                _ => Vec::new(),
            };
            if cli.json {
                println!("{}", serde_json::json!({"hits": hits, "read": read_spans, "explain": if explain { Some(&ex) } else { None }}));
                if let (Some(p), false) = (&ctx.project, explain) {
                    let mut notes: Vec<i64> = hits.iter().map(|h| h.note_id).collect();
                    notes.dedup();
                    memorycastle::learned::record_search(&ctx.conn, p.id, &query, &also, &notes, &ex)?;
                }
                return Ok(());
            }
            if explain {
                println!(
                    "fts: {}  bm25 hits: {} ({:.1} ms)  model: {} ({}){}  embed {:.0} ms  vec hits: {} (scan {:.1} ms)  learned {} (vec {:.1} ms, words {:.1} ms)  dropped {} under {:.2}  distinctive: {}",
                    ex.fts_query.as_deref().unwrap_or("-"),
                    ex.bm25_hits,
                    ex.bm25_ms,
                    if ex.model_used { "yes" } else { "no" },
                    ex.model_reason,
                    if ex.query_cached { ", query cached" } else { "" },
                    ex.embed_ms,
                    ex.vec_hits,
                    ex.scan_ms,
                    ex.learned,
                    ex.learned_vec_ms,
                    ex.learned_word_ms,
                    ex.dropped,
                    ex.floor,
                    if ex.distinctive.is_empty() { "none".to_string() } else { ex.distinctive.join(" ") }
                );
            }
            if hits.is_empty() {
                println!("{}", memorycastle::render::empty_line(&ex));
                if let Some(p) = &ctx.project {
                    print!("{}", memorycastle::render::code_fallback(&ctx.conn, p.id, &query)?);
                }
            }
            let repo_root = ctx.repo.as_ref().map(|r| r.root.clone());
            let r = memorycastle::render::search(&ctx.conn, &hits, &read_spans, &memorycastle::render::Options { query: &query, sections: sections || whole, whole, brief, explain, abs, repo_root: repo_root.as_deref() })?;
            print!("{}", r.text);
            // The ledger counts agent lookups only: `--explain` is a person
            // diagnosing ranking, and `eval` never records.
            if let (Some(p), false) = (&ctx.project, explain) {
                memorycastle::render::record(&ctx.conn, p.id, &r)?;
                memorycastle::learned::record_search(&ctx.conn, p.id, &query, &also, &r.notes, &ex)?;
            }
        }
        Cmd::Learned { cmd } => {
            let pid = ctx.require_project()?.id;
            match cmd {
                None => {
                    memorycastle::learned::derive(&ctx.conn, pid)?;
                    let rows = memorycastle::learned::list(&ctx.conn, pid)?;
                    if rows.is_empty() {
                        println!("no learned questions yet: a search followed by an open of a note it did not rank first teaches one");
                    }
                    let mut last = String::new();
                    for l in rows {
                        if l.path != last {
                            println!("{}", l.path);
                            last = l.path.clone();
                        }
                        println!("   {}  {:10}  {}  {}", l.id, l.kind, memorycastle::render::fmt_date(l.at), l.query);
                    }
                }
                Some(LearnedCmd::Add { note, question }) => {
                    let id: i64 = ctx
                        .conn
                        .query_row("SELECT id FROM notes WHERE project_id = ?1 AND (path = ?2 OR path LIKE '%/' || ?2) AND deleted_at IS NULL", rusqlite::params![pid, note], |r| r.get(0))
                        .map_err(|_| format!("no note '{note}' in this project"))?;
                    let (_, ex) = ctx.search(&question, &ScopeFlags::default(), &SearchOptions { k: 1, mode: Mode::Meaning, working: Vec::new() })?;
                    let Some(space) = ex.space_id else { return Err("learning a question needs the model".into()) };
                    memorycastle::learned::add_vector(&ctx.conn, pid, id, &question, &ex.qvec, space)?;
                    println!("learned: {question}");
                }
                Some(LearnedCmd::Forget { id }) => {
                    if !memorycastle::learned::forget(&ctx.conn, pid, id)? {
                        return Err(format!("no learned question {id} in this project").into());
                    }
                    println!("forgot learned question {id}");
                }
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
            ProjectCmd::Gate { mode } => {
                let id = ctx.require_project()?.id;
                if let Some(m) = &mode {
                    db::meta_set(&ctx.conn, &memorycastle::gate::mode_key(id), m)?;
                }
                println!("edit gate: {}", memorycastle::gate::mode(&ctx.conn, id)?);
            }
            ProjectCmd::Judge { mode } => {
                let id = ctx.require_project()?.id;
                if let Some(m) = &mode {
                    db::meta_set(&ctx.conn, &memorycastle::judge::mode_key(id), m)?;
                }
                let on = memorycastle::judge::mode(&ctx.conn, id)?;
                let key = if memorycastle::judge::key().is_some() { "" } else { " (TYPESAFE_API_KEY is not set: the gate stays local)" };
                println!("edit gate judge: {on}{key}");
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
        Cmd::Hook { agent, event } => {
            let Some(a) = memorycastle::hook::Agent::parse(&agent) else { return Err(format!("unknown agent {agent}: codex, gemini or cursor").into()) };
            let code = memorycastle::hook::run_hook(a, &event)?;
            std::process::exit(code);
        }
        Cmd::Guide => {
            print!("{}", include_str!("../docs/GUIDE.md"));
            return Ok(());
        }
        Cmd::Recall { text, explain } => {
            let text = match text {
                Some(t) => t,
                None => {
                    let mut s = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
                    s
                }
            };
            // The prompt's prose, not its paste (`search::recall_query`).
            let text = memorycastle::search::recall_query(&text);
            // The edit gate's task: the session's last prompt, kept locally;
            // judged by Jev when the judge is on, by a local search otherwise.
            if let Some(pid) = ctx.project.as_ref().map(|p| p.id) {
                if let Some(sid) = store::current_session(&ctx.conn, pid)? {
                    memorycastle::judge::remember_prompt(&ctx.conn, &sid, &text)?;
                }
            }
            if explain {
                eprintln!("query: {}", text.trim().replace('\n', " / "));
            }
            let words = memorycastle::search::content_words(&text);
            if words.len() < memorycastle::limits::RECALL_MIN_WORDS {
                return Ok(());
            }
            let Some(pid) = ctx.project.as_ref().map(|p| p.id) else { return Ok(()) };
            let (hits, _) = ctx.search(&text, &ScopeFlags::default(), &SearchOptions { k: memorycastle::limits::RECALL_MAX_NOTES * 2, mode: Mode::Auto, working: Vec::new() })?;
            let sid = store::current_session(&ctx.conn, pid)?;
            let now = memorycastle::sys::now();
            let mut out = String::new();
            let mut shown: Vec<i64> = Vec::new();
            // Judged without the identifier proof: a rare identifier in a
            // prompt (a pasted log, a file name) proves nothing about what
            // is asked; the model and the words must carry the hit.
            for h in hits.iter().filter(|h| h.trace.judged >= memorycastle::limits::RECALL_MIN_CONFIDENCE && h.superseded_by.is_none()) {
                if shown.contains(&h.note_id) || shown.len() >= memorycastle::limits::RECALL_MAX_NOTES {
                    continue;
                }
                let (bar, why) = memorycastle::quality::recall_bar(&ctx.conn, pid, h.note_id, now)?;
                if h.trace.judged < bar {
                    if explain {
                        eprintln!("held back: {} {:.2} < {bar:.2}: {}", h.path, h.trace.judged, why.unwrap_or_default());
                    }
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
                out.push_str(&format!("{}:{}  {:.2}  {}{}\n", h.path, h.line, h.trace.judged, h.heading, changed));
                // The decision in one sentence: a title gets skimmed, a decision gets read.
                if let Some(d) = store::decision_sentence(&ctx.conn, h.note_id)? {
                    out.push_str(&format!("    decision: {d}\n"));
                }
            }
            if !out.is_empty() {
                print!("{out}");
                store::record_served_notes(&ctx.conn, pid, &shown, "recall")?;
                store::record_usage(&ctx.conn, pid, "recall", out.len(), 0)?;
            }
        }
        Cmd::Owed { once, remind } => {
            let Some(p) = &ctx.project else { return Ok(()) };
            let owed = memorycastle::draft::owed(&ctx.conn, p.id)?;
            // Nothing edited since the last note: does that note's `Files:`
            // name what was edited for it? Held once per session on its own
            // key, so a session already held for the note is held for this too.
            // Only at stop (or asked plainly): compaction is no moment to
            // hold over a Files: line, and it has no draft to point at.
            let files_check = matches!(once.as_deref(), None | Some("stop"));
            if let (None, false, true, Some(repo)) = (&owed, remind, files_check, &ctx.repo) {
                if let Some((note, missing)) = memorycastle::draft::uncovered(&ctx.conn, p.id, &repo.root)? {
                    let line = format!("{note} does not name files edited for it: {}. Add them to its Files: line, then: memcastle save {note}", missing.join(", "));
                    if let Some(event) = once {
                        let key = format!("blocked:{event}-files:{}", p.id);
                        let session = store::current_session(&ctx.conn, p.id)?.unwrap_or_default();
                        if memorycastle::db::meta_get(&ctx.conn, &key)?.as_deref() != Some(session.as_str()) {
                            memorycastle::db::meta_set(&ctx.conn, &key, &session)?;
                            eprintln!("{line}");
                            std::process::exit(2);
                        }
                        return Ok(());
                    }
                    println!("{line}");
                }
                return Ok(());
            }
            if let Some(o) = owed {
                if remind {
                    use memorycastle::limits::{OWED_REMIND_COOLDOWN_SECS, OWED_REMIND_FILES, OWED_REMIND_SECS};
                    let now = memorycastle::sys::now();
                    let enough = o.files.len() >= OWED_REMIND_FILES || now - o.first_edit >= OWED_REMIND_SECS;
                    let key = format!("reminded:{}:{}", p.id, store::current_session(&ctx.conn, p.id)?.unwrap_or_default());
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
        Cmd::Guard { path, lines } => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            let rel = match std::path::Path::new(&path).strip_prefix(&repo.root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) if std::path::Path::new(&path).is_absolute() => return Ok(()),
                Err(_) => path.clone(),
            };
            let spans = store::unread_spans(&ctx.conn, p.id, &rel)?;
            let range = lines.as_deref().and_then(|l| l.split_once('-')).and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)));
            if spans.is_empty() || range.is_some_and(|(a, b)| spans.iter().any(|&(s, e)| a <= s && e <= b)) {
                return Ok(());
            }
            let ranges: Vec<String> = spans.iter().map(|(a, b)| format!("{a}-{b}")).collect();
            eprintln!(
                "memorycastle: a lookup already pointed at {rel} lines {}; read those ranges first (sed -n 'a,bp' {rel}, or Read with offset and limit). Reading the whole file is allowed on the next call.",
                ranges.join(", ")
            );
            std::process::exit(2);
        }
        Cmd::Embed => {
            // Detached, so nobody sees stderr: a failure is retried twice and
            // the last error kept in meta `embedding_error` (the first run on
            // the real store stopped 490 chunks in with the error discarded).
            memorycastle::db::meta_set(&ctx.conn, "embedding_at", &memorycastle::sys::now().to_string())?;
            let mut total = 0;
            let mut last = None;
            for _ in 0..3 {
                match ctx.embed_pending(None) {
                    Ok(n) => {
                        total += n;
                        last = None;
                        break;
                    }
                    Err(e) => last = Some(e.to_string()),
                }
            }
            memorycastle::db::meta_set(&ctx.conn, "embedding_error", last.as_deref().unwrap_or(""))?;
            memorycastle::db::meta_set(&ctx.conn, "embedding_at", "0")?;
            match last {
                Some(e) => return Err(memorycastle::err!("embed: {e}")),
                None => println!("embedded {total}"),
            }
        }
        Cmd::Sync => {
            let Some(root) = ctx.repo.as_ref().map(|r| r.root.clone()) else { return Ok(()) };
            let mut files: Vec<PathBuf> = Vec::new();
            for dir in ["memorycastle/journal", "journal"] {
                let Ok(rd) = std::fs::read_dir(root.join(dir)) else { continue };
                files.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "md")));
            }
            files.sort();
            let mut parts = Vec::new();
            if !files.is_empty() {
                let (added, updated, _, _) = ctx.save_files(&files, false, false, "sync")?;
                if added + updated > 0 {
                    parts.push(format!("ingested {} note{}{}", added + updated, if added + updated == 1 { "" } else { "s" }, if added > 0 && updated > 0 { format!(" ({added} new, {updated} updated)") } else if updated > 0 { " (updated)".into() } else { " (new)".into() }));
                }
            }
            if let Some(pid) = ctx.project.as_ref().map(|p| p.id) {
                let n = memorycastle::learned::derive(&ctx.conn, pid)?;
                memorycastle::learned::prune(&ctx.conn)?;
                if n > 0 {
                    parts.push(format!("learned {n} question{} from searches", if n == 1 { "" } else { "s" }));
                }
                // A repo that keeps a journal gets a code index even when
                // nobody ran `graph --code`: `links` answers from it.
                if memorycastle::code::has_index(&ctx.conn, pid)? || root.join("memorycastle/journal").is_dir() {
                    let st = memorycastle::code::index(&mut ctx.conn, pid, &root)?;
                    if st.files_indexed + st.files_removed > 0 {
                        parts.push(format!("code index: {} file{} changed", st.files_indexed + st.files_removed, if st.files_indexed + st.files_removed == 1 { "" } else { "s" }));
                    }
                }
            }
            if !parts.is_empty() {
                println!("{}", parts.join("; "));
            }
        }
        Cmd::Changed { command } => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            let out = std::process::Command::new("git").args(["status", "--porcelain", "--untracked-files=all"]).current_dir(&repo.root).output();
            let Ok(out) = out else { return Ok(()) };
            let mut now: Vec<(String, i64)> = Vec::new();
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if line.len() < 4 {
                    continue;
                }
                let rel = line[3..].rsplit(" -> ").next().unwrap_or("").trim_matches('"').to_string();
                if rel.is_empty() || rel.starts_with("memorycastle/") || rel.ends_with('/') || memorycastle::limits::GENERATED_DIRS.iter().any(|d| rel.starts_with(d) || rel.contains(&format!("/{d}"))) {
                    continue;
                }
                // An instruction file `install` just wrote is not the agent's edit.
                if memorycastle::install::is_own_write(&repo.root.join(&rel)) {
                    continue;
                }
                let mtime = std::fs::metadata(repo.root.join(&rel)).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
                now.push((rel, mtime));
            }
            let sid = store::current_session(&ctx.conn, p.id)?.unwrap_or_else(|| "none".to_string());
            let key = format!("bashfiles:{sid}");
            let before: std::collections::HashMap<String, i64> = memorycastle::db::meta_get(&ctx.conn, &key)?
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.rsplit_once('\t').and_then(|(a, b)| b.parse().ok().map(|m| (a.to_string(), m))))
                .collect();
            // A file this session has not seen yet counts only when it was
            // written after the session began: a change already in the
            // checkout is the user's, and charging it to the agent made the
            // stop hook ask a read-only session for a note (7 of 12 benchmark
            // runs spent a turn answering it).
            let started = store::session_started(&ctx.conn, p.id)?.unwrap_or(0);
            let changed: Vec<&String> = now
                .iter()
                .filter(|(rel, m)| match before.get(rel) {
                    Some(b) => b != m,
                    None => *m >= started,
                })
                .map(|(rel, _)| rel)
                .collect();
            let state: String = now.iter().map(|(r, m)| format!("{r}\t{m}")).collect::<Vec<_>>().join("\n");
            memorycastle::db::meta_set(&ctx.conn, &key, &state)?;
            if before.is_empty() && started == 0 && changed.len() > memorycastle::limits::CHANGED_MAX_FILES {
                // First call with no session start to judge mtimes by, on a
                // dirty checkout: nothing to attribute to the command.
                return Ok(());
            }
            let exe = std::env::current_exe()?;
            let have = memorycastle::gate::seen(&ctx.conn, p.id)?;
            // A formatter, a build or a command that rewrote more files than
            // a hand edit would: its files are edits, but nobody decided on
            // them line by line, so no lookup is printed. The tripwire still
            // runs on the first few.
            let tool_run = command.as_deref().is_some_and(memorycastle::hook::is_bulk_command) || changed.len() > memorycastle::limits::CHANGED_MAX_FILES;
            // Every changed file is an edit; only the first few get the
            // tripwire and a lookup printed, to keep the output short.
            for rel in changed.iter().skip(memorycastle::limits::CHANGED_MAX_FILES) {
                let mtime = now.iter().find(|(r, _)| r == *rel).map(|(_, m)| m.to_string()).unwrap_or_default();
                let mut args = vec!["edited", rel.as_str()];
                if !mtime.is_empty() && mtime != "0" {
                    args.extend(["--mtime", mtime.as_str()]);
                }
                let _ = std::process::Command::new(&exe).args(&args).current_dir(&repo.root).stdin(std::process::Stdio::null()).output();
            }
            for rel in changed.iter().take(memorycastle::limits::CHANGED_MAX_FILES) {
                // The tripwire for shell edits: the lines the checkout added
                // against HEAD, the file at HEAD as the before.
                let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&repo.root).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
                // The file as this session last saw it (the edit hook keeps
                // it before every edit), else HEAD.
                let current = std::fs::read_to_string(repo.root.join(rel.as_str())).unwrap_or_default();
                let (before, added) = match memorycastle::tripwire::read_snapshot(&sid, rel) {
                    Some(snap) => {
                        let added = memorycastle::tripwire::added_lines(&snap, &current);
                        (snap, added)
                    }
                    None => {
                        let before = git(&["show", &format!("HEAD:{rel}")]);
                        let added = match before.is_empty() {
                            true => current.clone(),
                            false => git(&["diff", "-U0", "HEAD", "--", rel]).lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).map(|l| &l[1..]).collect::<Vec<_>>().join("\n"),
                        };
                        (before, added)
                    }
                };
                memorycastle::tripwire::write_snapshot(&sid, rel, &current);
                let trips: Vec<_> = memorycastle::tripwire::check(&ctx.conn, p.id, rel, &before, &added, None, None)?.into_iter().filter(|t| !have.contains(&format!("trip:{}:{}", t.note, t.name))).collect();
                if !trips.is_empty() {
                    memorycastle::gate::mark(&ctx.conn, p.id, &trips.iter().map(|t| format!("trip:{}:{}", t.note, t.name)).collect::<Vec<_>>())?;
                    println!("{}\nIf the note's reason no longer holds, say why and supersede the note; otherwise take the rejected approach back out.", memorycastle::tripwire::render(&trips));
                }
                let mtime = now.iter().find(|(r, _)| r == *rel).map(|(_, m)| m.to_string()).unwrap_or_default();
                let mut args = vec!["edited", rel.as_str()];
                if !mtime.is_empty() && mtime != "0" {
                    args.extend(["--mtime", mtime.as_str()]);
                }
                let _ = std::process::Command::new(&exe).args(&args).current_dir(&repo.root).stdin(std::process::Stdio::null()).output();
                if tool_run {
                    continue;
                }
                // The hook's lookup, not the agent's: logged as `changed-links`.
                let o = std::process::Command::new(&exe).args(["links", rel, "--once"]).env("MEMORYCASTLE_VIA", "changed").current_dir(&repo.root).stdin(std::process::Stdio::null()).output();
                if let Ok(o) = o {
                    let text = String::from_utf8_lossy(&o.stdout);
                    let text = text.trim_end();
                    if !text.is_empty() && !text.starts_with("no notes link to") {
                        println!("notes about {rel} (changed by that command; read the printed ranges, not the file):\n{text}");
                    }
                }
            }
        }
        Cmd::Edited { path, mtime } => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            let rel = match std::path::Path::new(&path).strip_prefix(&repo.root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) if std::path::Path::new(&path).is_absolute() => return Ok(()),
                Err(_) => path.clone(),
            };
            if !rel.starts_with("memorycastle/") {
                store::record_edit_at(&ctx.conn, p.id, &repo.root, &rel, mtime.unwrap_or_else(memorycastle::sys::now))?;
                // A file the code index has not seen (a Write that created
                // it): index it now, so `links` finds its definitions.
                if memorycastle::code::paths_ending_with(&ctx.conn, p.id, &rel)?.iter().all(|f| f != &rel) {
                    memorycastle::code::refresh(&mut ctx.conn, p.id, &repo.root)?;
                }
            }
        }
        Cmd::Gate { path, at, reset, hook, new_stdin, replay, grep_hook } => {
            let (Some(p), Some(repo)) = (&ctx.project, &ctx.repo) else { return Ok(()) };
            if grep_hook {
                let mut input = String::new();
                let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
                if let memorycastle::gate::Outcome::Block(t) = memorycastle::gate::check_grep(&ctx.conn, p.id, &repo.root, &input)? {
                    eprintln!("{t}");
                    std::process::exit(2);
                }
                return Ok(());
            }
            if reset {
                return memorycastle::gate::reset(&ctx.conn, p.id);
            }
            if replay {
                let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(&repo.root).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
                let first: i64 = ctx.conn.query_row("SELECT min(coalesce(dated, saved_at)) FROM notes WHERE project_id = ?1 AND deleted_at IS NULL", [p.id], |r| r.get::<_, Option<i64>>(0))?.unwrap_or(0);
                let (mut commits, mut files, mut trips) = (0, 0, 0);
                // A note counts from the commit that added it, not its date:
                // the commit that writes a note often holds the code it describes.
                let mut known = std::collections::HashMap::new();
                let mut at = 0i64;
                for l in git(&["log", "--reverse", "--diff-filter=A", "--format=@%ct", "--name-only", "--", "memorycastle/journal"]).lines() {
                    if let Some(t) = l.strip_prefix('@') {
                        at = t.parse().unwrap_or(0);
                    } else if !l.is_empty() {
                        known.entry(l.to_string()).or_insert(at);
                    }
                }
                for line in git(&["log", "--reverse", "--no-merges", "--format=%H %ct", &format!("--since={first}")]).lines() {
                    let Some((c, t)) = line.split_once(' ') else { continue };
                    let t: i64 = t.parse().unwrap_or(0);
                    commits += 1;
                    for path in git(&["diff-tree", "--no-commit-id", "-r", "--name-only", c]).lines() {
                        if path.starts_with("memorycastle/") || path.ends_with(".md") {
                            continue;
                        }
                        files += 1;
                        let before = git(&["show", &format!("{c}^:{path}")]);
                        let added: String = git(&["diff", "-U0", &format!("{c}^"), c, "--", path]).lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).map(|l| &l[1..]).collect::<Vec<_>>().join("\n");
                        for tr in memorycastle::tripwire::check(&ctx.conn, p.id, path, &before, &added, Some(t), Some(&known))? {
                            trips += 1;
                            println!("{} {path}  `{}`  {}:{}", &c[..7], tr.name, tr.note, tr.line);
                        }
                        // Numbers per hunk, at the line it replaces in the parent.
                        let diff = git(&["diff", "-U0", &format!("{c}^"), c, "--", path]);
                        let mut hunk: Option<(u32, String)> = None;
                        let mut hunks = Vec::new();
                        for l in diff.lines() {
                            if let Some(h) = l.strip_prefix("@@ -") {
                                hunks.extend(hunk.take());
                                let a = h.split([',', ' ']).next().and_then(|x| x.parse().ok()).unwrap_or(0u32);
                                hunk = Some((a.max(1), String::new()));
                            } else if let (Some((_, text)), Some(added)) = (hunk.as_mut(), l.strip_prefix('+').filter(|_| !l.starts_with("+++"))) {
                                text.push_str(added);
                                text.push('\n');
                            }
                        }
                        hunks.extend(hunk);
                        for (a, text) in hunks {
                            for tr in memorycastle::tripwire::check_numbers(&ctx.conn, p.id, path, a, &before, &text, Some(t))? {
                                trips += 1;
                                println!("{} {path}:{a}  `{}`  {}:{}", &c[..7], tr.name, tr.note, tr.line);
                            }
                        }
                    }
                }
                println!("replay: {commits} commits, {files} file changes, {trips} trips");
                return Ok(());
            }
            let mut input = String::new();
            if hook || new_stdin {
                let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
            }
            let (path, at, new_text, old_text) = if hook {
                let Some(e) = memorycastle::gate::claude_edit(&input) else { return Ok(()) };
                (Some(e.file), e.at, e.new_text, e.old_text)
            } else {
                (path, at, new_stdin.then_some(input), None)
            };
            let Some(path) = path else { return Ok(()) };
            let abs = std::env::current_dir()?.join(&path);
            let Ok(rel) = abs.strip_prefix(&repo.root) else { return Ok(()) };
            let rel = rel.to_string_lossy().into_owned();
            match memorycastle::gate::check(&ctx.conn, p.id, &repo.root, &rel, at, abs.exists(), new_text.as_deref(), old_text.as_deref())? {
                memorycastle::gate::Outcome::Pass => {}
                memorycastle::gate::Outcome::Context(t) if hook => {
                    println!("{}", serde_json::json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": t}}));
                }
                memorycastle::gate::Outcome::Context(t) => println!("{t}"),
                memorycastle::gate::Outcome::Block(t) => {
                    eprintln!("{t}");
                    std::process::exit(2);
                }
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
                // A note file is not a code read for the savings ledger, but
                // reading it is the label log's open: without this row a note
                // served and then read in full never counted as opened.
                let id: Option<i64> = ctx.conn.query_row("SELECT id FROM notes WHERE project_id = ?1 AND path = ?2 AND deleted_at IS NULL", rusqlite::params![p.id, rel], |r| r.get(0)).ok();
                if let Some(id) = id {
                    store::record_served_notes(&ctx.conn, p.id, &[id], "read")?;
                }
                // Any read of the note, a range or whole, settles its spans:
                // the guard never stops a read after the agent followed one.
                ctx.conn.execute("UPDATE served_spans SET guarded = 1 WHERE project_id = ?1 AND path = ?2 AND guarded = 0", rusqlite::params![p.id, rel])?;
                return Ok(());
            }
            let range = lines.as_deref().and_then(|l| {
                let (a, b) = l.split_once('-')?;
                Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
            });
            store::record_read_lines(&ctx.conn, p.id, &repo.root, &rel, bytes, range, None)?;
        }
        Cmd::Status { line, session } => {
            if let Some(s) = session.filter(|s| !s.is_empty()) {
                std::env::set_var("MEMORYCASTLE_SESSION", s);
            }
            let root = ctx.repo.as_ref().map(|r| r.root.clone());
            let st = store::status(&mut ctx.conn, ctx.project.as_ref(), root.as_deref(), &ctx.db_path)?;
            if line {
                let mut parts = Vec::new();
                if let Some(r) = &root {
                    parts.push(r.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                }
                parts.push(format!("{} notes", st.notes));
                let mut sav = Vec::new();
                if st.usage.session.is_some() {
                    let o = &st.usage.observed_session;
                    sav.push(format!("~{} ({:.0}%) this session", fmt_tokens(o.saved_tokens), o.saved_pct.round()));
                }
                if st.usage.observed.reads > 0 {
                    let o = &st.usage.observed;
                    sav.push(format!("~{} ({:.0}%) all sessions", fmt_tokens(o.saved_tokens), o.saved_pct.round()));
                }
                if !sav.is_empty() {
                    parts.push(format!("{} tokens saved", sav.join(", ")));
                }
                println!("{}", parts.join(" | "));
                return Ok(());
            }
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
                if let Some(pid) = ctx.project.as_ref().map(|p| p.id) {
                  if let Ok(wide) = memorycastle::code::wide_defs(&ctx.conn, pid, 5) {
                    if !wide.is_empty() {
                        let list: Vec<String> = wide.iter().map(|(p, n, w)| format!("{p} {n} ({w} lines)")).collect();
                        println!("definitions wider than {} lines, where a note points at nothing readable: {}", memorycastle::limits::SPAN_WIDE, list.join(", "));
                    }
                  }
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
                if st.usage.session.is_some() && (st.usage.calls_session > 0 || st.usage.edits_session > 0) {
                    println!(
                        "this session: {} search/links lookups, {} files edited{}",
                        st.usage.calls_session,
                        st.usage.edits_session,
                        match st.usage.unlooked_session.len() {
                            0 => String::new(),
                            n => format!(", {n} of them with no links of your own: {}", short_list(&st.usage.unlooked_session)),
                        }
                    );
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
            let Some(listener) = serve::claim()? else {
                if !cli.quiet {
                    eprintln!("memorycastle: an embedding server already answers on {}", serve::socket_path().display());
                }
                return Ok(());
            };
            let e = embed::open_default(cli.quiet)?;
            eprintln!("memorycastle: serving embeddings on {} (idle {}s)", serve::socket_path().display(), idle.as_secs());
            serve::run(listener, e, idle)?;
        }
        Cmd::Eval { gold, baseline, meaning, keyword, gate, gate_recall, gate_absent, compare } => {
            let gold_path = gold.unwrap_or_else(|| memorycastle::sys::data_dir().join("gold.json"));
            let set = eval::load_gold(&gold_path)?;
            let mode = if meaning { Mode::Meaning } else if keyword { Mode::Keyword } else { Mode::Auto };
            if let Some(reference) = compare {
                let flag: &[&str] = if meaning { &["--meaning"] } else if keyword { &["--keyword"] } else { &[] };
                print!("{}", eval::compare_run(&ctx.conn, &gold_path, &reference, flag)?);
                return Ok(());
            }
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
            let fails = eval::retrieval_gate(&report, gate_recall, gate_absent);
            if !fails.is_empty() {
                return Err(format!("retrieval gate failed: {}", fails.join("; ")).into());
            }
            if gate {
                let fails = eval::gate(&report, previous.as_ref(), mode == Mode::Keyword);
                if !fails.is_empty() {
                    return Err(format!("gate failed: {}", fails.join("; ")).into());
                }
                println!("gates passed");
            }
        }
        Cmd::Links { target, at, context, once, all, for_task, task } => {
            let pid = ctx.require_project()?.id;
            // A line inside a file: the enclosing definition is the unit the
            // agent is editing, so notes about it come first and the file's
            // own notes are shown only when the symbol has none.
            let mut target = target;
            // The definition the line sits in, in this file only; when it is
            // wider than `SPAN_WIDE` the innermost block holding the line is
            // the span to read, and the `at:` line says so.
            let mut at_def: Option<memorycastle::code::Def> = None;
            let mut at_line = String::new();
            if let (Some(line), true) = (at, target.contains('/') || target.contains('.')) {
                let root = ctx.repo.as_ref().map(|r| r.root.clone());
                if let Some(r) = &root {
                    memorycastle::code::refresh(&mut ctx.conn, pid, r)?;
                }
                if let Some(d) = memorycastle::code::def_enclosing(&ctx.conn, pid, &target, line)? {
                    let width = d.end.saturating_sub(d.line) + 1;
                    let narrowed = root.as_ref().and_then(|r| memorycastle::code::narrow_span(r, &d, line));
                    at_line = match &narrowed {
                        Some((h, a, b)) => {
                            store::record_span(&ctx.conn, pid, &d.path, *a, *b, 0)?;
                            format!("at: {} ({}) {}:{}-{} holds line {line}, {width} lines, too wide: read {}:{}-{}  {}", d.name, d.kind, d.path, d.line, d.end, d.path, a, b, h)
                        }
                        None if width > memorycastle::limits::SPAN_WIDE => {
                            let (a, b) = (line.saturating_sub(20).max(d.line), (line + 20).min(d.end));
                            store::record_span(&ctx.conn, pid, &d.path, a, b, 0)?;
                            format!("at: {} ({}) {}:{}-{} holds line {line}, {width} lines, too wide: read {}:{}-{}", d.name, d.kind, d.path, d.line, d.end, d.path, a, b)
                        }
                        None => {
                            store::record_span(&ctx.conn, pid, &d.path, d.line, d.end, 0)?;
                            format!("at: {} ({}) {}:{}-{} holds line {line}", d.name, d.kind, d.path, d.line, d.end)
                        }
                    };
                    graph::ensure_links(&mut ctx.conn, pid)?;
                    if !graph::notes_for_def(&ctx.conn, pid, &d)?.is_empty() {
                        target = d.name.clone();
                    }
                    at_def = Some(d);
                } else {
                    // Outside every definition: say so, and name the
                    // neighbours, so the answer is never notes alone.
                    let (before, after) = memorycastle::code::defs_beside(&ctx.conn, pid, &target, line)?;
                    let side = |d: &memorycastle::code::Def| format!("{} ({}) {}:{}-{}", d.name, d.kind, d.path, d.line, d.end);
                    let near: Vec<String> = [before.as_ref().map(|d| format!("before: {}", side(d))), after.as_ref().map(|d| format!("after: {}", side(d)))].into_iter().flatten().collect();
                    if !near.is_empty() {
                        at_line = format!("at: line {line} is outside every definition; {}", near.join(", "));
                    }
                }
            }
            // Every lookup counts as shown this session, so the hooks' `--once`
            // calls do not print again what the agent just asked for.
            if let Some(sid) = store::current_session(&ctx.conn, pid)? {
                let key = format!("links:{sid}:{target}");
                if once && memorycastle::db::meta_get(&ctx.conn, &key)?.is_some() {
                    return Ok(());
                }
                if !memorycastle::db::is_readonly(&ctx.conn) {
                    memorycastle::db::meta_set(&ctx.conn, &key, "1")?;
                }
            }
            graph::ensure_links(&mut ctx.conn, pid)?;
            if let Some(r) = ctx.repo.as_ref().map(|r| r.root.clone()) {
                memorycastle::code::refresh(&mut ctx.conn, pid, &r)?;
            }
            let kind = if target.ends_with(".md") && target.contains('/') { graph::Kind::Note } else if memorycastle::code::is_file_target(&ctx.conn, pid, &target)? { graph::Kind::File } else { graph::Kind::Symbol };
            // A qualified name (`render::search`) is one definition: its
            // notes, not every note naming some `search` (22 notes, 5.5 KB,
            // for a question about five call sites).
            let qualified = kind == graph::Kind::Symbol && (target.contains("::") || target.contains('.'));
            let qdefs = if qualified && at_def.is_none() { memorycastle::code::defs_named(&ctx.conn, pid, &target)? } else { Vec::new() };
            let mut notes = match &at_def {
                Some(d) if kind == graph::Kind::Symbol => graph::notes_for_def(&ctx.conn, pid, d)?,
                _ if !qdefs.is_empty() => {
                    let mut v: Vec<graph::NoteRef> = Vec::new();
                    for d in &qdefs {
                        for n in graph::notes_for_def(&ctx.conn, pid, d)? {
                            if !v.iter().any(|o| o.id == n.id) {
                                v.push(n);
                            }
                        }
                    }
                    v
                }
                _ => graph::notes_for(&ctx.conn, pid, kind, &target)?,
            };
            if notes.is_empty() && kind == graph::Kind::File {
                notes = graph::notes_for(&ctx.conn, pid, graph::Kind::Symbol, &target)?;
            }
            let mut notes = graph::with_successors(&ctx.conn, pid, notes)?;
            // The line is in a definition no note names: the file's notes
            // about its call neighbours come first, not merely the newest.
            // A note that names no neighbour is about some other part of
            // the file: its title stays, its Decision line would be noise.
            let mut decisions = memorycastle::limits::LINKS_DECISIONS;
            if let (Some(d), graph::Kind::File) = (&at_def, kind) {
                let names = memorycastle::code::neighbours(&ctx.conn, pid, d)?;
                decisions = decisions.min(graph::rank_by_neighbours(&ctx.conn, pid, &mut notes, &names)?);
            }
            let defs = match &at_def {
                Some(d) if kind == graph::Kind::Symbol => vec![d.clone()],
                _ if kind == graph::Kind::Symbol => memorycastle::code::defs_named(&ctx.conn, pid, &target)?,
                _ => Vec::new(),
            };
            // A lookup is what the edit gate asks for: the files it answered
            // about need no gate this session.
            let mut looked: Vec<String> = match (&at_def, kind) {
                (Some(d), _) => vec![d.path.clone()],
                (None, graph::Kind::File) => memorycastle::code::paths_ending_with(&ctx.conn, pid, &target)?,
                (None, graph::Kind::Symbol) => defs.iter().map(|d| d.path.clone()).collect(),
                _ => Vec::new(),
            };
            // A file the code index does not hold (`combined3.toml`, a
            // script) is still a file the agent looked up: without it the
            // next write to it was gated as never looked at.
            if looked.is_empty() && kind == graph::Kind::File {
                if let Some(rel) = ctx.repo.as_ref().and_then(|r| repo_relative(&r.root, &target)) {
                    looked.push(rel);
                }
            }
            memorycastle::gate::mark(&ctx.conn, pid, &looked)?;
            memorycastle::gate::mark_lookup(&ctx.conn, pid, &looked)?;
            // With no note, the code index still answers: what the file
            // defines and who uses it, or the definition the line sits in.
            let facts: Vec<String> = if notes.is_empty() && kind == graph::Kind::File {
                let mut f = memorycastle::code::file_facts(&ctx.conn, pid, &target, all)?;
                if !at_line.is_empty() {
                    f.insert(0, at_line.clone());
                }
                f
            } else {
                Vec::new()
            };
            // With notes, what the file defines still answers "where is X in
            // here" without a grep: the first fact line only.
            let defines_line = if !notes.is_empty() && kind == graph::Kind::File && at_def.is_none() {
                memorycastle::code::file_facts(&ctx.conn, pid, &target, all)?.into_iter().find(|f| f.starts_with("defines:"))
            } else {
                None
            };
            // `--for <task>`: the judge keeps the notes that matter for the
            // task, best first; the code facts are never judged. Superseded
            // notes are left out (their successors are in the list).
            // From the edit gate (`gate::check`), the edit too: the judge also
            // asks whether it brings back what each note rejected.
            let judge_edit = memorycastle::judge::edit_from_env();
            let mut judged: std::collections::HashMap<i64, f32> = std::collections::HashMap::new();
            let mut judged_rejected: std::collections::HashMap<i64, f32> = std::collections::HashMap::new();
            let mut judged_reverses: std::collections::HashSet<i64> = std::collections::HashSet::new();
            let mut judge_model: Option<(String, usize)> = None;
            if let Some(task) = for_task.as_deref() {
                let cand: Vec<graph::NoteRef> = notes.iter().filter(|n| n.superseded_by.is_none()).take(memorycastle::limits::JUDGE_NOTES).cloned().collect();
                if !cand.is_empty() {
                    let mut texts = Vec::with_capacity(cand.len());
                    for n in &cand {
                        texts.push((n.path.clone(), ctx.conn.query_row("SELECT content FROM notes WHERE id = ?1", [n.id], |r| r.get::<_, String>(0))?));
                    }
                    match memorycastle::judge::score(&target, task, &texts, judge_edit.as_ref()) {
                        Ok(v) => {
                            // Once per session: this is a network call with the notes' text.
                            let sid = store::current_session(&ctx.conn, pid)?.unwrap_or_default();
                            let key = format!("judge_notice:{sid}");
                            if memorycastle::db::meta_get(&ctx.conn, &key)?.is_none() {
                                eprintln!("memorycastle: judge: the task and {} notes' text sent to api.typesafe.ai ({})", cand.len(), v.model);
                                memorycastle::db::meta_set(&ctx.conn, &key, "1")?;
                            }
                            // A note the edit may contradict prints whatever its score.
                            let rej = |i: usize| v.rejected.get(i).copied().unwrap_or(0.0);
                            let cut = if judge_edit.is_some() { memorycastle::limits::JUDGE_GATE_CUT } else { memorycastle::limits::JUDGE_CUT };
                            let mut kept: Vec<(f32, usize)> = v
                                .scores
                                .iter()
                                .copied()
                                .enumerate()
                                .filter(|&(i, s)| s >= cut || rej(i) >= memorycastle::limits::JUDGE_REJECTED)
                                .map(|(i, s)| (s, i))
                                .collect();
                            kept.sort_by(|a, b| rej(b.1).ge(&memorycastle::limits::JUDGE_REJECTED).cmp(&rej(a.1).ge(&memorycastle::limits::JUDGE_REJECTED)).then(b.0.total_cmp(&a.0)).then(a.1.cmp(&b.1)));
                            judged = kept.iter().map(|&(s, i)| (cand[i].id, s)).collect();
                            judged_rejected = kept.iter().filter(|&&(_, i)| rej(i) >= memorycastle::limits::JUDGE_REJECTED).map(|&(_, i)| (cand[i].id, rej(i))).collect();
                            judged_reverses = kept.iter().filter(|&&(_, i)| v.reverses.get(i).copied().unwrap_or(0.0) >= memorycastle::limits::JUDGE_REVERSES).map(|&(_, i)| cand[i].id).collect();
                            notes = kept.iter().map(|&(_, i)| cand[i].clone()).collect();
                            decisions = notes.len();
                            judge_model = Some((v.model, cand.len()));
                        }
                        Err(e) => eprintln!("memorycastle: {e}; notes listed without the judge"),
                    }
                }
            }
            // `--task` (or `--for` when the judge did not answer): a local search
            // of the task orders the notes, and notes it ranks under its floor
            // are left out. On 36 labelled edits the first note listed was
            // relevant 12 to 20 of 30 times, precision 23% to 35%
            // (2026-10-27-local-stand-ins-for-jev.md).
            let mut task_left_out = None;
            if let (None, Some(t)) = (&judge_model, task.as_deref().or(for_task.as_deref())) {
                if !notes.is_empty() {
                    let (hits, _) = ctx.search(t, &ScopeFlags::default(), &SearchOptions { k: memorycastle::limits::TASK_RANK_K, mode: Mode::Auto, working: Vec::new() })?;
                    // A hit on a superseded note counts for the note that replaced it.
                    let mut conf: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
                    for h in &hits {
                        for p in std::iter::once(h.path.clone()).chain(h.superseded_by.clone()) {
                            let c = conf.entry(p).or_insert(0.0);
                            *c = c.max(h.trace.confidence);
                        }
                    }
                    let before = notes.len();
                    let mut ranked: Vec<(f32, graph::NoteRef)> = notes.into_iter().filter(|n| n.superseded_by.is_none()).filter_map(|n| conf.get(&n.path).map(|&c| (c, n))).collect();
                    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
                    notes = ranked.into_iter().map(|(_, n)| n).collect();
                    decisions = decisions.min(notes.len());
                    task_left_out = Some(before - notes.len());
                }
            }
            if cli.json {
                // Callers and callees per definition, every one with its score.
                let mut calls = Vec::with_capacity(defs.len());
                for d in &defs {
                    let callers: Vec<_> = memorycastle::code::callers_for(&ctx.conn, pid, d)?
                        .into_iter()
                        .map(|(c, f, l, p)| serde_json::json!({"caller": c, "path": f, "line": l, "confidence": (p * 10.0).round() / 10.0}))
                        .collect();
                    let callees: Vec<_> = memorycastle::code::callees_for(&ctx.conn, pid, d)?
                        .into_iter()
                        .map(|(c, p)| serde_json::json!({"callee": c, "confidence": (p * 10.0).round() / 10.0}))
                        .collect();
                    calls.push(serde_json::json!({"path": d.path, "line": d.line, "name": d.name, "callers": callers, "callees": callees}));
                }
                let sites = match (ctx.repo.as_ref().map(|r| r.root.clone()), context) {
                    (Some(r), true) => memorycastle::spans::context(&ctx.conn, pid, &r, &defs, true)?,
                    _ => Vec::new(),
                };
                println!("{}", serde_json::json!({"notes": notes, "definitions": defs, "calls": calls, "context": sites, "code": facts}));
            } else if notes.is_empty() && defs.is_empty() && judge_model.is_none() && task_left_out.is_none() {
                println!("no notes link to {target}");
                for f in &facts {
                    println!("   {f}");
                }
                if facts.is_empty() {
                    if let Some(l) = memorycastle::code::nearest_line(&ctx.conn, pid, &target)? {
                        println!("   {l}");
                    }
                }
                if !facts.is_empty() {
                    store::record_usage(&ctx.conn, pid, "links", facts.iter().map(|f| f.len() + 4).sum(), 0)?;
                }
            } else {
                let root = ctx.repo.as_ref().map(|r| r.root.clone());
                let mut out = String::new();
                let mut said = (false, false);
                let mut stood = 0usize;
                if !at_line.is_empty() {
                    out.push_str(&at_line);
                    out.push('\n');
                }
                if let Some(l) = &defines_line {
                    out.push_str(l);
                    out.push('\n');
                }
                for d in &defs {
                    let span = match &root {
                        Some(r) => memorycastle::code::current_span(&ctx.conn, pid, r, d)?,
                        None => Some((d.line, d.end)),
                    };
                    let label = memorycastle::code::kind_label(&ctx.conn, pid, d)?;
                    match span {
                        Some((a, b)) if b > a => {
                            out.push_str(&format!("defined: {}:{}-{}  ({label})\n", d.path, a, b));
                            if let Some(blocks) = root.as_ref().and_then(|r| memorycastle::code::wide_blocks(r, &d.path, a, b)) {
                                out.push_str(&blocks);
                                out.push('\n');
                            }
                        }
                        Some((a, _)) => out.push_str(&format!("defined: {}:{}  ({label})\n", d.path, a)),
                        None => out.push_str(&format!("defined: {}  ({label}; moved since the last `graph --code`)\n", d.path)),
                    }
                    if let Some(r) = &root {
                        stood += std::fs::metadata(r.join(&d.path)).map(|m| m.len() as usize).unwrap_or(0).min(memorycastle::limits::USAGE_FILE_CAP);
                    }
                    if let Some((a, b)) = span {
                        store::record_span(&ctx.conn, pid, &d.path, a, b, 0)?;
                    }
                    // Callers per definition, with `--context` too: its
                    // `read:` lines add the call sites, they do not replace
                    // the scored list.
                    let mut calls = memorycastle::code::call_lines(&ctx.conn, pid, d, true, all, root.as_deref())?;
                    if defs.len() > 1 {
                        for l in calls.iter_mut() {
                            let (c, u, k) = memorycastle::code::compact_call_line(l);
                            *l = c;
                            said.0 |= u;
                            said.1 |= k;
                        }
                    }
                    // A table or constant is read, not called: the functions
                    // reading it are its callers' next hop.
                    let used = match &root {
                        Some(r) if d.kind != "fn" && !calls.iter().any(|l| l.starts_with("called by")) => memorycastle::code::used_by_line(&ctx.conn, pid, r, d)?,
                        _ => None,
                    };
                    if let Some(l) = &used {
                        out.push_str(&format!("   {l}\n"));
                    } else if calls.is_empty() {
                        if let Some(l) = memorycastle::code::users_line(&ctx.conn, pid, &d.name, &d.path)? {
                            out.push_str(&format!("   {l}\n"));
                        }
                    }
                    for l in calls {
                        out.push_str(&format!("   {l}\n"));
                    }
                }
                if let Some(d) = defs.first() {
                    for l in memorycastle::code::calls_footer(&d.name, said.0, said.1 && !said.0) {
                        out.push_str(&format!("{l}\n"));
                    }
                }
                // A hub file names dozens of notes: the first few, then a count.
                // Every note only when `--at` narrowed to a definition a note
                // names; a line in one no note names, or outside any, lists
                // the first few like a plain file lookup: 21 notes otherwise.
                let cap = if kind == graph::Kind::File && judge_model.is_none() { memorycastle::limits::LINKS_NOTES } else { notes.len() };
                if let (Some((m, n)), true) = (&judge_model, notes.is_empty()) {
                    out.push_str(&format!("no note bears on this task (judge {m}, {n} notes judged)\n"));
                }
                if let Some(n) = task_left_out.filter(|&n| n > 0) {
                    if notes.is_empty() {
                        out.push_str(&format!("no note about {target} matches the task ({n} left out; links {target} lists them)\n"));
                    } else {
                        out.push_str(&format!("{n} notes about {target} that do not match the task left out (links {target} lists them)\n"));
                    }
                }
                // A hook's lookup (`--once`) names a note it already printed
                // this session in one line: the same trackers came back under
                // every file edited (one RushTex session printed `progress.md`
                // 54 times). Compaction clears the record with the gate's.
                let sid = store::current_session(&ctx.conn, pid)?;
                let shown_before = match &sid {
                    Some(s) if once => memorycastle::gate::notes_shown(&ctx.conn, s)?,
                    _ => std::collections::HashSet::new(),
                };
                let mut again = Vec::new();
                let mut i = 0usize;
                for n in notes.iter().take(cap) {
                    if shown_before.contains(&n.id) {
                        again.push(n.path.rsplit('/').next().unwrap_or(&n.path).to_string());
                        continue;
                    }
                    let sup = n.superseded_by.as_ref().map(|p| format!("  (superseded by {p})")).unwrap_or_default();
                    // A successor (`graph::with_successors`) is the current word
                    // on what the note it replaces decided: always its Decision.
                    let successor = n.via.starts_with("replaces ");
                    let via = if successor { format!("  ({})", n.via) } else if !n.via.is_empty() && n.via != target { format!("  as {}", n.via) } else { String::new() };
                    out.push_str(&format!("{}  {}  {}  ({}){}{}\n", n.path, n.title, fmt_date(n.dated.unwrap_or(n.saved_at)), n.author, sup, via));
                    if let (Some(sc), Some((m, _))) = (judged.get(&n.id), &judge_model) {
                        // The one consideration precise enough to print as the reason.
                        let why = if judged_reverses.contains(&n.id) && !judged_rejected.contains_key(&n.id) { ": the task changes or retries what it decided" } else { "" };
                        out.push_str(&format!("   judge      {sc:.2} for this task ({m}){why}\n"));
                    }
                    if let Some(p) = judged_rejected.get(&n.id) {
                        // The Rejected bullet sharing most words with the task and the
                        // new text (the judge says which note, not which bullet).
                        let body = store::section_body(&ctx.conn, n.id, "Rejected")?.unwrap_or_default();
                        let want: std::collections::HashSet<String> = memorycastle::search::content_words(&format!(
                            "{} {}",
                            for_task.as_deref().unwrap_or(""),
                            judge_edit.as_ref().map(|e| e.new.as_str()).unwrap_or("")
                        ))
                        .into_iter()
                        .collect();
                        let mut bullets: Vec<String> = Vec::new();
                        for l in body.lines() {
                            if l.trim_start().starts_with("- ") || bullets.is_empty() {
                                bullets.push(l.trim().trim_start_matches("- ").to_string());
                            } else if let Some(b) = bullets.last_mut() {
                                b.push(' ');
                                b.push_str(l.trim());
                            }
                        }
                        let line = bullets
                            .iter()
                            .enumerate()
                            .max_by_key(|(i, b)| (memorycastle::search::content_words(b).iter().filter(|w| want.contains(*w)).count(), std::cmp::Reverse(*i)))
                            .map(|(_, b)| b.clone())
                            .unwrap_or_default();
                        let line: String = line.chars().take(memorycastle::limits::TRIP_LINE_CHARS).collect();
                        out.push_str(&format!("   rejected   {p:.2} this edit may bring back what this note ruled out: {line}\n              If the reason no longer holds, say why and supersede the note; otherwise keep it out.\n"));
                    }
                    if (i < decisions || successor) && n.superseded_by.is_none() {
                        if let Some(d) = store::decision_sentence(&ctx.conn, n.id)? {
                            out.push_str(&format!("   decision   {d}\n"));
                        }
                    }
                    if let Some((l, ranges)) = store::lines_field(&ctx.conn, n.id, &target, &n.via)? {
                        out.push_str(&l);
                        store::record_note_spans(&ctx.conn, pid, n.id, &n.path, &ranges)?;
                    }
                    stood += store::note_bytes(&ctx.conn, n.id)?;
                    i += 1;
                }
                if !again.is_empty() {
                    out.push_str(&format!("   shown earlier this session: {}\n", again.join(", ")));
                }
                if let Some(s) = &sid {
                    memorycastle::gate::mark_notes_shown(&ctx.conn, s, notes.iter().take(cap).map(|n| n.id))?;
                }
                if notes.len() > cap {
                    out.push_str(&format!("   +{} more notes name {target}; `links {target} --at <line>` shows the ones about that definition\n", notes.len() - cap));
                }
                if kind == graph::Kind::File {
                    if let Some(l) = memorycastle::learned::asked_line(&ctx.conn, pid, &target)? {
                        out.push_str(&l);
                    }
                }
                // A note `links` printed is served like a search hit, so a
                // read of it afterwards counts as opened in the label log.
                let shown: Vec<i64> = notes.iter().take(cap).map(|n| n.id).collect();
                store::record_served_notes(&ctx.conn, pid, &shown, "links")?;
                if context {
                    if let Some(r) = &root {
                        for line in memorycastle::spans::context(&ctx.conn, pid, r, &defs, all)? {
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
        Cmd::Open { target, all, resume } => {
            let pid = ctx.require_project()?.id;
            if resume {
                // After a compaction: where the last note left the work, so the
                // summary's account of it is checked against what was written.
                let Some(since) = store::session_started(&ctx.conn, pid)? else { return Ok(()) };
                let last: Option<(i64, String, String)> = ctx
                    .conn
                    .query_row(
                        "SELECT id, path, title FROM notes WHERE project_id = ?1 AND deleted_at IS NULL AND superseded_by IS NULL AND saved_at >= ?2
                         AND EXISTS (SELECT 1 FROM sections s WHERE s.note_id = notes.id AND s.name = 'Open') ORDER BY saved_at DESC LIMIT 1",
                        rusqlite::params![pid, since],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map(Some)
                    .or_else(|e| if matches!(e, rusqlite::Error::QueryReturnedNoRows) { Ok(None) } else { Err(e) })?;
                let Some((id, path, title)) = last else { return Ok(()) };
                let items = store::open_items(&ctx.conn, pid, Some(&[id]))?;
                if !items.is_empty() {
                    println!("this session's last note with open items: {path}  {title}");
                    for it in items.iter().take(memorycastle::limits::RESUME_OPEN_ITEMS) {
                        let text: String = it.text.chars().take(memorycastle::limits::OPEN_ITEM_CHARS).collect();
                        println!("   {}:{}  {text}", it.path, it.line);
                    }
                }
                return Ok(());
            }
            graph::ensure_links(&mut ctx.conn, pid)?;
            let ids: Option<Vec<i64>> = match &target {
                Some(t) => {
                    let kind = if memorycastle::code::is_file_target(&ctx.conn, pid, t)? { graph::Kind::File } else { graph::Kind::Symbol };
                    let mut v: Vec<i64> = graph::notes_for(&ctx.conn, pid, kind, t)?.into_iter().map(|n| n.id).collect();
                    if v.is_empty() && kind == graph::Kind::File {
                        v = graph::notes_for(&ctx.conn, pid, graph::Kind::Symbol, t)?.into_iter().map(|n| n.id).collect();
                    }
                    Some(v)
                }
                None => None,
            };
            let items = store::open_items(&ctx.conn, pid, ids.as_deref())?;
            let shown = if all { items.len() } else { items.len().min(memorycastle::limits::OPEN_ITEMS) };
            if cli.json {
                println!("{}", serde_json::to_string(&items[..shown])?);
            } else if items.is_empty() {
                println!("no open items{}", target.as_ref().map(|t| format!(" in notes about {t}")).unwrap_or_default());
            } else {
                for it in &items[..shown] {
                    let text: String = it.text.chars().take(memorycastle::limits::OPEN_ITEM_CHARS).collect();
                    let cut = if it.text.chars().count() > memorycastle::limits::OPEN_ITEM_CHARS { "…" } else { "" };
                    println!("{}:{}  {}  {text}{cut}", it.path, it.line, fmt_date(it.dated));
                }
                if items.len() > shown {
                    println!("+{} more (--all); an item is closed by a later note that supersedes this one", items.len() - shown);
                }
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
        Cmd::Init { .. } | Cmd::Mcp => unreachable!(),
    }
    Ok(())
}

/// `target` as a path to an existing file under `root` (absolute, relative
/// to the working directory, or to the root), repo-relative.
fn repo_relative(root: &std::path::Path, target: &str) -> Option<String> {
    let t = std::path::Path::new(target);
    let cands = if t.is_absolute() { vec![t.to_path_buf()] } else { vec![std::env::current_dir().ok()?.join(t), root.join(t)] };
    let root = root.canonicalize().ok()?;
    cands.into_iter().filter_map(|c| c.canonicalize().ok()).find(|c| c.is_file()).and_then(|c| c.strip_prefix(&root).ok().map(|r| r.to_string_lossy().into_owned()))
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
