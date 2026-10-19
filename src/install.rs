//! `memorycastle install`: one command that wires every coding agent on the
//! machine to this store. Per agent: the MCP server in its config (Codex,
//! Gemini CLI, Antigravity, Cursor, GitHub Copilot, OpenCode), the plugin
//! and status line (Claude Code), and
//! the memory section in the instruction file it reads (`CLAUDE.md`,
//! `AGENTS.md`, `GEMINI.md`). Per repo: the journal directory, the code
//! index, notes saved, past transcripts imported into the ledger, and the
//! embedding server started. Every step is idempotent and reported.

use crate::app::{AGENT_SNIPPET, SNIPPET_MARKER};
use crate::{err, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Gemini,
    Antigravity,
    Cursor,
    Copilot,
    OpenCode,
}

impl Agent {
    pub const ALL: [Agent; 7] = [Agent::Claude, Agent::Codex, Agent::Gemini, Agent::Antigravity, Agent::Cursor, Agent::Copilot, Agent::OpenCode];

    pub fn parse(s: &str) -> Option<Agent> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "cc" => Agent::Claude,
            "codex" => Agent::Codex,
            "gemini" | "gemini-cli" => Agent::Gemini,
            "antigravity" | "agy" => Agent::Antigravity,
            "cursor" => Agent::Cursor,
            "copilot" | "github-copilot" | "gh" => Agent::Copilot,
            "opencode" => Agent::OpenCode,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Gemini => "Gemini CLI",
            Agent::Antigravity => "Antigravity",
            Agent::Cursor => "Cursor",
            Agent::Copilot => "GitHub Copilot",
            Agent::OpenCode => "OpenCode",
        }
    }

    /// The instruction file in a repo this agent reads.
    pub fn instruction_file(self) -> &'static str {
        match self {
            Agent::Claude => "CLAUDE.md",
            Agent::Codex | Agent::Antigravity | Agent::Cursor | Agent::Copilot | Agent::OpenCode => "AGENTS.md",
            Agent::Gemini => "GEMINI.md",
        }
    }

    /// The user-level instruction file this agent reads in every repo, so
    /// the memory rules apply everywhere once, with no per-repo step.
    /// Antigravity, Cursor and Copilot keep their user-level rules in the
    /// app, not a file; they read the repo's `AGENTS.md`.
    pub fn user_instruction_file(self, home: &Path) -> Option<PathBuf> {
        Some(match self {
            Agent::Claude => home.join(".claude/CLAUDE.md"),
            Agent::Codex => home.join(".codex/AGENTS.md"),
            Agent::Gemini => home.join(".gemini/GEMINI.md"),
            Agent::OpenCode => home.join(".config/opencode/AGENTS.md"),
            Agent::Antigravity | Agent::Cursor | Agent::Copilot => return None,
        })
    }

    /// Whether the agent is on this machine: its config dir or binary.
    pub fn detected(self) -> bool {
        let home = home();
        match self {
            Agent::Claude => home.join(".claude").is_dir() || on_path("claude"),
            Agent::Codex => home.join(".codex").is_dir() || on_path("codex"),
            Agent::Gemini => home.join(".gemini/settings.json").is_file() || on_path("gemini"),
            Agent::Antigravity => home.join(".gemini/antigravity").is_dir() || home.join(".antigravity").is_dir() || on_path("agy"),
            Agent::Cursor => home.join(".cursor").is_dir() || on_path("cursor"),
            Agent::Copilot => home.join(".copilot").is_dir() || on_path("copilot"),
            Agent::OpenCode => home.join(".config/opencode").is_dir() || on_path("opencode"),
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// What one install step did, for the report.
pub struct Step {
    pub what: String,
    pub done: bool,
}

pub struct Options {
    pub dry_run: bool,
    /// Skip the machine-level config (plugin, MCP entries, status line).
    pub repo_only: bool,
}

/// Inserts or refreshes the memory section in an instruction file.
/// Returns the new text when it changed.
pub fn upsert_snippet(text: &str) -> Option<String> {
    const HEAD: &str = "## Memory (memorycastle)";
    if text.contains(SNIPPET_MARKER) {
        return None;
    }
    let mut out = String::with_capacity(text.len() + AGENT_SNIPPET.len());
    match text.find(HEAD) {
        Some(start) => {
            // Replace the old section: up to the marker line of an older
            // snippet, else up to the next top-level section.
            let rest = &text[start + HEAD.len()..];
            let end = rest
                .find("<!-- memorycastle snippet")
                .and_then(|i| rest[i..].find('\n').map(|j| i + j + 1))
                .or_else(|| rest.find("\n## ").map(|i| i + 1))
                .unwrap_or(rest.len());
            out.push_str(&text[..start]);
            out.push_str(AGENT_SNIPPET);
            let tail = &rest[end..];
            if !tail.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(tail);
        }
        None => {
            out.push_str(text);
            if !text.is_empty() && !text.ends_with('\n') {
                out.push('\n');
            }
            if !text.is_empty() {
                out.push('\n');
            }
            out.push_str(AGENT_SNIPPET);
        }
    }
    Some(out)
}

fn write_if(path: &Path, text: &str, dry: bool) -> Result<()> {
    if dry {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text).map_err(|e| err!("{}: {e}", path.display()))
}

/// The memory section upserted into one instruction file, repo or user level.
fn install_snippet(path: &Path, label: &str, o: &Options) -> Result<Step> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    match upsert_snippet(&text) {
        Some(new) => {
            write_if(path, &new, o.dry_run)?;
            Ok(Step { what: format!("{label}: memory section {}", if text.is_empty() { "created" } else if text.contains("## Memory (memorycastle)") { "refreshed" } else { "added" }), done: true })
        }
        None => Ok(Step { what: format!("{label}: memory section current"), done: false }),
    }
}

/// The repo-level part for one agent: its instruction file.
pub fn install_repo(root: &Path, agent: Agent, o: &Options) -> Result<Step> {
    install_snippet(&root.join(agent.instruction_file()), agent.instruction_file(), o)
}

/// Machine-level config for one agent.
pub fn install_global(agent: Agent, o: &Options) -> Result<Vec<Step>> {
    let home = home();
    let mut steps = Vec::new();
    if let Some(path) = agent.user_instruction_file(&home) {
        let label = format!("~/{}", path.strip_prefix(&home).unwrap_or(&path).display());
        steps.push(install_snippet(&path, &label, o)?);
    }
    match agent {
        Agent::Claude => {
            steps.push(claude_plugin(o)?);
            steps.push(claude_statusline(&home, o)?);
        }
        Agent::Codex => {
            let path = home.join(".codex/config.toml");
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.contains("[mcp_servers.memorycastle]") {
                steps.push(Step { what: "~/.codex/config.toml: MCP server present".into(), done: false });
            } else {
                let mut new = text.clone();
                if !new.is_empty() && !new.ends_with('\n') {
                    new.push('\n');
                }
                new.push_str("\n[mcp_servers.memorycastle]\ncommand = \"memorycastle\"\nargs = [\"mcp\"]\n");
                write_if(&path, &new, o.dry_run)?;
                steps.push(Step { what: "~/.codex/config.toml: MCP server added".into(), done: true });
            }
        }
        Agent::Gemini => steps.push(json_mcp(&home.join(".gemini/settings.json"), "~/.gemini/settings.json", "mcpServers", STDIO_ENTRY, o)?),
        Agent::Antigravity => steps.push(json_mcp(&home.join(".gemini/antigravity/mcp_config.json"), "~/.gemini/antigravity/mcp_config.json", "mcpServers", STDIO_ENTRY, o)?),
        Agent::Cursor => steps.push(json_mcp(&home.join(".cursor/mcp.json"), "~/.cursor/mcp.json", "mcpServers", STDIO_ENTRY, o)?),
        Agent::Copilot => steps.push(json_mcp(&home.join(".copilot/mcp-config.json"), "~/.copilot/mcp-config.json", "mcpServers", r#"{"type": "local", "command": "memorycastle", "args": ["mcp"], "tools": ["*"]}"#, o)?),
        Agent::OpenCode => steps.push(json_mcp(&home.join(".config/opencode/opencode.json"), "~/.config/opencode/opencode.json", "mcp", r#"{"type": "local", "command": ["memorycastle", "mcp"], "enabled": true}"#, o)?),
    }
    Ok(steps)
}

/// The stdio server entry most agents take (`mcpServers.<name>`).
const STDIO_ENTRY: &str = r#"{"command": "memorycastle", "args": ["mcp"]}"#;

/// `<key>.memorycastle` in a JSON settings file, other keys untouched.
fn json_mcp(path: &Path, shown: &str, key: &str, entry: &str, o: &Options) -> Result<Step> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".into());
    let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| err!("{shown}: not JSON ({e})"))?;
    let servers = v.as_object_mut().ok_or_else(|| err!("{shown}: top level is not an object"))?.entry(key).or_insert_with(|| serde_json::json!({}));
    let servers = servers.as_object_mut().ok_or_else(|| err!("{shown}: {key} is not an object"))?;
    if servers.contains_key("memorycastle") {
        return Ok(Step { what: format!("{shown}: MCP server present"), done: false });
    }
    servers.insert("memorycastle".into(), serde_json::from_str(entry)?);
    write_if(path, &format!("{}\n", serde_json::to_string_pretty(&v)?), o.dry_run)?;
    Ok(Step { what: format!("{shown}: MCP server added"), done: true })
}

/// The Claude Code plugin through the `claude` CLI: marketplace then plugin.
fn claude_plugin(o: &Options) -> Result<Step> {
    let installed = std::fs::read_to_string(home().join(".claude/plugins/installed_plugins.json")).unwrap_or_default();
    if installed.contains("memorycastle@memorycastle") {
        return Ok(Step { what: "Claude Code plugin installed".into(), done: false });
    }
    if !on_path("claude") {
        return Ok(Step { what: "Claude Code plugin: `claude` not on PATH; run: claude plugin marketplace add abhksinha/MemoryCastle && claude plugin install memorycastle@memorycastle".into(), done: false });
    }
    if o.dry_run {
        return Ok(Step { what: "Claude Code plugin: would run claude plugin marketplace add / install".into(), done: true });
    }
    let run = |args: &[&str]| std::process::Command::new("claude").args(args).stdin(std::process::Stdio::null()).output();
    let _ = run(&["plugin", "marketplace", "add", "abhksinha/MemoryCastle"]);
    match run(&["plugin", "install", "memorycastle@memorycastle"]) {
        Ok(out) if out.status.success() => Ok(Step { what: "Claude Code plugin installed (restart Claude Code to load its hooks)".into(), done: true }),
        Ok(out) => Ok(Step { what: format!("Claude Code plugin: install failed: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").trim()), done: false }),
        Err(e) => Ok(Step { what: format!("Claude Code plugin: could not run claude: {e}"), done: false }),
    }
}

/// The status line script and its setting, when the user has none.
fn claude_statusline(home: &Path, o: &Options) -> Result<Step> {
    let script = home.join(".claude/statusline.sh");
    let settings = home.join(".claude/settings.json");
    let had_script = script.is_file();
    let ours = std::fs::read_to_string(&script).map(|t| t.contains("memorycastle")).unwrap_or(false);
    if had_script && !ours {
        return Ok(Step { what: "~/.claude/statusline.sh exists and is not memorycastle's; left alone".into(), done: false });
    }
    if !had_script || std::fs::read_to_string(&script).map(|t| t != STATUSLINE).unwrap_or(true) {
        write_if(&script, STATUSLINE, o.dry_run)?;
        if !o.dry_run {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755));
        }
    }
    let text = std::fs::read_to_string(&settings).unwrap_or_else(|_| "{}".into());
    let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| err!("~/.claude/settings.json: not JSON ({e})"))?;
    let obj = v.as_object_mut().ok_or_else(|| err!("~/.claude/settings.json: top level is not an object"))?;
    if obj.contains_key("statusLine") {
        return Ok(Step { what: format!("~/.claude/statusline.sh {}; statusLine setting present", if had_script { "refreshed" } else { "installed" }), done: !had_script });
    }
    obj.insert("statusLine".into(), serde_json::json!({"type": "command", "command": "~/.claude/statusline.sh"}));
    write_if(&settings, &format!("{}\n", serde_json::to_string_pretty(&v)?), o.dry_run)?;
    Ok(Step { what: "~/.claude/statusline.sh installed and set in ~/.claude/settings.json".into(), done: true })
}

/// The status line script `install` writes for Claude Code.
pub const STATUSLINE: &str = r#"#!/bin/sh
# Claude Code status line (installed by memorycastle): model, repo, notes,
# and savings. "~N tokens saved" is the observed ceiling: bytes of files a
# printed span stood in for, minus what was read and the lookup output.
input=$(cat)
model=$(printf '%s' "$input" | jq -r '.model.display_name // ""')
dir=$(printf '%s' "$input" | jq -r '.workspace.current_dir // .cwd // ""')
line="$model  $(basename "$dir")"
if command -v memorycastle >/dev/null 2>&1; then
  mc=$(cd "$dir" 2>/dev/null && memorycastle status 2>/dev/null)
  notes=$(printf '%s' "$mc" | sed -n 's/^notes: \([0-9]*\).*/\1/p')
  ses=$(printf '%s' "$mc" | sed -n 's/^observed (this session): [0-9.k-]*-\([0-9.k]*\) tokens saved ([0-9-]*-\([0-9]*%\)).*/~\1 (\2) this session/p')
  rec=$(printf '%s' "$mc" | sed -n 's/^observed (last \([0-9]*\) sessions): [0-9.k-]*-\([0-9.k]*\) tokens saved ([0-9-]*-\([0-9]*%\)).*/~\2 (\3) last \1 sessions/p')
  [ -n "$notes" ] && line="$line | $notes notes"
  sav=""
  [ -n "$ses" ] && sav="$ses"
  [ -n "$rec" ] && sav="${sav:+$sav, }$rec"
  [ -n "$sav" ] && line="$line | $sav tokens saved"
fi
printf '%s' "$line"
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_added_refreshed_and_left_alone() {
        let added = upsert_snippet("# CLAUDE.md\n\nBuild with cargo.\n").unwrap();
        assert!(added.starts_with("# CLAUDE.md\n\nBuild with cargo.\n\n## Memory (memorycastle)"));
        assert!(added.contains(SNIPPET_MARKER));
        assert!(upsert_snippet(&added).is_none(), "current snippet is left alone");
        let old = "# X\n\n## Memory (memorycastle)\n\nold text\n<!-- memorycastle snippet 1 -->\n\n## After\nkeep\n";
        let refreshed = upsert_snippet(old).unwrap();
        assert!(!refreshed.contains("old text"));
        assert!(refreshed.contains("## After\nkeep"), "{refreshed}");
        assert_eq!(refreshed.matches("## Memory (memorycastle)").count(), 1);
        let unmarked = "# X\n\n## Memory (memorycastle)\nold\n## After\nkeep\n";
        let r = upsert_snippet(unmarked).unwrap();
        assert!(!r.contains("\nold\n") && r.contains("## After\nkeep"), "{r}");
    }

    #[test]
    fn agent_aliases() {
        assert_eq!(Agent::parse("agy"), Some(Agent::Antigravity));
        assert_eq!(Agent::parse("Claude"), Some(Agent::Claude));
        assert_eq!(Agent::parse("vim"), None);
    }
}
