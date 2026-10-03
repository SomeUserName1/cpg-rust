# CFG and dominator tree from the Rust AST layer (task t_17450c16)

Implementation of the control-flow overlay over the CPG AST layer built by
t_75f6e158: per-function CFG construction with entry/exit nodes, branch-labelled
edges, all loop forms, break/continue/return, best-effort panic edges; plus the
dominator tree via a from-scratch Lengauer–Tarjan implementation attached in the
`dominators` edge namespace (schema `Dominate`).

## Layout

- `src/cfg.rs` — CFG data model (`CfgNodeKind::{Entry,Exit,Panic,Statement,
  Condition,Iterate}`, `Cfg` with nodes/edges) and the builder
  (`build_function_cfg`, `build_cfgs`, `attach_cfg_edges`).
- `src/dominators.rs` — Lengauer–Tarjan (`idom_map`, `dom_edges`,
  `postdom_edges` for post-dominators on the reversed graph) plus
  `attach_dominator_edges` (`AttachOptions`), which materializes the
  immediate-dominator/post-dominator trees into the shared graph as
  `EdgeKind::Dominate` / `EdgeKind::PostDominate` edges in the
  `dominators` overlay namespace, endpoints mapped CFG-flow-node → bound
  AST node (virtual Entry/Exit/Panic/loop-exit nodes drop out, as in
  `attach_cfg_edges`). No dominator library used.
- `tests/cfg_dominators.rs` — 17 tests (see Verification).

## Design decision: statement-level flow nodes

The CFG uses statement-level flow nodes, not basic blocks:

- one CFG node per AST `Statement` node plus `Condition` nodes for branching
  decisions, plus nodes for block tail expressions, plus virtual
  Entry/Exit/Panic nodes;
- the AST layer (t_75f6e158) already materializes kinded Statement nodes with
  byte-authoritative spans, so CFG nodes map 1:1 to source locations;
- branching must be labelled per branch (`true`/`false`, match-arm pattern
  text), which basic blocks would hide;
- basic blocks remain a purely local re-computation for any consumer.

`Block` AST nodes are transparent (their statements are inlined into the
enclosing flow); the control-flow constructs wrapped in `expression_statement`
by the AST layer are unwrapped.

## Edge model (all in the `cfg` namespace, `EdgeKind::Cfg { branch_label }`)

- sequencing: unlabelled;
- `if`: condition → first node of each branch ("true"/"false"); no-else
  falls through to the join with "false";
- `match`: condition → arm-entry nodes (label = pattern text, e.g. "0",
  "_"); guarded arms: arm-entry → guard (unlabelled), guard → body ("true"),
  guard → next arm (unlabelled, pattern/guard miss);
- `loop`: head node; body back-edges to head; break → loop-exit node;
- `while`/`for`: condition → body ("true") / loop-exit node ("false"); body
  back-edges to condition; `for` adds an `Iterate` node (binds the loop
  variable) ahead of the condition;
- `break` → loop exit ("break"), `continue` → loop head ("continue"),
  `return` → Exit ("return");
- always-panic statements (`panic!`/`unreachable!`/`todo!`/
  `unimplemented!` macro invocations) flow to the function's `Panic` node
  ("panic") and do NOT fall through;
- maybe-panic operations (best-effort, conservative superset): indexing,
  `.unwrap()`/`.expect()`, division/remainder with a possibly-zero divisor,
  and parse-degraded (`error:`) subtrees get an EXTRA edge to the shared
  `Panic` node ("panic") alongside normal flow. One Panic node per function,
  created lazily.

## Dominator tree

`dominators.rs` implements the textbook Lengauer–Tarjan algorithm
(Lengauer & Tarjan 1979): DFS preorder numbering, semi-dominators in reverse
preorder with path-compressing `eval` over the link-eval forest, bucket
processing per DFS parent, then the idom correction pass in preorder.
Post-dominators are computed by running the same code on the reversed edge
set rooted at the function Exit. Unreachable nodes are excluded.

## Verification (`cargo test`: 36/36 pass — 1 schema + 15 AST + 20 CFG)

Hand-drawn expected graphs (edge-level assertions):
- straight-line code: exact node count, sequence, unlabelled edges, no Panic;
- if/else: "true"/"false" labelled edges into both arms, join at Exit;
- if without else: false edge falls through to the join;
- `loop` with break+continue: head/back edges, break → loop-exit ("break"),
  continue → head ("continue"), loop-exit → Exit;
- `while`: cond → body ("true"), back edge, cond → loop-exit ("false");
- `for`: Entry → Iterate → cond → body ("true") / loop-exit ("false"),
  back edge to cond;
- early return: return → Exit ("return"), tail flows only from "false";
- match with guards: dispatch chain cond → arm0 → arm1 → guard → arm2,
  unlabelled pattern/guard-miss edges, pattern-text labels ("0", "_") on
  unguarded arms, "true" into the guarded arm body;
- panicking ops: shared Panic node, "panic" edges from index/div/unwrap
  statements, none from the panic-free tail; `panic!` → Panic without
  fall-through.

Dominator verification:
- `dominators_match_brute_force_on_all_constructs`: LT vs an independent
  brute-force dominator computation (reachability-with-node-removed) over 10
  CFG shapes: straight-line, if/else, if-no-else, loop+break+continue, while,
  for, early return, match with guards, nested loops, break-in-for-in-if;
- `nested_loop_dominators`, `dominator_tree_shape_on_branching_code`: tree
  shape assertions (idom of both arms = condition, exit's idom = condition);
- `dom_edges_pairwise_and_postdom_smoke`: dom-edge count and post-dominator
  tree well-formedness (each node at most once as the dominated child).

Graph attachment of the dominator trees (review round 1 fix):
- `dominator_edges_attach_in_dominators_namespace` and
  `attached_dominator_edges_live_in_dominators_namespace`: `attach_dominator_edges`
  populates `edges_by_overlay("dominators")` with only `Dominate`/`PostDominate`
  edges; other overlays untouched; no self-edges; endpoints are existing AST
  nodes; the attached `Dominate` edge set equals the brute-force idom map
  (AST-mapped) on a branchy CFG. `dom_edges` pairs are (dominator, dominated)
  — the earlier flipped ordering found in review was fixed.

Namespace checks:
- all CFG edges are `EdgeKind::Cfg`; attaching them to the shared graph makes
  them visible via `edges_by_overlay("cfg")` (no AST edges disturbed);
- `build_cfgs_on_sample_crate`: 14 function bodies in the sample crate, every
  non-Panic node reachable from Entry, spans carried from the AST layer;
- CFG serializes/deserializes losslessly (serde JSON round-trip).

## For downstream tasks (t_8306ad86 DFG/call)

- `CfgNodeKind::Statement { ast }` binds each flow node to the AST Statement
  node — the DFG pass can walk CFG edges between the same shared nodes via
  `attach_cfg_edges`, or use `Cfg` directly for flow-sensitive analyses.
- Loops are modelled with explicit condition/Iterate nodes; loop-carried
  dependencies show up as back edges to the condition node.
- The Panic node id is reachable via `CfgNodeKind::Panic` — panic flow is a
  terminal side-exit, not on the entry→exit paths.
