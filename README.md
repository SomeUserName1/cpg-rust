# cpg — Code Property Graph pipeline (Rust milestone)

One command builds the complete CPG for a folder of Rust source; a second
dumps any function's subgraph for manual inspection.

## Build

    cargo build --release
    ./target/release/cpg-ast build <FOLDER> [-o out.json]
    ./target/release/cpg-ast inspect <graph.json> <FUNCTION> [--format dot|json] [-o out]

## What `cpg build` produces

A single JSON file (serde, v1 storage per `docs/cpg-schema.md` §7) containing
the full graph with every layer attached to one shared node set:

| Layer | Edges (overlay namespace) | Built by |
|---|---|---|
| AST | `Ast { order, field }` (`ast`) | tree-sitter front-end, byte-authoritative spans |
| CFG | `Cfg { branch_label }` (`cfg`) | statement-level flow graph, per function, branch-labelled |
| Dominators | `Dominate` / `PostDominate` (`dominators`) | Lengauer–Tarjan (from scratch), immediate dom/postdom trees |
| Data dependence | `DataDependence { variable }` (`dfg`) | scope-chain reaching definitions, loop-carried deps, arg→param / return→call-site hand-off |
| Call graph | `Call { resolved, argument_index }` (`call`) | direct/path/method calls with receiver-type inference; unresolved targets get synthetic `<unresolved:...>` Function nodes, never dropped |
| Module deps | `ModuleDependency { import_path }` (`moddep`) | `mod x;` links to `x.rs` / `x/mod.rs`; `use`/`extern crate` to internal files or `<crate:name>` nodes from Cargo.toml |

The `build` step prints node/edge counts plus per-layer statistics and
`parse_errors` (parse-degraded subtrees are kept and marked, never dropped).

### Node/edge contract highlights

- Edge ids are unique; edge endpoints never dangle; no self-edges.
- Every resolved `Call` edge targets a `Function` node (or a
  `MacroInvocation` node for macro links); unresolved calls carry
  `resolved: false` and target a synthetic `<unresolved:...>` Function node.
- CFG edges are AST-node → AST-node: virtual flow nodes (Entry/Exit/Panic)
  are dropped at attachment; the loop-exit node maps to the `for`/`while`
  statement's AST node.
- JSON → Graph → JSON round-trip is lossless (asserted by tests).

## What `cpg inspect` shows

Given the graph JSON and a function name, prints (or writes with `-o`) the
function's subgraph: the Function node, everything reachable via `Ast` edges
from its body, all overlay edges with both endpoints inside that set, and
outgoing `Call` edges to synthetic targets. DOT output is styled per overlay
(CFG = bold red with branch labels, DFG = dashed blue, call = green/orange by
resolution, dom/postdom = dotted); JSON output lists nodes with kind/label
and edges with overlay + payload. Unknown function names exit non-zero.

    ./target/release/cpg-ast inspect cpg.json factor_iter --format dot -o factor_iter.dot

## End-to-end verification

`tests/e2e_pipeline.rs` runs the complete pipeline and asserts cross-layer
consistency: unique ids / no dangling endpoints / no self-edges, all overlays
present, every non-degenerate function owns CFG edges, call-resolution
contract, JSON round-trip losslessness, and determinism per input.
`crate_wildcard_for_regression` covers `for _ in ...` loops (the wildcard
pattern is an anonymous tree-sitter token; the CFG builder used to panic —
now it emits `for _ in <iter>` Iterate nodes).

Real-crate runs (vendored under `../../t_4ffa769c/vendor/` because the
environment blocks network fetches; crates are MIT/Apache-2.0, copied from
the local cargo registry cache):

| Crate | Files | Nodes | Edges | Parse errors | DFG | Call (res/unres) |
|---|---|---|---|---|---|---|
| either 1.13.0 | 5 | 8 627 | 9 708 | 0 | 252 | 108 / 148 |
| unicode-ident 1.0.13 | 11 | 23 109 | 23 695 | 0 | 160 | 26 / 118 |
| sample-crate | 3 | 420 | 527 | 0 | 37 | 5 / 19 |

All three satisfy the integrity + call contracts (checked independently of
the Rust test harness).

## Architecture and extension path

```
folder ──► ast_builder (tree-sitter, per language)
              │  nodes: File/Module/Function/Statement/Expression/
              │         Identifier/Declaration/Type/Literal (+ front-end
              │         extensions MacroInvocation, Import)
              │  edges: Ast only
              ▼
          shared Graph (src/schema.rs)
              ├── overlays.rs   (language-independent: dfg, call, imports)
              ├── cfg.rs        (language-independent over Statement nodes)
              └── dominators.rs (pure graph algorithm over CFG)
```

**Adding TypeScript next.** Only `ast_builder` changes:

1. Add the `tree-sitter-typescript` grammar (0.23.x, prototyped in
   t_49c7d4f7) and a parallel `ast_builder_ts.rs` translating its tree into
   the *same* node kinds + `Ast` edges (schema doc §8 fit check: ES
   modules/namespaces → `Module`; type annotations feed
   `Expression::type_name` where cheap; no def-site type checking).
2. Extend `discover_rust_files` into a per-language dispatcher (.ts/.tsx) and
   `build_folder_full` to run the front-end per file, merging into one graph.
3. Touch nothing else — `overlays.rs`, `cfg.rs`, `dominators.rs`,
   serialization, CLI, and all tests are language-independent by design
   (they only see `NodeKind`/`EdgeKind`).

**Then C.** Same recipe with `tree-sitter-c`: per-file translation units
(`File` nodes, no `Module`), preprocessor directives stay opaque
`MacroInvocation`-style nodes, consistent with schema G1 (fuzzy parsing
tolerates unresolved syntax).

**Tracked future extensions** (epic graph types not yet built):

- **Type hierarchies** — `impl`/`trait` relation graphs beyond the current
  contains-edges (`EdgeKind::TypeHierarchy` exists; a dedicated overlay for
  subtyping/trait bounds is future work).
- **Execution-order graph** — inter-procedural ordering / summary edges
  building on the call graph.

These are deliberately out of scope for the Rust milestone; the schema's
namespaced overlay design lets each land as a new pass with no migration.

## Layout

- `src/ast_builder.rs` — tree-sitter Rust front-end + folder driver
- `src/schema.rs` — shared node/edge types, namespaced overlays, JSON (de)ser
- `src/overlays.rs` — data-dependence, call graph, import/module passes
- `src/cfg.rs` — statement-level CFG construction + attachment
- `src/dominators.rs` — Lengauer–Tarjan idoms/postdoms + attachment
- `src/inspect.rs` — function-subgraph DOT/JSON dumps
- `src/main.rs` — `build` / `inspect` CLI
- `tests/` — ast_layer, cfg_dominators, overlays, e2e_pipeline
- `docs/cpg-schema.md` — schema decisions and traceability
