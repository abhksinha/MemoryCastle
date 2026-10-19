# Code graph layer under the note graph
Author: abhi+claude | Date: 2026-10-18
Files: src/code.rs, src/graph.rs, src/graph.html, src/db.rs, src/main.rs

## Context
Note-declared edges left 53 of 73 AbhiGram symbols unplaced because notes rarely name the defining file. The user asked for a graph of all files and functions, not bounded to journal outcomes.

## Decision
`graph --code` indexes the checkout by line scanning (definitions by keyword-before-name, imports resolved by file name, mentions as identifier tokens matching a definition elsewhere), keyed by content hash so re-runs touch only changed files. The graph merges note symbols onto definition nodes, areas become directories, and the HTML keeps a navigation subset (definitions used from 3 or more files, 12 mention edges per file) while the TSVs hold everything.

## Rejected
Tree-sitter or LSP: a parser per language inside a memory tool; the agent already has those for exact structure. LLM extraction: nondeterministic and a second source of truth. Putting every mention edge in the HTML: 15.8 MB and 184k edges on AbhiGram, unusable; the view trims to 2.0 MB. `sealed` and `data` as definition keywords: turned `sealed: true,` into a definition named `true`.

## Measured
AbhiGram, 974 files, 12 MB source: index 0.58 s cold, 39 ms incremental, 59 MB peak; 30,136 definitions, 1,106 imports, 40,984 mention rows after dropping names used from more than 100 files. Graph build 0.78 s, HTML 2.0 MB with 5,139 nodes, parsed in 8 ms. `links CropRect` now prints the definition at `StatusRender.kt:64`.

## Open
Enum variants (`RecordType::DeliveryWatermark`) and nested names are not definitions, so a note naming one still shows unlocated. Go imports resolve to packages, not files, and are skipped. Mention edges are name matches, not calls.
