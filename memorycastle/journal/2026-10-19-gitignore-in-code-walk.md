# The code walker honours .gitignore
Author: abhi+claude | Date: 2026-10-19
Files: src/code.rs

## Context
Setting rushtex up for a fresh-session measurement, `graph --code` indexed
9,277 files and wrote 26 MB of graph into the repo: the checkout holds a
gitignored `texlive-source` tree of C sources, and the walker only skipped a
fixed list of build directories.

## Decision
`code::walk` reads `.gitignore` at the root and in each directory it enters
and skips what the simple patterns say: a bare name at any depth, a rooted
path, a trailing `/` for directories only, `*.ext`. Negations and other
globs are ignored. No `git` process, in keeping with `project.rs`. Code
index version 8 so every store re-walks once.

## Rejected
- Running `git ls-files`: a process per refresh, and refresh runs before
  every `links` and `search`.
- Full gitignore semantics: the vendored-tree case is what matters; a
  pattern the walker does not understand indexes a few extra files, which
  the per-file caps already bound.

## Measured
- rushtex: 9,277 files, 109k definitions, 26 MB of graph files, 12.6 s;
  after: 89 files, 3,204 definitions, 626 KB, 2.3 s.
