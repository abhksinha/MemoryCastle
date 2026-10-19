# Graph signals rerank search hits
Author: abhi+claude | Date: 2026-10-18
Files: src/search.rs, src/graph.rs, src/limits.rs

## Context
The user wanted reranking improved without waiting for labelled data for a learned reranker, and suggested the graph as the signal.

## Decision
A hit's note is weighted 1.5x when its declared links hold a file or symbol the question names, 1.2x when a question symbol is defined (code index) in a file the note links. Deterministic, from declared links and the code index, so `--explain` can show the factor. A learned tree, if ever built, combines such features; it does not replace them.

## Rejected
Centrality (hub notes first): a status board with 40 file links would win every question. Boosting notes about files changed in the working tree: reading the git index is not free on the keyword path and the signal is noisy mid-task; revisit with a flag.

## Measured
Gold set 44 questions (two new ones name a file and a symbol): auto recall@3 1.000, recall@10 1.000, MRR 0.909; keyword 0.955 / 0.955 / 0.868. Before the rerank, the two new questions ranked the named note second and fourth in keyword mode.

## Open
Weights are hand set; a label log of which hit the agent used would let a tree learn them per project. Not built until the hand rules show a miss.

## Measured (correction, same day)
The eval after the commit reads auto MRR 0.902, not 0.909; keyword MRR 0.871. Recall figures stand.
