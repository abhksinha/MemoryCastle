//! Ranked read spans over a small checkout: the question's symbol first,
//! then what the top note names, then the first use elsewhere.
use memorycastle::project::{self, Project, Scope};
use memorycastle::search::{search, Mode, ModelSource, SearchOptions};
use memorycastle::store::{self, SaveOptions};
use memorycastle::{code, db, graph, spans};

#[test]
fn question_symbol_then_note_symbols_then_uses() {
    let dir = std::env::temp_dir().join(format!("mc-spans-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.as_path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "pub fn alpha_sum() -> u32 {\n    beta_base() + 1\n}\n\npub fn beta_base() -> u32 {\n    1\n}\n\npub enum Kind {\n    Note,\n    File,\n}\n").unwrap();
    std::fs::write(root.join("src/b.rs"), "use crate::a::alpha_sum;\n\npub fn run() {\n    let x = alpha_sum();\n    let _ = x;\n}\n").unwrap();
    let mut conn = db::open_memory().unwrap();
    let id = project::project_id_of("github.com/t/spans");
    conn.execute("INSERT INTO projects(id, name, paths, grp, added_at) VALUES (?1, 'spans', '[]', NULL, 0)", [id]).unwrap();
    let p = Project { id, name: "spans".into(), paths: vec![], group: None };
    code::index(&mut conn, id, root).unwrap();
    let note = "# Why alpha_sum adds one\nAuthor: t+claude | Date: 2026-10-18\nFiles: src/a.rs\n\n## Context\nCounting starts at one.\n\n## Decision\n`alpha_sum` adds one to `beta_base` so callers never see zero, which the old code returned by mistake.\n";
    store::save_note(&mut conn, &p, "memorycastle/journal/2026-10-18-alpha_sum.md", note, &SaveOptions { source: "user", force: false }).unwrap();
    graph::ensure_links(&mut conn, id).unwrap();
    let scope = Scope::single(id);
    let (hits, _) = search(&conn, &scope, "why does alpha_sum add one", &SearchOptions { k: 5, mode: Mode::Keyword, working: Vec::new() }, ModelSource::None).unwrap();
    assert!(!hits.is_empty());
    let out = spans::ranked(&conn, id, Some(root), "why does alpha_sum add one", &hits, &[]).unwrap();
    let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(out[0].name, "alpha_sum", "{names:?}");
    assert_eq!((out[0].path.as_str(), out[0].start, out[0].end), ("src/a.rs", 1, 3));
    assert!(names.contains(&"beta_base"), "note names beta_base: {names:?}");
    let use_line = out.iter().find(|s| s.kind == "use").expect("a use span");
    assert_eq!(use_line.path, "src/b.rs");
    assert!(use_line.start <= 4 && use_line.end >= 4, "use at line 4: {use_line:?}");
    assert!(out[0].abs.as_deref().unwrap().ends_with("src/a.rs"));
    // enum variants are definitions now
    let v = code::defs_named(&conn, id, "Note").unwrap();
    assert_eq!((v[0].kind, v[0].line), ("variant", 10));
    let _ = std::fs::remove_dir_all(&dir);
}
