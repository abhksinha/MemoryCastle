//! Project identity and search scope.
//!
//! A project is identified by its git remote, normalised to `host/owner/repo`,
//! so SSH and HTTPS clones, worktrees and moved checkouts are one project. A
//! repo with no remote is identified by its main worktree path. Git is read from
//! disk directly; no `git` process is spawned.

use crate::db::placeholders;
use crate::{err, sys, Result};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// A resolved git checkout.
#[derive(Debug, Clone)]
pub struct Repo {
    /// The worktree root that contains the current directory.
    pub root: PathBuf,
    /// The main worktree (same as `root` unless this is a linked worktree).
    pub main: PathBuf,
    /// The normalised remote, if any.
    pub remote: Option<String>,
}

impl Repo {
    /// Walks up from `start` to the first directory with a `.git` entry.
    pub fn discover(start: &Path) -> Option<Repo> {
        let mut dir = start.to_path_buf();
        loop {
            let dot_git = dir.join(".git");
            if dot_git.is_dir() {
                let remote = read_remote(&dot_git.join("config"));
                return Some(Repo { root: dir.clone(), main: dir, remote });
            }
            if dot_git.is_file() {
                // Linked worktree: ".git" holds "gitdir: <path>/.git/worktrees/<name>".
                let content = std::fs::read_to_string(&dot_git).ok()?;
                let gitdir = content.trim().strip_prefix("gitdir:")?.trim();
                let gitdir = resolve_relative(&dir, Path::new(gitdir));
                let common = std::fs::read_to_string(gitdir.join("commondir"))
                    .ok()
                    .map(|c| resolve_relative(&gitdir, Path::new(c.trim())))
                    .unwrap_or_else(|| gitdir.clone());
                let main = common.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.clone());
                let remote = read_remote(&common.join("config"));
                return Some(Repo { root: dir, main, remote });
            }
            if !dir.pop() {
                return None;
            }
        }
    }

    /// The string the project id is hashed from.
    pub fn identity(&self) -> String {
        match &self.remote {
            Some(r) => r.clone(),
            None => self.main.to_string_lossy().into_owned(),
        }
    }

    pub fn id(&self) -> i64 {
        project_id_of(&self.identity())
    }

    pub fn default_name(&self) -> String {
        self.main
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into())
    }
}

fn resolve_relative(base: &Path, p: &Path) -> PathBuf {
    normalise_path(&if p.is_absolute() { p.to_path_buf() } else { base.join(p) })
}

/// Lexically removes `.` and `..` so `parent()` means the real parent.
fn normalise_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Reads `[remote "origin"] url`, else the first remote url, from a git config.
fn read_remote(config: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config).ok()?;
    let mut in_remote = false;
    let mut is_origin = false;
    let mut first: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_remote = line.starts_with("[remote ");
            is_origin = line == "[remote \"origin\"]";
            continue;
        }
        if !in_remote {
            continue;
        }
        if let Some(rest) = line.strip_prefix("url") {
            let rest = rest.trim_start();
            if let Some(url) = rest.strip_prefix('=') {
                let url = url.trim();
                if is_origin {
                    return Some(normalise_remote(url));
                }
                if first.is_none() {
                    first = Some(normalise_remote(url));
                }
            }
        }
    }
    first
}

/// `git@github.com:Abhi/x.git`, `ssh://git@github.com/abhi/x`, and
/// `https://github.com/abhi/x.git` all become `github.com/abhi/x`.
pub fn normalise_remote(url: &str) -> String {
    let mut s = url.trim();
    // scheme
    if let Some(i) = s.find("://") {
        s = &s[i + 3..];
    }
    // user@
    if let Some(i) = s.find('@') {
        if !s[..i].contains('/') {
            s = &s[i + 1..];
        }
    }
    // scp-style host:path (only when the part before ':' has no '/')
    let mut out = String::with_capacity(s.len());
    if let Some(i) = s.find(':') {
        if !s[..i].contains('/') {
            let host = &s[..i];
            let rest = &s[i + 1..];
            // ssh://host:22/path keeps a port; drop it.
            let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches('/');
            out.push_str(host);
            out.push('/');
            out.push_str(rest);
            s = "";
        }
    }
    if !s.is_empty() {
        out.push_str(s);
    }
    let mut out = out.trim_end_matches('/').to_string();
    if out.ends_with(".git") {
        out.truncate(out.len() - 4);
    }
    out.make_ascii_lowercase();
    out
}

/// First 8 bytes of SHA-256, made non-negative so it reads cleanly in SQL.
pub fn project_id_of(identity: &str) -> i64 {
    let h = Sha256::digest(identity.as_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&h[..8]);
    (i64::from_le_bytes(b) & i64::MAX).max(1)
}

#[derive(Debug, Clone)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub paths: Vec<String>,
    pub group: Option<String>,
}

fn row_to_project(r: &rusqlite::Row) -> rusqlite::Result<Project> {
    let paths: String = r.get(2)?;
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        paths: serde_json::from_str(&paths).unwrap_or_default(),
        group: r.get(3)?,
    })
}

pub fn by_id(conn: &Connection, id: i64) -> Result<Option<Project>> {
    Ok(conn
        .query_row("SELECT id, name, paths, grp FROM projects WHERE id = ?1", [id], row_to_project)
        .optional()?)
}

pub fn by_name(conn: &Connection, name: &str) -> Result<Option<Project>> {
    Ok(conn
        .query_row("SELECT id, name, paths, grp FROM projects WHERE name = ?1", [name], row_to_project)
        .optional()?)
}

pub fn list(conn: &Connection) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare("SELECT id, name, paths, grp FROM projects ORDER BY name")?;
    let rows = stmt.query_map([], row_to_project)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Projects whose known paths include `path`.
pub fn by_path(conn: &Connection, path: &str) -> Result<Option<Project>> {
    // A few projects at most: scanning them is cheaper than a JSON index.
    Ok(list(conn)?.into_iter().find(|p| p.paths.iter().any(|q| q == path)))
}

/// Registers the project for `repo`, or updates its known paths. Returns the
/// stored project. Errors when the id is unknown but the path belongs to a
/// project with a different id (a remote change), so memory never forks.
pub fn ensure(conn: &Connection, repo: &Repo) -> Result<Project> {
    let id = repo.id();
    let main = repo.main.to_string_lossy().into_owned();
    if let Some(mut p) = by_id(conn, id)? {
        if !p.paths.contains(&main) {
            p.paths.push(main);
            conn.execute(
                "UPDATE projects SET paths = ?1 WHERE id = ?2",
                params![serde_json::to_string(&p.paths)?, id],
            )?;
        }
        return Ok(p);
    }
    if let Some(p) = by_path(conn, &main)? {
        return Err(err!(
            "this checkout is known as project '{}' but its remote now resolves to a different id; run `memorycastle project relink`",
            p.name
        ));
    }
    let mut name = repo.default_name();
    if by_name(conn, &name)?.is_some() {
        name = format!("{name}-{:x}", id & 0xffff);
    }
    let paths = vec![main];
    conn.execute(
        "INSERT INTO projects(id, name, paths, grp, added_at) VALUES (?1, ?2, ?3, NULL, ?4)",
        params![id, name, serde_json::to_string(&paths)?, sys::now()],
    )?;
    Ok(Project { id, name, paths, group: None })
}

/// Re-attaches memory stored under an old id to the id the checkout now
/// resolves to. One transaction across every table that carries `project_id`.
pub fn relink(conn: &mut Connection, repo: &Repo) -> Result<Option<(i64, i64)>> {
    let new_id = repo.id();
    let main = repo.main.to_string_lossy().into_owned();
    let Some(old) = by_path(conn, &main)? else {
        return Ok(None);
    };
    if old.id == new_id {
        return Ok(None);
    }
    let tx = conn.transaction()?;
    tx.execute("PRAGMA defer_foreign_keys = ON", [])?;
    tx.execute("UPDATE projects SET id = ?1 WHERE id = ?2", params![new_id, old.id])?;
    tx.execute("UPDATE notes SET project_id = ?1 WHERE project_id = ?2", params![new_id, old.id])?;
    tx.execute("UPDATE chunks SET project_id = ?1 WHERE project_id = ?2", params![new_id, old.id])?;
    tx.execute("UPDATE vectors SET project_id = ?1 WHERE project_id = ?2", params![new_id, old.id])?;
    tx.execute("UPDATE links SET project_id = ?1 WHERE project_id = ?2", params![new_id, old.id])?;
    tx.execute("DELETE FROM vector_bits WHERE project_id = ?2", params![new_id, old.id])?;
    // chunks_fts.project_id comes from the content table; rebuild its copy.
    tx.execute("INSERT INTO chunks_fts(chunks_fts) VALUES ('rebuild')", [])?;
    tx.commit()?;
    Ok(Some((old.id, new_id)))
}

pub fn set_group(conn: &Connection, name: &str, group: Option<&str>) -> Result<bool> {
    Ok(conn.execute("UPDATE projects SET grp = ?1 WHERE name = ?2", params![group, name])? == 1)
}

/// The set of projects a search may see, each with a rank weight.
/// Every channel filters on `ids` before ranking.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Sorted by id; no duplicates.
    pub members: Vec<(i64, f32)>,
}

impl Scope {
    pub fn single(id: i64) -> Scope {
        Scope { members: vec![(id, 1.0)] }
    }

    pub fn add(&mut self, id: i64, weight: f32) {
        match self.members.binary_search_by_key(&id, |m| m.0) {
            Ok(i) => self.members[i].1 = self.members[i].1.max(weight),
            Err(i) => self.members.insert(i, (id, weight)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn weight(&self, id: i64) -> f32 {
        self.members
            .binary_search_by_key(&id, |m| m.0)
            .map(|i| self.members[i].1)
            .unwrap_or(0.0)
    }

    pub fn ids(&self) -> impl Iterator<Item = i64> + '_ {
        self.members.iter().map(|m| m.0)
    }

    /// `(?a,?b,...)` numbered from `start`, for binding with [`Scope::bind`].
    pub fn placeholders(&self, start: usize) -> String {
        placeholders(start, self.members.len())
    }

    pub fn bind(&self) -> impl Iterator<Item = rusqlite::types::Value> + '_ {
        self.members.iter().map(|m| rusqlite::types::Value::Integer(m.0))
    }
}

/// How the user asked to widen the scope.
#[derive(Debug, Clone, Default)]
pub struct ScopeFlags {
    pub related: bool,
    pub project_glob: Option<String>,
    pub all: bool,
}

pub use crate::limits::SIBLING_WEIGHT;

/// Resolves the scope for a search. `current` is the project the caller is in,
/// if any. Errors when nothing selects a project.
pub fn resolve_scope(conn: &Connection, current: Option<&Project>, flags: &ScopeFlags) -> Result<Scope> {
    let mut scope = Scope::default();
    if let Some(p) = current {
        scope.add(p.id, 1.0);
    }
    if flags.all {
        for p in list(conn)? {
            scope.add(p.id, 1.0);
        }
        return Ok(scope);
    }
    if let Some(glob) = &flags.project_glob {
        let mut any = false;
        for p in list(conn)? {
            if glob_match(glob, &p.name) {
                scope.add(p.id, 1.0);
                any = true;
            }
        }
        if !any {
            return Err(err!("no project matches '{glob}'"));
        }
    }
    if flags.related {
        match current.and_then(|p| p.group.as_deref()) {
            Some(group) => {
                let mut stmt = conn.prepare("SELECT id FROM projects WHERE grp = ?1")?;
                for id in stmt.query_map([group], |r| r.get::<_, i64>(0))? {
                    scope.add(id?, SIBLING_WEIGHT);
                }
            }
            // No group declared: isolation wins, and the user is told how to
            // declare the sisters instead of silently searching nothing more.
            None => {
                let others = list(conn)?.len().saturating_sub(1);
                if others > 0 {
                    eprintln!("memorycastle: --related covers nothing: this project is in no group ({others} other projects in the store); run `memorycastle project group <name>` here and in each sister repo");
                }
            }
        }
    }
    if scope.is_empty() {
        return Err("not inside a git repository; pass --project <name> or --all".into());
    }
    Ok(scope)
}

/// `*` matches any run of characters, `?` one character. Case-insensitive.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remotes_normalise_to_one_id() {
        let a = normalise_remote("git@github.com:Abhi/x.git");
        let b = normalise_remote("https://github.com/abhi/x");
        let c = normalise_remote("ssh://git@github.com:22/Abhi/X.git/");
        assert_eq!(a, "github.com/abhi/x");
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(project_id_of(&a), project_id_of(&b));
    }

    #[test]
    fn glob() {
        assert!(glob_match("phys*", "Physics_ISP"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("phys", "physics"));
        assert!(glob_match("a?c", "abc"));
    }

    #[test]
    fn scope_dedups_and_keeps_max_weight() {
        let mut s = Scope::single(5);
        s.add(3, 0.6);
        s.add(5, 0.6);
        assert_eq!(s.members, vec![(3, 0.6), (5, 1.0)]);
        assert_eq!(s.placeholders(2), "(?2,?3)");
    }
}
