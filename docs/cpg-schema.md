# CPG Core Schema and Graph Overlay Architecture

Task: t_ebf8bd37 (child of the CPG Construction epic t_8472ef5b).

## 1. References

Every edge type in this design traces to one of:

- **[Y14]** Yamaguchi, Golde, Arp, Rieck, "Modeling and Discovering Vulnerabilities with Code Property Graphs", IEEE S&P 2014. (Original CPG: AST + CFG + DFG joined on a shared node set.)
- **[A22]** Weiss, Banse et al. (Fraunhofer AISEC), "A Language-Independent Analysis Platform for Source Code", arXiv:2203.08424, 2022. (Labeled directed multi-graph with properties; AST/EOG/DFG/type/call overlays; INVOKES, REFERS_TO.)

A traceability table is in §6.

## 2. Design goals (from [A22])

- G1: analyze incomplete / non-compilable code (fuzzy parsing via tree-sitter).
- G2: language-independent representation — Rust, then TypeScript, then C front-ends plug into the *same* schema.
- G3: usable semi-automatically (CLI / CI).
- G4: model language-level semantics where it matters, but keep the common core small.

## 3. Core principle: one shared node set, namespaced edge sets

The graph is a labeled directed multi-graph [A22 §1]: nodes are syntactic
entities, edges capture relations. The single defining idea inherited from
[Y14] is that **all overlays live on the same node set**. There is no
"CFG copy of node 42" — node 42 *is* the statement, and the CFG, DFG, AST,
and call overlays each hold edges pointing at it.

Concretely:

    Graph = { nodes: Vec<Node>, edges: Vec<Edge> }
    Edge   = { id, src, dst, kind: EdgeKind }

`EdgeKind` is an enum whose variants are grouped by *overlay namespace*.
Each overlay is a subset of edges, addressable by name (`edges_by_overlay("cfg")`).
The AST overlay is the base: every node exists exactly once in it, connected
by AST parent/child edges [A22 §3.1]. All other overlays add edges *between
existing AST nodes* and may only reference nodes that exist.

### Adding a new graph type without schema migration

Two mechanisms:

1. **Closed-core, open extension.** The core `EdgeKind` enum covers the
   overlays defined by [Y14]+[A22]. Front-ends and passes may add overlay
   edges without touching the enum only if they reuse existing variants'
   payloads (e.g. a new analysis that produces def->use style edges reuses
   `DataDependence`).
2. **Versioned graph envelope.** Serialized graphs carry a
   `schema_version` + `edge_namespaces: Vec<OverlayDecl>` header. Unknown
   overlay namespaces in the header are *preserved verbatim* on load
   (stored as tagged opaque edges `EdgeKind::Ast`-style escapes are NOT
   used; instead the serializer keeps unknown overlays in a side table),
   so a newer producer's output remains loadable by an older consumer
   without regenerating code. This mirrors how [A22] adds semantics in
   passes rather than in the schema.

For the first milestones (AST layer, then CFG/DFG/call), the enum is closed
and versioned; the side-table mechanism exists in the format but has zero
entries. No migration is ever needed to *add* an overlay: adding enum
variants is additive (serde externally tagged), and old readers reject or
drop unknown variants only when strict mode is requested.

## 4. Node types (language-agnostic)

All nodes carry `NodeCommon { span, code, order }` [A22 §3.1: CODE, ORDER,
LINE_NUMBER/COLUMN_NUMBER, OFFSET/OFFSET_END].

| Kind | Maps to CPG [A22] | Notes |
|---|---|---|
| `File` | TranslationUnitDeclaration / FILE | path + content hash |
| `Module` | NamespaceDeclaration / NAMESPACE_BLOCK | crate, TS module, C: none per-file |
| `Function` | METHOD | incl. methods in impl/trait blocks |
| `Block` | BLOCK | compound statements |
| `Statement` | control structures, RETURN etc. | `stmt_kind` is free-form ("if","for","return","let") |
| `Expression` | EXPRESSION, CALL, operators | `expr_kind` free-form ("binary","call","cast") |
| `Identifier` | IDENTIFIER, FIELD_IDENTIFIER | references |
| `Declaration` | LOCAL, PARAMETER | declaring occurrences |
| `Type` | TYPE / TYPE_DECL | references and declarations |
| `Literal` | LITERAL | string/number/bool |
| `MacroInvocation` | (front-end extension) | opaque node annotated for future expansion — required for Rust macros [A22 G1: fuzzy parsing tolerates unresolved syntax] |
| `Import` | (front-end extension) | source of module-dependency edges |

Spans are byte-range authoritative; line/col derived once at parse.

## 5. Edge namespaces and their paper provenance

| EdgeKind | Payload | Overlay | Provenance |
|---|---|---|---|
| `Ast { order, field }` | sibling order, tree-sitter field name | AST | [A22 §3.1] AST edges; [Y14] AST |
| `Cfg { branch_label }` | Some("true"/"false") at branching points | CFG/EOG | [Y14] CFG; [A22 §3.2] EOG with true/false labels at branches |
| `DataDependence { variable }` | def -> use, variable name | DFG/PDG | [Y14] DFG; [A22 §3.3] DFG edges |
| `Call { resolved, argument_index }` | call site -> callee | call graph | [A22 §3.4] INVOKES (best-effort); [Y14] CALL |
| `Dominate` | immediate dominator edge | dominators | [A22] Dominators layer (DOMINATE); computed from CFG per Lengauer–Tarjan (epic body) |
| `PostDominate` | immediate post-dominator edge | dominators | [A22] Dominators layer (POST_DOMINATE) |
| `TypeHierarchy { relation }` | subtype-of / contains | type system | [A22 §3.4] type sub-graph |
| `ModuleDependency { import_path }` | file/module depends on module | imports | [A22 §4.1] frontends collect imports; epic "package/module/library dependencies via imports & includes" |

Notes on choices:

- The CFG payload carries branch-condition labels, matching [A22 §3.2]:
  "EOG edges at such branching positions save additional information on the
  result of the branching expression ... e.g. true or false for conditions."
- Data-dependence is def->use, matching [A22 §3.3] and [Y14]'s
  definition-use perspective.
- Call edges are best-effort resolution [A22 §3.4]; `resolved` records
  whether the target is static.
- Import statements materialize as nodes (they are syntax) and the
  dependency *edge* may originate from the File or Module node.

## 6. Edge-type traceability table (acceptance criterion)

Every EdgeKind variant above cites its exact section in [Y14] or [A22];
macro/import nodes are explicitly marked front-end extensions, not paper
edges. No edge type lacks a citation.

## 7. Storage / serialization decision

**Decision: serde JSON (per-graph single file) for v1, with a protobuf
schema as the v2 upgrade path — not GraphSON, not an embedded graph DB as
primary store.**

Justification:

- The graph's shape (closed node enum, tagged edge enum, property bags) maps
  1:1 onto serde's externally-tagged enums — a round-trip is lossless with
  zero mapping code, which the acceptance tests exercise.
- GraphSON (TinkerPop) would pull in the Gremlin property-model impedance
  mismatch (vertex/edge/property trichotomy, no enum payload, string-typed
  everything) for zero gain at our scale.
- Embedded graph DBs (Neo4j, sled-backed) are the wrong v1 dependency:
  the analysis passes are batch in-memory computations over a graph that
  fits in RAM for our target crate sizes ([A22] evaluates small/medium
  repos). We already run an optional SQLite persistence layer elsewhere in
  the stack (`live_store`, t_35fb2f31) — that remains an optional
  durability layer *above* the JSON format, not the format itself.
- Protobuf is the right *interchange* upgrade when Rust/TS/C graphs must be
  consumed by heterogeneous tooling or when file size matters (varint
  node/edge ids, repeated fields). The enum tags map directly onto protobuf
  oneofs; the v2 proto is mechanical to derive from `EdgeKind`/`NodeKind`.

Lossless round-trip is a hard invariant: JSON -> Graph -> JSON must be
byte-stable modulo key ordering, and the unit test in `src/lib.rs` asserts
structural equality after a round-trip.

## 8. Language-agnostic fit check (Rust / TypeScript / C)

- Rust: modules map to `Module`; macros stay opaque `MacroInvocation`
  nodes; `impl`/`trait` blocks create `TypeHierarchy` contains edges.
- TypeScript: `Module` for ES modules/namespaces; type annotations feed
  `Expression::type_name` when cheaply available (stripping-only parser
  [spike t_49c7d4f7]); no def-site type checking.
- C: per-file translation units as `File` + `Module` absent; preprocessor
  directives stay opaque (no expansion), consistent with G1.

Front-end contract (as in [A22 §4.1]): a front-end translates its parser's
tree into these node kinds + `Ast` edges only; all other overlays are built
by language-independent passes over that base.

## 9. Deliverables in this repo

- `src/lib.rs` — the schema types (compile + unit test: overlay filtering,
  JSON round-trip losslessness).
- This doc — `docs/cpg-schema.md`.
- `refs/` — fetched CPG schema sources from ShiftLeftSecurity/codepropertygraph
  for cross-checking ([A22]'s reference implementation lineage).
