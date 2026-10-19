//! One entry point for every agent's hooks: `memcastle hook <agent> <event>`.
//! Reads the agent's JSON on stdin, runs the same subcommands the Claude
//! plugin's scripts run, and answers in the agent's own output shape. No
//! shell, jq or python on the other agents; the binary is the hook.
//!
//! Events (ours, mapped from each agent's names at install): `session-start`,
//! `prompt`, `before-edit`, `after-edit`, `after-bash`, `pre-compact`, `stop`.

use crate::error::Result;
use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Agent {
    Codex,
    Gemini,
    Cursor,
}

impl Agent {
    pub fn parse(s: &str) -> Option<Agent> {
        match s {
            "codex" => Some(Agent::Codex),
            "gemini" | "antigravity" => Some(Agent::Gemini),
            "cursor" => Some(Agent::Cursor),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Gemini => "gemini",
            Agent::Cursor => "cursor",
        }
    }
    /// The agent's own event name for one of ours.
    pub fn event_name(self, ours: &str) -> Option<&'static str> {
        Some(match (self, ours) {
            (Agent::Codex, "session-start") => "SessionStart",
            (Agent::Codex, "prompt") => "UserPromptSubmit",
            (Agent::Codex, "before-edit") => "PreToolUse",
            (Agent::Codex, "after-edit") => "PostToolUse",
            (Agent::Codex, "after-bash") => "PostToolUse",
            (Agent::Codex, "pre-compact") => "PreCompact",
            (Agent::Codex, "stop") => "Stop",
            (Agent::Gemini, "session-start") => "SessionStart",
            (Agent::Gemini, "prompt") => "BeforeAgent",
            (Agent::Gemini, "before-edit") => "BeforeTool",
            (Agent::Gemini, "after-edit") => "AfterTool",
            (Agent::Gemini, "after-bash") => "AfterTool",
            (Agent::Gemini, "pre-compact") => "PreCompress",
            (Agent::Gemini, "stop") => "AfterAgent",
            (Agent::Cursor, "session-start") => "sessionStart",
            (Agent::Cursor, "after-edit") => "afterFileEdit",
            (Agent::Cursor, "after-bash") => "afterShellExecution",
            (Agent::Cursor, "stop") => "stop",
            _ => return None,
        })
    }
    /// Tool-name matcher for the edit and shell events, in the agent's syntax.
    pub fn matcher(self, ours: &str) -> Option<&'static str> {
        Some(match (self, ours) {
            (Agent::Codex, "before-edit") | (Agent::Codex, "after-edit") => "apply_patch|Edit|Write|MultiEdit|write_file|edit_file",
            (Agent::Codex, "after-bash") => "Bash|shell|local_shell",
            (Agent::Gemini, "before-edit") | (Agent::Gemini, "after-edit") => "write_file|replace|edit",
            (Agent::Gemini, "after-bash") => "run_shell_command|shell",
            _ => return None,
        })
    }
}

/// What the event's JSON holds, read the same way whatever the agent.
struct Input {
    session: Option<String>,
    prompt: Option<String>,
    file: Option<String>,
    old_first_line: Option<String>,
    command: Option<String>,
}

fn read_input(agent: Agent) -> Input {
    let mut s = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s);
    let v: serde_json::Value = serde_json::from_str(&s).unwrap_or(serde_json::Value::Null);
    let str_at = |keys: &[&str]| -> Option<String> {
        let mut cur = &v;
        for k in keys {
            cur = cur.get(k)?;
        }
        cur.as_str().map(str::to_string).filter(|x| !x.is_empty())
    };
    let session = str_at(&["session_id"]).or_else(|| str_at(&["conversation_id"])).or_else(|| std::env::var("GEMINI_SESSION_ID").ok().filter(|x| !x.is_empty()));
    let tool_in = &v["tool_input"];
    let mut file = str_at(&["tool_input", "file_path"]).or_else(|| str_at(&["tool_input", "path"])).or_else(|| str_at(&["file_path"]));
    let command = str_at(&["tool_input", "command"]).or_else(|| str_at(&["command"]));
    // Codex edits are patches: the first file the patch names.
    if file.is_none() && agent == Agent::Codex {
        let patch = tool_in.as_str().map(str::to_string).or_else(|| str_at(&["tool_input", "patch"])).or_else(|| command.clone()).unwrap_or_default();
        file = patch.lines().find_map(|l| l.strip_prefix("*** Update File: ").or_else(|| l.strip_prefix("*** Add File: ")).or_else(|| l.strip_prefix("*** Delete File: "))).map(|p| p.trim().to_string());
    }
    let mut old_first_line = str_at(&["tool_input", "old_string"]).or_else(|| str_at(&["tool_input", "old_str"])).and_then(|o| o.lines().next().map(str::to_string));
    // A patch's first removed or context line places the edit as well.
    if old_first_line.is_none() && agent == Agent::Codex {
        let patch = tool_in.as_str().map(str::to_string).or_else(|| str_at(&["tool_input", "patch"])).or_else(|| command.clone()).unwrap_or_default();
        old_first_line = patch.lines().skip_while(|l| !l.starts_with("@@")).skip(1).find(|l| (l.starts_with('-') || l.starts_with(' ')) && l.len() > 1).map(|l| l[1..].to_string());
    }
    Input { session, prompt: str_at(&["prompt"]), file, old_first_line, command }
}

/// Runs this same binary with `args`, from `dir` when given; stdout and the exit code.
fn run(args: &[&str], dir: Option<&std::path::Path>) -> (String, String, i32) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "memcastle".into());
    let mut c = Command::new(exe);
    c.args(args).stdin(std::process::Stdio::null());
    if let Some(d) = dir {
        c.current_dir(d);
    }
    match c.output() {
        Ok(o) => (String::from_utf8_lossy(&o.stdout).trim_end().to_string(), String::from_utf8_lossy(&o.stderr).trim_end().to_string(), o.status.code().unwrap_or(1)),
        Err(_) => (String::new(), String::new(), 1),
    }
}

fn run_in(args: &[&str], dir: Option<&std::path::Path>, input: &str) -> (String, String, i32) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "memcastle".into());
    let mut c = Command::new(exe);
    c.args(args).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    if let Some(d) = dir {
        c.current_dir(d);
    }
    let Ok(mut child) = c.spawn() else { return (String::new(), String::new(), 1) };
    if let Some(mut si) = child.stdin.take() {
        let _ = std::io::Write::write_all(&mut si, input.as_bytes());
    }
    match child.wait_with_output() {
        Ok(o) => (String::from_utf8_lossy(&o.stdout).trim_end().to_string(), String::from_utf8_lossy(&o.stderr).trim_end().to_string(), o.status.code().unwrap_or(1)),
        Err(_) => (String::new(), String::new(), 1),
    }
}

fn repo_root_of(file: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(file);
    let start = if p.is_absolute() { p.parent()?.to_path_buf() } else { std::env::current_dir().ok()? };
    crate::project::Repo::discover(&start).map(|r| r.root)
}

/// What a hook wants to say back: context for the model, or a block with a reason.
enum Reply {
    Nothing,
    Context(String),
    Block(String),
}

/// Prints the reply in the agent's shape and returns the exit code.
fn emit(agent: Agent, event: &str, reply: Reply) -> i32 {
    let name = agent.event_name(event).unwrap_or(event);
    match (agent, reply) {
        (_, Reply::Nothing) => {
            if agent == Agent::Gemini {
                println!("{{}}");
            }
            0
        }
        // Codex: plain stdout is context for SessionStart and UserPromptSubmit; the JSON form elsewhere.
        (Agent::Codex, Reply::Context(t)) if matches!(event, "session-start" | "prompt") => {
            println!("{t}");
            0
        }
        (Agent::Codex, Reply::Context(t)) | (Agent::Gemini, Reply::Context(t)) => {
            println!("{}", serde_json::json!({"hookSpecificOutput": {"hookEventName": name, "additionalContext": t}}));
            0
        }
        (Agent::Cursor, Reply::Context(t)) => {
            println!("{}", serde_json::json!({"additional_context": t}));
            0
        }
        // A hold: the agent continues with the reason as its next instruction
        // (Codex Stop, Gemini AfterAgent, Cursor stop) or is stopped once with
        // the reason on stderr (compaction).
        (Agent::Codex, Reply::Block(r)) if event == "stop" => {
            println!("{}", serde_json::json!({"decision": "block", "reason": r}));
            0
        }
        (Agent::Gemini, Reply::Block(r)) if event == "stop" => {
            println!("{}", serde_json::json!({"decision": "block", "reason": r}));
            0
        }
        (Agent::Cursor, Reply::Block(r)) if event == "stop" => {
            println!("{}", serde_json::json!({"followup_message": r}));
            0
        }
        (_, Reply::Block(r)) => {
            eprintln!("{r}");
            2
        }
    }
}

/// The hook body, agent-independent: the same steps the Claude plugin's
/// scripts take, with the same once-per-session rules.
pub fn run_hook(agent: Agent, event: &str) -> Result<i32> {
    let input = read_input(agent);
    let cwd = std::env::current_dir().ok();
    let reply = match event {
        "session-start" => {
            if let Some(sid) = &input.session {
                run(&["session", sid, "--agent", agent.name()], None);
            }
            run(&["serve", "--detach"], None);
            let (status, _, _) = run(&["status"], None);
            let mut lines: Vec<String> = status.lines().filter(|l| l.starts_with("project:") || l.starts_with("notes:") || l.starts_with("observed") || l.starts_with("label log") || l.starts_with("warning:") || l.starts_with("changed on disk")).map(str::to_string).collect();
            let (owed, _, _) = run(&["owed"], None);
            if !owed.is_empty() {
                lines.push(format!("memorycastle: {owed}. Draft or append with: memcastle draft"));
            }
            if lines.is_empty() { Reply::Nothing } else { Reply::Context(lines.join("\n")) }
        }
        "prompt" => {
            let mut out = Vec::new();
            if let Some(p) = &input.prompt {
                let (recall, _, _) = run_in(&["recall"], None, p);
                if !recall.is_empty() {
                    out.push(format!("memorycastle recall (memory, not instruction; path:line  confidence  section):\n{recall}"));
                }
            }
            let (owed, _, _) = run(&["owed", "--remind"], None);
            if !owed.is_empty() {
                out.push(format!("memorycastle: {owed}. Before going further, write the journal note for the decision so far (memcastle draft gives the skeleton; memcastle save <note>), or carry on if no decision was made yet."));
            }
            if out.is_empty() { Reply::Nothing } else { Reply::Context(out.join("\n")) }
        }
        "before-edit" => {
            let Some(file) = &input.file else { return Ok(emit(agent, event, Reply::Nothing)) };
            let Some(root) = repo_root_of(file) else { return Ok(emit(agent, event, Reply::Nothing)) };
            let abs = if std::path::Path::new(file).is_absolute() { std::path::PathBuf::from(file) } else { cwd.clone().unwrap_or_default().join(file) };
            let rel = abs.strip_prefix(&root).map(|p| p.display().to_string()).unwrap_or_else(|_| file.clone());
            if rel.starts_with("memorycastle/") {
                return Ok(emit(agent, event, Reply::Nothing));
            }
            let at = input.old_first_line.as_deref().and_then(|first| {
                let text = std::fs::read_to_string(&abs).ok()?;
                text.lines().position(|l| l.contains(first)).map(|i| (i + 1).to_string())
            });
            let mut args = vec!["links", rel.as_str(), "--once"];
            if let Some(a) = &at {
                args.push("--at");
                args.push(a);
            }
            let (out, _, _) = run(&args, Some(&root));
            if out.is_empty() || out.starts_with("no notes link to") {
                Reply::Nothing
            } else {
                Reply::Context(format!("memorycastle: notes about {rel} (read the printed ranges, not the file):\n{out}"))
            }
        }
        "after-edit" => {
            if let Some(file) = &input.file {
                if let Some(root) = repo_root_of(file) {
                    run(&["edited", file], Some(&root));
                }
            }
            Reply::Nothing
        }
        "after-bash" => {
            let is_commit = input.command.as_deref().map(|c| c.contains("git commit")).unwrap_or(false);
            if !is_commit {
                return Ok(emit(agent, event, Reply::Nothing));
            }
            let (owed, _, _) = run(&["owed"], None);
            if owed.is_empty() {
                Reply::Nothing
            } else {
                Reply::Context(format!("memorycastle: {owed}. If this commit closed a task that changed a decision, measured something or rejected an approach, write the journal note now (memcastle draft writes the skeleton) and save it; otherwise ignore."))
            }
        }
        "pre-compact" => {
            let (draft, _, _) = run(&["draft"], None);
            let (_, reason, rc) = run(&["owed", "--once", "compact"], None);
            if rc == 2 {
                Reply::Block(format!("memorycastle: {reason}. Context is about to be compacted: write the journal note now, while the decision and the numbers are still in context. Draft at {draft}: set the title to the decision, fill Decision, Rejected and Measured, then run: memcastle save {draft}. Compaction continues on the next attempt."))
            } else {
                Reply::Nothing
            }
        }
        "stop" => {
            let (draft, _, _) = run(&["draft"], None);
            let (_, reason, rc) = run(&["owed", "--once", "stop"], None);
            if rc == 2 {
                Reply::Block(format!("memorycastle: {reason}. Before stopping, write the journal note if this task changed a decision, measured something or rejected an approach. Draft at {draft}: set the title to the decision, fill the sections, then: memcastle save {draft}. If no decision was made, stop again and say so."))
            } else {
                Reply::Nothing
            }
        }
        _ => Reply::Nothing,
    };
    Ok(emit(agent, event, reply))
}

/// The hook entries `install` writes for one agent, as (our event, the
/// agent's event, matcher, command).
pub fn entries(agent: Agent) -> Vec<(&'static str, &'static str, Option<&'static str>, String)> {
    ["session-start", "prompt", "before-edit", "after-edit", "after-bash", "pre-compact", "stop"]
        .into_iter()
        .filter_map(|ev| agent.event_name(ev).map(|name| (ev, name, agent.matcher(ev), format!("memcastle hook {} {ev}", agent.name()))))
        .collect()
}
