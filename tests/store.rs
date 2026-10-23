//! Integration tests over an in-memory store: isolation, save/search/forget,
//! FTS quoting against real FTS5, fusion with the stub embedder, relink.

use memorycastle::embed::{self, HashEmbedder};
use memorycastle::project::{self, Project, Scope, ScopeFlags};
use memorycastle::search::{search, Hit, Mode, ModelSource, SearchOptions};
use memorycastle::store::{self, SaveOptions, SaveOutcome};
use memorycastle::{db, graph, Repo};
use rusqlite::Connection;

fn project(conn: &Connection, name: &str, group: Option<&str>) -> Project {
    let id = project::project_id_of(&format!("github.com/t/{name}"));
    conn.execute(
        "INSERT INTO projects(id, name, paths, grp, added_at) VALUES (?1, ?2, '[]', ?3, 0)",
        rusqlite::params![id, name, group],
    )
    .unwrap();
    Project { id, name: name.into(), paths: vec![], group: group.map(str::to_string) }
}

fn save(conn: &mut Connection, p: &Project, path: &str, content: &str) -> SaveOutcome {
    store::save_note(conn, p, path, content, &SaveOptions { source: "user", force: false }).unwrap()
}

fn kw(conn: &Connection, scope: &Scope, q: &str) -> Vec<String> {
    let (hits, _) = search(conn, scope, q, &SearchOptions { k: 10, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    hits.into_iter().map(|h| h.path).collect()
}

#[test]
fn isolation_project_a_never_sees_project_b() {
    let mut conn = db::open_memory().unwrap();
    let a = project(&conn, "a", Some("g"));
    let b = project(&conn, "b", Some("g"));
    let c = project(&conn, "c", None);
    save(&mut conn, &b, "secret.md", "# Only in B\nThe sentinel ZEBRA-7781 lives here.");
    save(&mut conn, &a, "a.md", "# A note\nnothing about zebras");
    save(&mut conn, &c, "c.md", "# C note\nsomething else");

    // Plain scope of A, with and without a stub meaning channel.
    let scope_a = Scope::single(a.id);
    assert!(kw(&conn, &scope_a, "ZEBRA-7781").is_empty());
    let space = embed::ensure_space(&conn, &HashEmbedder::space_def(64)).unwrap();
    let mut e = HashEmbedder { dims: 64 };
    store::embed_missing(&mut conn, &space, &mut e, None).unwrap();
    let (hits, ex) = search(&conn, &scope_a, "ZEBRA-7781", &SearchOptions { k: 10, mode: Mode::Meaning, working: Vec::new() }, ModelSource::Warm(&mut e)).unwrap();
    assert!(ex.model_used);
    assert!(hits.iter().all(|h| h.project == "a"), "meaning channel leaked across projects: {hits:?}");

    // --related from C (no group) sees nothing from B.
    let scope_c = project::resolve_scope(&conn, Some(&c), &ScopeFlags { related: true, ..Default::default() }).unwrap();
    assert!(kw(&conn, &scope_c, "ZEBRA-7781").is_empty());

    // --related from A (same group) does see B, down-weighted and labelled.
    let scope_rel = project::resolve_scope(&conn, Some(&a), &ScopeFlags { related: true, ..Default::default() }).unwrap();
    let (hits, _) = search(&conn, &scope_rel, "ZEBRA-7781", &SearchOptions { k: 10, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].project, "b");
    assert_eq!(hits[0].trace.weight, project::SIBLING_WEIGHT);

    // A glob naming B sees it; --all sees it.
    let scope_glob = project::resolve_scope(&conn, Some(&a), &ScopeFlags { project_glob: Some("b*".into()), ..Default::default() }).unwrap();
    assert_eq!(kw(&conn, &scope_glob, "ZEBRA-7781"), vec!["secret.md"]);
    let scope_all = project::resolve_scope(&conn, Some(&a), &ScopeFlags { all: true, ..Default::default() }).unwrap();
    assert_eq!(kw(&conn, &scope_all, "ZEBRA-7781"), vec!["secret.md"]);
}

#[test]
fn identifiers_are_found_through_fts5() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "n.md", "# Status\nB-42 was fixed in ui/status/StatusGl.kt by snake_case_fn on 2026-10-18. Rotated.");
    let s = Scope::single(p.id);
    for q in ["B-42", "snake_case_fn", "2026-10-18", "StatusGl.kt", "ui/status/StatusGl.kt", "rotated", "\"B-42\"", "rotated.", "where was B-42 fixed"] {
        assert_eq!(kw(&conn, &s, q), vec!["n.md"], "query {q:?}");
    }
    assert!(kw(&conn, &s, "B-43").is_empty());
    assert!(kw(&conn, &s, "?? --").is_empty());
}

#[test]
fn save_update_forget_roundtrip() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    assert_eq!(save(&mut conn, &p, "d.md", "# One\nfirst text"), SaveOutcome::Added);
    assert_eq!(save(&mut conn, &p, "d.md", "# One\nfirst text"), SaveOutcome::Unchanged);
    assert_eq!(save(&mut conn, &p, "d.md", "# One\nsecond text"), SaveOutcome::Updated);
    let s = Scope::single(p.id);
    assert!(kw(&conn, &s, "first").is_empty());
    assert_eq!(kw(&conn, &s, "second"), vec!["d.md"]);

    assert!(store::forget(&mut conn, p.id, "d.md", false).unwrap());
    assert!(kw(&conn, &s, "second").is_empty());
    assert_eq!(store::verify_clean(&conn).unwrap(), 0);
    // Re-saving a soft-deleted note revives it.
    assert_eq!(save(&mut conn, &p, "d.md", "# One\nsecond text"), SaveOutcome::Updated);
    assert_eq!(kw(&conn, &s, "second"), vec!["d.md"]);
    assert!(store::forget(&mut conn, p.id, "d.md", true).unwrap());
    let n: i64 = conn.query_row("SELECT count(*) FROM notes", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    assert!(!store::forget(&mut conn, p.id, "missing.md", false).unwrap());
}

#[test]
fn secrets_are_refused_unless_forced() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    let r = store::save_note(&mut conn, &p, ".env", "X=1", &SaveOptions { source: "user", force: false });
    assert!(r.is_err());
    let r = store::save_note(&mut conn, &p, "k.md", "-----BEGIN PRIVATE KEY-----", &SaveOptions { source: "user", force: false });
    assert!(r.is_err());
    let r = store::save_note(&mut conn, &p, "k.md", "-----BEGIN PRIVATE KEY-----", &SaveOptions { source: "user", force: true });
    assert!(r.is_ok());
}

#[test]
fn fusion_keeps_vectors_for_unchanged_chunks_and_traces_channels() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "a.md", "# Alpha\nthe rotation bug\n## Beta\nunrelated section");
    let space = embed::ensure_space(&conn, &HashEmbedder::space_def(64)).unwrap();
    let mut e = HashEmbedder { dims: 64 };
    assert_eq!(store::embed_missing(&mut conn, &space, &mut e, None).unwrap(), 2);
    // Edit only the second section: one vector should survive.
    save(&mut conn, &p, "a.md", "# Alpha\nthe rotation bug\n## Beta\nchanged section");
    let kept: i64 = conn.query_row("SELECT count(*) FROM vectors", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, 1);
    assert_eq!(store::embed_missing(&mut conn, &space, &mut e, None).unwrap(), 1);

    let s = Scope::single(p.id);
    let (hits, ex) = search(&conn, &s, "rotation bug", &SearchOptions { k: 5, mode: Mode::Auto, working: Vec::new() }, ModelSource::Warm(&mut e)).unwrap();
    assert!(ex.model_used, "warm model must always fuse");
    assert_eq!(hits[0].heading, "Alpha");
    assert_eq!(hits[0].trace.bm25_rank, Some(1));
    assert_eq!(hits[0].trace.vec_rank, Some(1));
    // Second search hits the query cache.
    let (_, ex2) = search(&conn, &s, "rotation bug", &SearchOptions { k: 5, mode: Mode::Auto, working: Vec::new() }, ModelSource::Warm(&mut e)).unwrap();
    assert!(ex2.query_cached);
}

#[test]
fn cold_model_skipped_on_exact_identifier_hit() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "a.md", "# Bugs\nB-42 crashed on export");
    embed::ensure_space(&conn, &HashEmbedder::space_def(64)).unwrap();
    let s = Scope::single(p.id);
    let mut opened = 0;
    let mut open = || -> memorycastle::Result<Box<dyn embed::Embedder>> {
        opened += 1;
        Ok(Box::new(HashEmbedder { dims: 64 }))
    };
    let (hits, ex) = search(&conn, &s, "B-42", &SearchOptions { k: 5, mode: Mode::Auto, working: Vec::new() }, ModelSource::Lazy(&mut open)).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(!ex.model_used);
    let (_, ex) = search(&conn, &s, "crash on export", &SearchOptions { k: 5, mode: Mode::Auto, working: Vec::new() }, ModelSource::Lazy(&mut open)).unwrap();
    assert!(ex.model_used);
    assert_eq!(opened, 1);
}

#[test]
fn relink_moves_memory_to_the_new_id() {
    let mut conn = db::open_memory().unwrap();
    let dir = std::env::temp_dir().join(format!("mc-relink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/config"), "[remote \"origin\"]\n\turl = git@github.com:me/old.git\n").unwrap();
    let repo = Repo::discover(&dir).unwrap();
    let p = project::ensure(&conn, &repo).unwrap();
    save(&mut conn, &p, "n.md", "# N\nremembered");
    std::fs::write(dir.join(".git/config"), "[remote \"origin\"]\n\turl = https://github.com/me/new\n").unwrap();
    let repo2 = Repo::discover(&dir).unwrap();
    assert!(project::ensure(&conn, &repo2).is_err(), "a changed remote must not silently fork");
    let (old, new) = project::relink(&mut conn, &repo2).unwrap().unwrap();
    assert_eq!(old, p.id);
    assert_eq!(new, repo2.id());
    let p2 = project::ensure(&conn, &repo2).unwrap();
    assert_eq!(kw(&conn, &Scope::single(p2.id), "remembered"), vec!["n.md"]);
    let stale: i64 = conn.query_row("SELECT count(*) FROM links WHERE project_id = ?1", [old], |r| r.get(0)).unwrap();
    assert_eq!(stale, 0, "links follow the project id");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn worktree_resolves_to_main_repo() {
    let dir = std::env::temp_dir().join(format!("mc-wt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let main = dir.join("main");
    std::fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
    std::fs::write(main.join(".git/config"), "[core]\n[remote \"origin\"]\n\turl = https://github.com/me/x\n").unwrap();
    std::fs::write(main.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();
    let wt = dir.join("wt");
    std::fs::create_dir_all(wt.join("sub")).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}\n", main.join(".git/worktrees/wt").display())).unwrap();
    let r = Repo::discover(&wt.join("sub")).unwrap();
    assert_eq!(r.root, wt);
    assert_eq!(r.remote.as_deref(), Some("github.com/me/x"));
    assert_eq!(std::fs::canonicalize(&r.main).unwrap(), std::fs::canonicalize(&main).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn newer_note_supersedes_older_in_either_save_order() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    let old = "# Batching\nFiles: src/store.rs\n## Decision\nEmbed chunks in batches of eight for throughput.";
    let new = "# One chunk per call\nFiles: src/store.rs\nSupersedes: journal/2026-10-01-batching.md\n## Decision\nEmbed chunks one at a time; batches of eight gave no throughput.";
    let s = Scope::single(p.id);
    let q = |conn: &Connection| search(conn, &s, "batches of eight throughput", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap().0;
    // Old first, then new.
    save(&mut conn, &p, "journal/2026-10-01-batching.md", old);
    save(&mut conn, &p, "journal/2026-10-18-one-chunk.md", new);
    let hits = q(&conn);
    assert_eq!(hits[0].path, "journal/2026-10-18-one-chunk.md");
    assert_eq!(hits[1].superseded_by.as_deref(), Some("journal/2026-10-18-one-chunk.md"));
    assert!(hits[1].trace.weight < 0.5);
    // New first, then old: same result.
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "journal/2026-10-18-one-chunk.md", new);
    save(&mut conn, &p, "journal/2026-10-01-batching.md", old);
    let hits = q(&conn);
    assert_eq!(hits[0].path, "journal/2026-10-18-one-chunk.md");
    assert_eq!(hits[1].superseded_by.as_deref(), Some("journal/2026-10-18-one-chunk.md"));
    // Re-saving the old note never clears the mark; forgetting the new note does.
    save(&mut conn, &p, "journal/2026-10-01-batching.md", "# Batching\nFiles: src/store.rs\n## Decision\nEmbed chunks in batches of eight, revised.");
    assert_eq!(q(&conn)[1].superseded_by.as_deref(), Some("journal/2026-10-18-one-chunk.md"));
}

#[test]
fn author_becomes_source_and_export_restores_files() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    let text = "# N\nAuthor: abhi+claude | Date: 2026-10-18\nFiles: src/x.rs\n## Decision\nremembered";
    save(&mut conn, &p, "journal/2026-10-18-n.md", text);
    let s = Scope::single(p.id);
    let (hits, _) = search(&conn, &s, "remembered", &SearchOptions { k: 1, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert_eq!(hits[0].source, "abhi+claude");
    let dir = std::env::temp_dir().join(format!("mc-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(store::export(&conn, p.id, &dir).unwrap(), 1);
    assert_eq!(std::fs::read_to_string(dir.join("journal/2026-10-18-n.md")).unwrap(), text);
    assert_eq!(store::export(&conn, p.id, &dir).unwrap(), 0, "unchanged files are skipped");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_notes_naming_a_call_neighbour_rank_first() {
    use memorycastle::graph::{self, Kind};
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "memorycastle/journal/2026-10-01-a.md", "# A\nFiles: src/store.rs\n## Decision\nKeep `chunk_key` short.");
    save(&mut conn, &p, "memorycastle/journal/2026-10-02-b.md", "# B\nFiles: src/store.rs\n## Decision\nNewest, about `status`.");
    let mut notes = graph::notes_for(&conn, p.id, Kind::File, "src/store.rs").unwrap();
    let hits = graph::rank_by_neighbours(&conn, p.id, &mut notes, &["chunk_key".to_string()]).unwrap();
    assert_eq!(hits, 1);
    assert_eq!(notes[0].path, "memorycastle/journal/2026-10-01-a.md");
    assert_eq!(notes[0].via, "chunk_key", "the line says which neighbour");
    assert_eq!(graph::rank_by_neighbours(&conn, p.id, &mut notes, &[]).unwrap(), 0);
}

#[test]
fn links_history_and_graph_from_declared_lines() {
    use memorycastle::graph::{self, Kind};
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "memorycastle/journal/2026-10-01-a.md", "# A\nFiles: src/store.rs\n## Decision\nCall `embed_missing` once.");
    save(&mut conn, &p, "memorycastle/journal/2026-10-02-b.md", "# B\nFiles: src/store.rs, src/chunk.rs\nSupersedes: 2026-10-01-a.md\n## Decision\nSplit first, then `embed_missing`.");
    save(&mut conn, &p, "memorycastle/journal/2026-10-03-c.md", "# C\nFiles: src/search.rs\n## Decision\nUnrelated; see 2026-10-02-b.md for context.");
    // links by file, suffix form, symbol; superseded last
    let by_file = graph::notes_for(&conn, p.id, Kind::File, "src/store.rs").unwrap();
    assert_eq!(by_file.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(), ["memorycastle/journal/2026-10-02-b.md", "memorycastle/journal/2026-10-01-a.md"]);
    assert_eq!(graph::notes_for(&conn, p.id, Kind::File, "store.rs").unwrap().len(), 2);
    assert_eq!(graph::notes_for(&conn, p.id, Kind::Symbol, "embed_missing").unwrap().len(), 2);
    // history walks both ways from either end
    let h = graph::history(&conn, p.id, "memorycastle/journal/2026-10-01-a.md").unwrap();
    assert_eq!(h.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(), ["memorycastle/journal/2026-10-01-a.md", "memorycastle/journal/2026-10-02-b.md"]);
    let h2 = graph::history(&conn, p.id, "memorycastle/journal/2026-10-02-b.md").unwrap();
    assert_eq!(h2.len(), 2);
    // search hits carry files
    let s = Scope::single(p.id);
    let (hits, _) = search(&conn, &s, "embed_missing", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(hits.iter().all(|h| h.files.contains(&"src/store.rs".to_string())));
    // graph: 3 notes, 3 files, 1 symbol; mention edge from c to b; two communities (c is alone)
    let root = std::env::temp_dir().join(format!("mc-root-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/store.rs"), "use a;\n\npub fn embed_missing() {}\n").unwrap();
    std::fs::write(root.join("src/chunk.rs"), "").unwrap();
    let g = graph::build(&conn, p.id, Some(&root)).unwrap();
    assert_eq!(g.nodes.iter().filter(|n| n.kind == "note").count(), 3);
    let sym = g.nodes.iter().find(|n| n.kind == "symbol").unwrap();
    assert_eq!(sym.loc, "src/store.rs:3");
    assert!(sym.exists);
    assert!(g.edges.iter().any(|e| e.kind == "in"));
    let missing = g.nodes.iter().find(|n| n.key == "src/search.rs").unwrap();
    assert!(!missing.exists, "src/search.rs is not in the checkout");
    let b = g.nodes.iter().find(|n| n.key == "memorycastle/journal/2026-10-02-b.md").unwrap();
    assert_eq!(b.summary, "Split first, then `embed_missing`");
    assert_eq!(b.sections, graph::SECTION_DECISION);
    assert_eq!(g.nodes.iter().filter(|n| n.kind == "file").count(), 3);
    assert_eq!(g.nodes.iter().filter(|n| n.kind == "symbol").count(), 1);
    assert!(g.edges.iter().any(|e| e.kind == "note"));
    let dir = std::env::temp_dir().join(format!("mc-graph-{}", std::process::id()));
    let bytes = graph::write(&g, &dir, false).unwrap();
    assert!(bytes > 1000);
    assert!(std::fs::read_to_string(dir.join("graph.html")).unwrap().contains("\"nodes\":["));
    assert_eq!(std::fs::read_to_string(dir.join("edges.tsv")).unwrap().lines().count(), 1 + g.edges.len());
    let _ = std::fs::remove_dir_all(&dir);
    // dangling: src/search.rs does not exist under the root
    let d = graph::dangling_files(&conn, p.id, &root).unwrap();
    assert_eq!(d, vec![("src/search.rs".to_string(), 1)]);
    let _ = std::fs::remove_dir_all(&root);
    // forget cascades links
    store::forget(&mut conn, p.id, "memorycastle/journal/2026-10-03-c.md", true).unwrap();
    assert_eq!(graph::notes_for(&conn, p.id, Kind::File, "src/search.rs").unwrap().len(), 0);
}

#[test]
fn bit_prefilter_matches_exact_scan() {
    use memorycastle::search::Explain;
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    let def = HashEmbedder::space_def(64);
    let space = embed::ensure_space(&conn, &def).unwrap();
    let mut e = HashEmbedder { dims: def.dims };
    for i in 0..120 {
        let body: String = (0..6).map(|j| format!("## S{j}\nsentence about topic {} and item {} with words {}.\n", i % 13, i * 7 % 31, (i + j) % 17)).collect();
        save(&mut conn, &p, &format!("memorycastle/journal/2026-01-{:02}-n{i}.md", i % 28 + 1), &format!("# N{i}\nFiles: src/a.rs\n{body}"));
    }
    store::embed_missing(&mut conn, &space, &mut e, Some(p.id)).unwrap();
    let n: i64 = conn.query_row("SELECT count(*) FROM vectors", [], |r| r.get(0)).unwrap();
    assert!(n >= 512, "need enough vectors to engage the prefilter, got {n}");
    let s = Scope::single(p.id);
    let run = |conn: &Connection, e: &mut HashEmbedder, q: &str| -> (Vec<i64>, Explain) {
        let (hits, ex) = search(conn, &s, q, &SearchOptions { k: 10, mode: Mode::Meaning, working: Vec::new() }, ModelSource::Warm(e)).unwrap();
        (hits.into_iter().map(|h| h.chunk_id).collect(), ex)
    };
    let mut agree = 0;
    let queries = ["topic 3 item 5", "words 11 sentence", "item 20 topic 9", "sentence about topic 12", "item 2 words 4"];
    for q in queries {
        std::env::set_var("MEMORYCASTLE_NO_BITS", "1");
        let (exact, ex_exact) = run(&conn, &mut e, q);
        std::env::remove_var("MEMORYCASTLE_NO_BITS");
        conn.execute("DELETE FROM query_cache", []).unwrap();
        let (fast, ex_fast) = run(&conn, &mut e, q);
        conn.execute("DELETE FROM query_cache", []).unwrap();
        assert_eq!(ex_exact.prefiltered, 0);
        assert_eq!(ex_fast.prefiltered as i64, n);
        agree += exact.iter().take(5).filter(|id| fast.contains(id)).count();
    }
    assert!(agree >= 20, "top-5 overlap across 5 queries was {agree}/25");
    // Saving a note drops the pack; the next search rebuilds it.
    save(&mut conn, &p, "memorycastle/journal/2026-02-01-x.md", "# X\nFiles: src/a.rs\n## S\nnew text");
    let packs: i64 = conn.query_row("SELECT count(*) FROM vector_bits", [], |r| r.get(0)).unwrap();
    assert_eq!(packs, 0);
}

#[test]
fn file_links_match_bare_partial_and_full_paths_and_say_the_name_used() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "memorycastle/journal/a.md", "# A\nFiles: StatusLayers.kt\n## Decision\nbare name.\n");
    save(&mut conn, &p, "memorycastle/journal/b.md", "# B\nFiles: apps/ui/status/StatusLayers.kt\n## Decision\nfull path.\n");
    for q in ["StatusLayers.kt", "status/StatusLayers.kt", "apps/ui/status/StatusLayers.kt"] {
        let notes = graph::notes_for(&conn, p.id, graph::Kind::File, q).unwrap();
        let mut paths: Vec<&str> = notes.iter().map(|n| n.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, ["memorycastle/journal/a.md", "memorycastle/journal/b.md"], "query {q}");
    }
    let notes = graph::notes_for(&conn, p.id, graph::Kind::File, "apps/ui/status/StatusLayers.kt").unwrap();
    let a = notes.iter().find(|n| n.path.ends_with("a.md")).unwrap();
    assert_eq!(a.via, "StatusLayers.kt");
}

#[test]
fn off_topic_question_returns_nothing_and_says_why() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "memorycastle/journal/a.md", "# Retention\nFiles: x.rs\n## Decision\nThe server keeps a message until every device acknowledged it.\n");
    let scope = Scope::single(p.id);
    let (hits, ex) = search(&conn, &scope, "how do we bake the sourdough bread", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(hits.is_empty(), "{hits:?}");
    assert!(ex.dropped > 0, "the weak keyword hit was dropped, not absent: {ex:?}");
    assert!(ex.best_confidence < memorycastle::limits::MIN_CONFIDENCE);
    // A real question keeps its hit, with a line number and a confidence.
    let (hits, _) = search(&conn, &scope, "server keeps a message until acknowledged", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].line, 4);
    assert!(hits[0].trace.confidence >= 0.5);
}

#[test]
fn number_questions_prefer_the_measured_section_and_duplicates_collapse() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    let fact = "Frosted shapes cost 18 MB at peak, inside the 24 MB budget for the editor screen.";
    save(&mut conn, &p, "memorycastle/journal/2026-10-18-frosted.md", &format!("# Frosted\nFiles: x.kt\n## Decision\nFrosted shapes stay; memory fits the budget.\n## Measured\n{fact}\n"));
    save(&mut conn, &p, "memorycastle/journal/app-bugs.md", &format!("# Bugs\nFiles: x.kt\n## Low\nB-7 {fact}\n"));
    let scope = Scope::single(p.id);
    let (hits, _) = search(&conn, &scope, "did frosted shapes fit the memory budget", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(hits[0].heading.ends_with("Measured"), "{:?}", hits.iter().map(|h| &h.heading).collect::<Vec<_>>());
    // The same sentence in the tracker and the dated note appears once, from the dated note.
    let same: Vec<&Hit> = hits.iter().filter(|h| h.text.contains("18 MB")).collect();
    assert_eq!(same.len(), 1, "{:?}", hits.iter().map(|h| (&h.path, &h.heading)).collect::<Vec<_>>());
    assert!(same[0].path.contains("2026-10-18-frosted"));
    assert_eq!(memorycastle::search::section_cue("why did we choose A"), Some("decision"));
    assert_eq!(memorycastle::search::section_cue("what was tried instead of B"), Some("rejected"));
    assert_eq!(memorycastle::search::section_cue("status editor handoff"), None);
}

#[test]
fn graph_rerank_lifts_the_note_that_links_the_named_file_or_symbol() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    // Both notes share the words; only one declares the file the question names.
    save(&mut conn, &p, "memorycastle/journal/2026-10-01-a.md", "# Crop handling in the editor\nFiles: src/other.rs\n## Decision\nCrop handling uses the editor state and the crop rect math.\n");
    save(&mut conn, &p, "memorycastle/journal/2026-10-02-b.md", "# Crop rect\nFiles: src/crop.rs\n## Decision\nCrop handling uses `CropRect` in `src/crop.rs` for the editor.\n");
    let scope = Scope::single(p.id);
    let (hits, _) = search(&conn, &scope, "crop handling editor src/crop.rs", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(hits[0].path.ends_with("b.md"), "{:?}", hits.iter().map(|h| (&h.path, h.trace.graph)).collect::<Vec<_>>());
    assert_eq!(hits[0].trace.graph, memorycastle::limits::GRAPH_DIRECT_WEIGHT);
    let (hits, _) = search(&conn, &scope, "crop handling CropRect", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(hits[0].path.ends_with("b.md"));
    assert_eq!(hits[0].trace.graph, memorycastle::limits::GRAPH_DIRECT_WEIGHT);
    // A question with no file or symbol leaves weights alone.
    let (hits, _) = search(&conn, &scope, "crop handling editor", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(hits.iter().all(|h| h.trace.graph == 1.0));
}

#[test]
fn owed_follows_edits_after_the_last_note() {
    use memorycastle::draft;
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "o", None);
    let root = std::env::temp_dir().join(format!("mc-owed-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
    assert!(draft::owed(&conn, p.id).unwrap().is_none(), "no session declared: nothing owed");
    store::set_session(&conn, p.id, "s1", Some("claude")).unwrap();
    assert!(draft::owed(&conn, p.id).unwrap().is_none(), "no edits yet");
    store::record_edit(&conn, p.id, &root, "src/a.rs").unwrap();
    store::record_edit(&conn, p.id, &root, "memorycastle/journal/x.md").unwrap();
    let o = draft::owed(&conn, p.id).unwrap().expect("an edit outside the journal is owed");
    assert_eq!(o.files, vec!["src/a.rs".to_string()]);
    assert!(o.last_note.is_none());
    // Saving a note settles it; the note's saved_at must be after the edit.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    save(&mut conn, &p, "memorycastle/journal/2026-10-19-a.md", "# Chose a\nDate: 2026-10-19\nFiles: `src/a.rs`\n\n## Decision\nbecause");
    assert!(draft::owed(&conn, p.id).unwrap().is_none(), "settled by the note: {:?}", draft::owed(&conn, p.id).unwrap());
    // A shell edit the after-command hook reports once the command is done,
    // stamped with the file's mtime from before the save: not owed.
    let saved_at = memorycastle::sys::now();
    store::record_edit_at(&conn, p.id, &root, "src/a.rs", saved_at - 1).unwrap();
    store::record_edit_at(&conn, p.id, &root, "src/a.rs", saved_at).unwrap();
    assert!(draft::owed(&conn, p.id).unwrap().is_none(), "an edit written before the save is the note's");
    // A later edit is owed again, and names the note it follows.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    store::record_edit(&conn, p.id, &root, "src/a.rs").unwrap();
    let o = draft::owed(&conn, p.id).unwrap().expect("edit after the note is owed");
    assert_eq!(o.last_note.as_ref().map(|n| n.0.as_str()), Some("memorycastle/journal/2026-10-19-a.md"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn brief_prints_the_hit_line_and_the_decision_sentence() {
    use memorycastle::render;
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "b", None);
    save(&mut conn, &p, "memorycastle/journal/2026-10-19-brief.md", "# Pick the brief default\nAuthor: abhi+claude | Date: 2026-10-19\nFiles: `src/render.rs`\n\n## Context\nAgents skim titles.\n\n## Decision\nBrief is the default output. The sections come on request.\n");
    let scope = Scope::single(p.id);
    let (hits, _) = search(&conn, &scope, "brief default", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(!hits.is_empty());
    let r = render::search(&conn, &hits, &[], &render::Options { query: "brief default", sections: false, brief: true, explain: false, abs: false, repo_root: None }).unwrap();
    let lines: Vec<&str> = r.text.lines().collect();
    assert!(lines[0].starts_with("1. memorycastle/journal/2026-10-19-brief.md:"), "{}", r.text);
    assert!(lines[1].starts_with("   decision") && lines[1].contains("Brief is the default output."), "{}", r.text);
    assert!(!lines[1].contains("The sections come"), "one sentence only: {}", r.text);
    assert!(!r.text.contains("Pick the brief default"), "no title line in brief: {}", r.text);
    assert!(r.text.ends_with("read first: none\n"), "{}", r.text);
    assert_eq!(r.mode, "brief");
}

#[test]
fn render_follows_the_fixed_grammar() {
    use memorycastle::render::{self, VALUE_COL};
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "g", None);
    save(&mut conn, &p, "memorycastle/journal/2026-10-19-grammar.md", "# Choose the label grammar\nAuthor: abhi+claude | Date: 2026-10-19\nFiles: `src/render.rs`\n\n## Decision\n1. a line that starts like a hit\n   files      a line that starts like a label\nAnd a very long sentence that keeps going well past the ninety-two column wrap limit so that the renderer has to break it at a space somewhere sensible.\n\n## Rejected\nXML tags: tokens.\n");
    save(&mut conn, &p, "memorycastle/journal/2026-10-19-other.md", "# Another note\nDate: 2026-10-19\nFiles: `src/x.rs`\n\n## Decision\nlabel grammar also mentioned here\n");
    let scope = Scope::single(p.id);
    let (hits, _) = search(&conn, &scope, "label grammar", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(hits.len() >= 2);
    for sections in [false, true] {
        let r = render::search(&conn, &hits, &[], &render::Options { query: "label grammar", sections, brief: false, explain: false, abs: false, repo_root: None }).unwrap();
        let lines: Vec<&str> = r.text.lines().collect();
        let mut hit_lines = 0;
        for (i, l) in lines.iter().enumerate() {
            if l.is_empty() || l.starts_with("read first") {
                continue;
            }
            let is_hit = l.len() > 3 && l.as_bytes()[0].is_ascii_digit() && l.contains(". ");
            let is_cont = l.starts_with(&" ".repeat(VALUE_COL));
            let is_label = !is_cont && l.starts_with("   ") && !l[3..].starts_with(' ');
            if is_hit {
                hit_lines += 1;
                let (num, rest) = l.split_once(". ").unwrap();
                assert!(num.chars().all(|c| c.is_ascii_digit()), "{l}");
                let fields: Vec<&str> = rest.split("   ").collect();
                assert!(fields.len() >= 3, "hit line has path, confidence, section: {l}");
                assert!(fields[0].contains(".md:"), "{l}");
                assert!(fields[1].parse::<f64>().is_ok(), "confidence is a number: {l}");
            } else {
                assert!(is_cont || is_label, "line {i} is neither label nor continuation: {l:?}");
                assert!(l.chars().count() <= VALUE_COL + render::WRAP_COLS + 8, "wrapped: {l}");
            }
        }
        assert_eq!(hit_lines, 2, "{}", r.text);
        // A dated note's hit line carries no date: the file name has it. (A new note's
        // `dated` column was once left NULL on insert; this is the regression test.)
        assert!(!lines[0].ends_with("2026-10-19"), "dated note printed its date: {}", lines[0]);
        assert!(r.text.contains("read first: none"));
        // The note's own lines that look like a hit or a label never reach column 0 or the label column.
        assert!(!lines.iter().any(|l| l.starts_with("1. a line")), "{}", r.text);
        if sections {
            assert!(r.text.contains(&format!("{}   files      a line", " ".repeat(VALUE_COL))), "{}", r.text);
        }
    }
}

#[test]
fn a_note_knows_when_its_code_changed() {
    use memorycastle::code;
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "cc", None);
    let root = std::env::temp_dir().join(format!("mc-cc-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn alpha_one() -> i32 {\n    1\n}\n\npub fn beta_two() -> i32 {\n    2\n}\n").unwrap();
    code::index(&mut conn, p.id, &root).unwrap();
    save(&mut conn, &p, "memorycastle/journal/2026-10-19-alpha.md", "# alpha returns one\nDate: 2026-10-19\nFiles: `src/a.rs`\n\n## Decision\n`alpha_one` returns one because beta needs it.");
    save(&mut conn, &p, "memorycastle/journal/2026-10-19-beta.md", "# beta returns two\nDate: 2026-10-19\nFiles: `src/a.rs`\n\n## Decision\n`beta_two` returns two because alpha needs it.");
    assert_eq!(graph::refresh_note_code(&conn, p.id, &root, "memorycastle/journal/2026-10-19-alpha.md").unwrap(), 1);
    assert_eq!(graph::refresh_note_code(&conn, p.id, &root, "memorycastle/journal/2026-10-19-beta.md").unwrap(), 1);
    let scope = Scope::single(p.id);
    let (mut hits, _) = search(&conn, &scope, "why does alpha return one and beta two", &SearchOptions { k: 10, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    memorycastle::search::mark_code_changed(&conn, &root, &mut hits).unwrap();
    assert!(hits.iter().all(|h| h.code_changed.is_empty()), "nothing changed yet");
    // Edit beta's body: the beta note is labelled and ranks under alpha's.
    std::fs::write(root.join("src/a.rs"), "pub fn alpha_one() -> i32 {\n    1\n}\n\npub fn beta_two() -> i32 {\n    3\n}\n").unwrap();
    code::refresh(&mut conn, p.id, &root).unwrap();
    let (mut hits, _) = search(&conn, &scope, "why does alpha return one and beta two", &SearchOptions { k: 10, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    memorycastle::search::mark_code_changed(&conn, &root, &mut hits).unwrap();
    let beta = hits.iter().find(|h| h.path.contains("beta")).unwrap();
    let alpha = hits.iter().find(|h| h.path.contains("alpha")).unwrap();
    assert_eq!(beta.code_changed, vec!["beta_two".to_string()]);
    assert!(alpha.code_changed.is_empty(), "alpha's body is as it was");
    assert!(alpha.trace.score > beta.trace.score, "the note whose code changed ranks under the one whose code did not");
    // The enclosing definition of a line, for the edit hook.
    assert_eq!(code::def_enclosing(&conn, p.id, "src/a.rs", 6).unwrap().map(|d| d.name), Some("beta_two".to_string()));
    assert!(code::def_enclosing(&conn, p.id, "src/a.rs", 4).unwrap().is_none(), "a blank line between definitions is in none");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn recall_asks_nothing_of_a_short_prompt() {
    use memorycastle::limits::RECALL_MIN_WORDS;
    use memorycastle::search::content_words;
    assert!(content_words("yes").len() < RECALL_MIN_WORDS);
    assert!(content_words("run the tests again").len() < RECALL_MIN_WORDS, "{:?}", content_words("run the tests again"));
    assert!(content_words("make embedding faster by batching chunks").len() >= RECALL_MIN_WORDS);
}

#[test]
fn one_embedding_server_owns_the_socket() {
    // The socket is named by the data dir, so point this test at its own.
    let dir = std::env::temp_dir().join(format!("mc-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("MEMORYCASTLE_HOME", &dir);
    let path = memorycastle::serve::socket_path();
    let _ = std::fs::remove_file(&path);
    let first = memorycastle::serve::claim().unwrap().expect("first server claims the socket");
    assert!(memorycastle::serve::claim().unwrap().is_none(), "a second server must stand down while the first answers");
    drop(first);
    // The file is still there but nobody answers: stale, replaced.
    assert!(path.exists());
    let third = memorycastle::serve::claim().unwrap();
    assert!(third.is_some(), "a stale socket file is replaced");
    drop(third);
    let _ = std::fs::remove_file(&path);
    std::env::remove_var("MEMORYCASTLE_HOME");
}

#[test]
fn recall_prints_the_decision_sentence() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "memorycastle/journal/a.md", "# Serial by default\nFiles: x.rs\n## Context\nwhy\n## Decision\n- Serial is the default. The pipelined writer stays behind --threads.\n## Measured\n27 ms.\n");
    let id: i64 = conn.query_row("SELECT id FROM notes WHERE path = 'memorycastle/journal/a.md'", [], |r| r.get(0)).unwrap();
    assert_eq!(store::decision_sentence(&conn, id).unwrap().as_deref(), Some("Serial is the default."));
    save(&mut conn, &p, "memorycastle/journal/b.md", "# No decision\nFiles: x.rs\n## Measured\n1 ms.\n");
    let id: i64 = conn.query_row("SELECT id FROM notes WHERE path = 'memorycastle/journal/b.md'", [], |r| r.get(0)).unwrap();
    assert_eq!(store::decision_sentence(&conn, id).unwrap(), None);
    // A long sentence is cut at a word, never inside one.
    let long = format!("## Decision\n{} tailword end.\n", "alpha ".repeat(40));
    save(&mut conn, &p, "memorycastle/journal/c.md", &format!("# Long\nFiles: x.rs\n{long}"));
    let id: i64 = conn.query_row("SELECT id FROM notes WHERE path = 'memorycastle/journal/c.md'", [], |r| r.get(0)).unwrap();
    let s = store::decision_sentence(&conn, id).unwrap().unwrap();
    assert!(s.ends_with("alpha ...") && s.len() <= memorycastle::limits::RECALL_DECISION_CHARS + 4, "{s}");
}

#[test]
fn reading_a_served_note_counts_as_opened() {
    use memorycastle::quality;
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "l", None);
    save(&mut conn, &p, "memorycastle/journal/2026-10-20-a.md", "# Chose a\n\n## Decision\nbecause");
    let id: i64 = conn.query_row("SELECT id FROM notes WHERE project_id = ?1", [p.id], |r| r.get(0)).unwrap();
    let ins = |at: i64, mode: &str| {
        conn.execute("INSERT INTO served_notes(at, project_id, note_id, mode) VALUES (?1, ?2, ?3, ?4)", rusqlite::params![at, p.id, id, mode]).unwrap();
    };
    // Served at 1000 and read 10 s later: opened. Served at 5000, never read.
    ins(1000, "brief");
    ins(1010, "read");
    ins(5000, "recall");
    assert_eq!(quality::label_totals(&conn, p.id).unwrap(), (2, 1), "the read row is an open, not a serve");
}

#[test]
fn a_common_identifier_proves_nothing_a_rare_one_does() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "ag", None);
    let topics = ["login tokens", "push wakes", "group calls", "media upload", "status editor", "backup restore", "contact lists", "relay hosts", "crash reports", "theme colours", "keyboard input", "device linking"];
    for (i, t) in topics.iter().enumerate() {
        save(&mut conn, &p, &format!("memorycastle/journal/n{i}.md"), &format!("# AbhiGram {t}\n\n## Decision\nAbhiGram handles {t} on the server side for now."));
    }
    save(&mut conn, &p, "memorycastle/journal/blur.md", "# Frosted shapes\n\n## Decision\nAbhiGram blurs once in `blur_rgba` with three box passes.");
    let scope = Scope::single(p.id);
    let q = |s: &str| search(&conn, &scope, s, &SearchOptions { k: 10, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap().0;
    // The project's name is in every note: it is vocabulary, not evidence.
    assert!(q("how does AbhiGram pick theme colours").iter().all(|h| h.trace.confidence < 1.0), "a name in every note proved a hit");
    // A rare identifier found verbatim still settles it.
    let hits = q("where is blur_rgba called");
    assert_eq!(hits[0].path, "memorycastle/journal/blur.md");
    assert_eq!(hits[0].trace.confidence, 1.0);
}

#[test]
fn an_unfilled_draft_is_not_a_note() {
    use memorycastle::draft;
    let skeleton = format!("# {}\nAuthor: a | Date: 2026-10-20\n\n## Decision\n<what was chosen and why>\n", draft::PLACEHOLDER_TITLE);
    assert!(draft::is_unfilled(&skeleton));
    assert!(!draft::is_unfilled("# Chose one chunk per call\n\n## Decision\nbecause"));
}

#[test]
fn a_resume_keeps_the_note_saved_before_it() {
    use memorycastle::draft;
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "r", None);
    let root = std::env::temp_dir().join(format!("mc-resume-{}", std::process::id()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    for f in ["a", "b", "c"] {
        std::fs::write(root.join(format!("src/{f}.rs")), "fn x() {}\n").unwrap();
    }
    store::set_session(&conn, p.id, "s1", Some("claude")).unwrap();
    store::record_edit(&conn, p.id, &root, "src/a.rs").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    save(&mut conn, &p, "memorycastle/journal/2026-10-20-a.md", "# Chose a\nDate: 2026-10-20\nFiles: `src/a.rs`\n\n## Decision\nbecause");
    // A restart declares a fresh id, then the resumed one: the declared
    // start moves past the note, the session's own rows do not.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    store::set_session(&conn, p.id, "fresh", Some("claude")).unwrap();
    store::set_session(&conn, p.id, "s1", Some("claude")).unwrap();
    store::record_edit(&conn, p.id, &root, "src/b.rs").unwrap();
    let o = draft::owed(&conn, p.id).unwrap().expect("the edit after the note is owed");
    assert_eq!(o.last_note.as_ref().map(|n| n.0.as_str()), Some("memorycastle/journal/2026-10-20-a.md"), "the note before the resume still counts");
    assert_eq!(o.files, vec!["src/b.rs".to_string()]);
    // The next note is told only about what followed the previous one.
    store::record_edit(&conn, p.id, &root, "src/c.rs").unwrap();
    let missing = store::files_missing_from_note(&conn, p.id, "memorycastle/journal/2026-10-20-b.md", "# Chose b\nFiles: `src/b.rs`\n").unwrap();
    assert_eq!(missing, vec!["src/c.rs".to_string()], "src/a.rs belongs to the earlier note");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hits_carry_line_ranges_and_the_notes_other_parts() {
    use memorycastle::render;
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    let note = "# Stemming\nDate: 2026-10-20\nFiles: `src/fts.rs`\n\n## Decision\nA plain word also matches by its stem as a prefix.\nExact matches still score.\n\n## Rejected\nThe porter tokenizer: it stems too far.\n\n## Measured\nMRR 0.911 to 0.866 with porter.\n";
    save(&mut conn, &p, "memorycastle/journal/2026-10-20-stem.md", note);
    let scope = Scope::single(p.id);
    let (hits, _) = search(&conn, &scope, "porter tokenizer", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    let h = &hits[0];
    // Every part is a range inside the note, on the section's own lines.
    let lines: Vec<&str> = note.lines().collect();
    assert!(h.parts.len() >= 2, "{:?}", h.parts);
    for part in &h.parts {
        assert!(part.line <= part.end && (part.end as usize) <= lines.len(), "{part:?}");
        assert!(!lines[part.line as usize - 1].trim().is_empty(), "{part:?}");
    }
    assert!(h.parts.iter().any(|x| x.section == "Rejected" && x.line == 10 && x.end == 10), "{:?}", h.parts);
    let r = render::search(&conn, &hits, &[], &render::Options { query: "porter tokenizer", sections: false, brief: true, explain: false, abs: false, repo_root: None }).unwrap();
    assert!(r.text.lines().any(|l| l.starts_with("   lines      ")), "{}", r.text);
    // links: the sections that name the target, header lines left out.
    let id: i64 = conn.query_row("SELECT id FROM notes WHERE path = 'memorycastle/journal/2026-10-20-stem.md'", [], |r| r.get(0)).unwrap();
    assert_eq!(store::note_lines_naming(&conn, id, &["porter"]).unwrap(), vec![(10, 10, "Rejected".to_string()), (13, 13, "Measured".to_string())]);
    assert!(store::note_lines_naming(&conn, id, &["fts.rs"]).unwrap().is_empty(), "Files: is a header line");
    assert!(store::note_lines_naming(&conn, id, &["stem"]).unwrap().iter().all(|(_, _, s)| s == "Decision"), "stems is not stem");
}

#[test]
fn working_set_ranks_notes_about_the_files_in_hand() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "memorycastle/journal/2026-10-20-a.md", "# Tokenizer cache kept\nDate: 2026-10-20\nFiles: `src/a.rs`\n\n## Decision\nThe tokenizer cache stays on disk.\n");
    save(&mut conn, &p, "memorycastle/journal/2026-10-19-b.md", "# Tokenizer cache kept\nDate: 2026-10-19\nFiles: `src/b.rs`\n\n## Decision\nThe tokenizer cache stays on disk.\n");
    let scope = Scope::single(p.id);
    let run = |working: Vec<(String, f32)>| search(&conn, &scope, "tokenizer cache", &SearchOptions { k: 5, mode: Mode::Keyword, working }, ModelSource::None).unwrap().0;
    let plain = run(Vec::new());
    assert!(plain.iter().all(|h| h.trace.working == 1.0));
    let hits = run(vec![("src/b.rs".into(), 1.0)]);
    assert_eq!(hits[0].path, "memorycastle/journal/2026-10-19-b.md", "the note about the file in hand ranks first");
    assert!((hits[0].trace.working - memorycastle::limits::WORKING_WEIGHT).abs() < 1e-6);
    // A file the session only read weighs less, and the factor scales with it.
    let half = run(vec![("src/b.rs".into(), 0.5)]);
    let b = half.iter().find(|h| h.path.ends_with("-b.md")).unwrap();
    assert!(b.trace.working > 1.0 && b.trace.working < memorycastle::limits::WORKING_WEIGHT);
}

#[test]
fn also_merges_wordings_one_hit_per_note() {
    let mut conn = db::open_memory().unwrap();
    let p = project(&conn, "p", None);
    save(&mut conn, &p, "journal/2026-10-18-fallback.md", "# Fallback\n## Decision\nA document that loads a local style file runs whole on the engine.");
    save(&mut conn, &p, "journal/2026-10-18-port.md", "# Port\n## Decision\nEach command becomes a native primitive, ported one by one.");
    let s = Scope::single(p.id);
    let opts = SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() };
    let q = |text: &str| search(&conn, &s, text, &opts, ModelSource::None).unwrap().0;
    // The question's words reach only the port note; the other wording reaches the fallback note.
    let first = q("custom package ported natively");
    assert!(first.iter().all(|h| h.path != "journal/2026-10-18-fallback.md"));
    let merged = memorycastle::search::merge_queries(vec![first, q("local style file engine"), q("ported command")], 5);
    let paths: Vec<&str> = merged.iter().map(|h| h.path.as_str()).collect();
    assert!(paths.contains(&"journal/2026-10-18-fallback.md"));
    assert_eq!(paths.iter().filter(|p| **p == "journal/2026-10-18-port.md").count(), 1, "one hit per note: {paths:?}");
    // Found by two wordings, the port note ranks first; the fallback note came in through the first --also.
    assert_eq!(paths[0], "journal/2026-10-18-port.md");
    assert_eq!(merged.iter().find(|h| h.path.ends_with("fallback.md")).unwrap().trace.via, 1);
}
