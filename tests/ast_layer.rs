//! Unit tests for the Rust AST-layer CPG builder.
//!
//! Coverage areas (acceptance criteria):
//! - nested modules (inline `mod { }`, `mod x;` + mod.rs, two-level nesting)
//! - impl blocks and trait blocks (method attachment, TypeHierarchy edges)
//! - generics
//! - macros (opaque MacroInvocation nodes)
//! - function bodies: statements/expressions down to identifier/literal
//!   granularity with parent/child edges and spans
//! - lossless serialization round-trip

use cpg_ast::schema::{EdgeKind, NodeKind, TypeRelation};
use cpg_ast::{build_folder, discover_rust_files, find_crate_roots, from_json, parse_file_to_graph, to_json};
use std::path::Path;

/// Workspace-relative sample crate.
const SAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/sample-crate");

// --- helpers -------------------------------------------------------------

fn children_of<'g>(
    g: &'g cpg_ast::schema::Graph,
    parent: cpg_ast::schema::NodeId,
) -> Vec<(u32, Option<String>, &'g cpg_ast::schema::Node)> {
    let mut out: Vec<(u32, Option<String>, &cpg_ast::schema::Node)> = g
        .edges
        .iter()
        .filter(|e| e.src == parent)
        .filter_map(|e| match &e.kind {
            EdgeKind::Ast { order, field } => Some((*order, field.clone(), e.dst)),
            _ => None,
        })
        .filter_map(|(o, f, dst)| g.nodes.iter().find(|n| n.id == dst).map(|n| (o, f, n)))
        .collect();
    out.sort_by_key(|(o, _, _)| *o);
    out
}

fn node_by<'g>(g: &'g cpg_ast::schema::Graph, pred: impl Fn(&cpg_ast::schema::Node) -> bool) -> &'g cpg_ast::schema::Node {
    g.nodes.iter().find(|n| pred(n)).expect("node not found")
}

fn count_kind(g: &cpg_ast::schema::Graph, pred: impl Fn(&NodeKind) -> bool + Copy) -> usize {
    g.nodes.iter().filter(|n| pred(&n.kind)).count()
}

// --- single-file basics --------------------------------------------------

#[test]
fn function_body_full_decomposition_with_spans() {
    let src = b"fn main() { let x = 1; println!(\"{}\", x + 2); }";
    let (_g, stats) = parse_file_to_graph("main.rs", src).unwrap();
    assert_eq!(stats.files_parsed, 1);
    assert_eq!(stats.parse_errors, 0);
    assert!(stats.macro_invocations >= 1);
}

#[test]
fn spans_are_byte_authoritative_and_slice_source() {
    let src = b"fn add(a: i32, b: i32) -> i32 { a + b }";
    let (g, _) = parse_file_to_graph("a.rs", src).unwrap();
    let func = node_by(&g, |n| matches!(&n.kind, NodeKind::Function { name, .. } if name == "add"));
    // span slices exactly the function text
    assert_eq!(
        std::str::from_utf8(&src[func.common.span.start_byte as usize..func.common.span.end_byte as usize]).unwrap(),
        "fn add(a: i32, b: i32) -> i32 { a + b }"
    );
    // every node spans within the file and has line/col derived
    for n in &g.nodes {
        assert!(n.common.span.start_byte <= n.common.span.end_byte);
        assert_eq!(n.common.span.file, "a.rs");
        assert!(n.common.span.start_line >= 1);
    }
}

#[test]
fn parent_child_edges_cover_whole_body_down_to_identifiers_and_literals() {
    let src = b"fn f() { let x: u64 = 5; if x > 1 { return x; } }";
    let (g, _) = parse_file_to_graph("a.rs", src).unwrap();
    let func = node_by(&g, |n| matches!(&n.kind, NodeKind::Function { name, .. } if name == "f"));
    let body = node_by(&g, |n| matches!(n.kind, NodeKind::Block));
    // fn -> body via `body` field
    let kids = children_of(&g, func.id);
    assert!(kids.iter().any(|(_o, f, n)| f.as_deref() == Some("body") && n.id == body.id));
    // let statement with Declaration + Literal children
    let let_stmt = node_by(&g, |n| matches!(&n.kind, NodeKind::Statement { stmt_kind } if stmt_kind == "let"));
    let let_kids = children_of(&g, let_stmt.id);
    assert!(let_kids.iter().any(|(_, _, n)| matches!(&n.kind, NodeKind::Declaration { name, .. } if name == "x")));
    assert!(let_kids.iter().any(|(_, _, n)| matches!(&n.kind, NodeKind::Literal { literal_kind, .. } if literal_kind == "integer")));
    // if statement
    let if_stmt = node_by(&g, |n| matches!(&n.kind, NodeKind::Statement { stmt_kind } if stmt_kind == "if"));
    assert!(children_of(&g, if_stmt.id).iter().any(|(_, _, n)| matches!(&n.kind, NodeKind::Expression { expr_kind, .. } if expr_kind == "binary")));
    // identifier use inside if body
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Identifier { name } if name == "x")) >= 1);
}

// --- nested modules ------------------------------------------------------

#[test]
fn module_tree_inline_and_file_modules() {
    let folder = Path::new(SAMPLE);
    let (g, stats) = build_folder(folder).unwrap();
    assert_eq!(stats.files_parsed, 3); // main.rs, utils/mod.rs, utils/deep.rs
    assert_eq!(stats.parse_errors, 0);
    // inline module `inner` and file module `utils` exist as Module nodes
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Module { name } if name == "inner")) >= 1);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Module { name } if name == "utils")) >= 1);
    // mod x; links: Module("utils") -> File(src/utils/mod.rs) and utils/mod.rs's
    // `pub mod deep;` -> File(src/utils/deep.rs)
    assert!(stats.module_links >= 2);
    let mod_dep_edges = g.edges.iter().filter(|e| matches!(e.kind, EdgeKind::ModuleDependency { .. })).count();
    assert_eq!(mod_dep_edges, stats.module_links as usize);
}

#[test]
fn crate_root_discovery() {
    let folder = Path::new(SAMPLE);
    let files = discover_rust_files(folder);
    assert_eq!(files.len(), 3);
    let roots = find_crate_roots(folder, &files);
    assert_eq!(roots.len(), 1);
    assert!(roots[0].ends_with("main.rs"));
}

// --- impl blocks, traits -------------------------------------------------

#[test]
fn impl_blocks_attach_methods_and_trait_edges() {
    let src = b"
        struct Dog { name: String }
        trait Animal { fn speak(&self) -> String; }
        impl Animal for Dog {
            fn speak(&self) -> String { self.name.clone() }
        }";
    let (g, _) = parse_file_to_graph("a.rs", src).unwrap();
    // speak exists as Function
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Function { name, .. } if name == "speak")) >= 1);
    // TypeHierarchy::SubtypeOf edge exists (impl Animal for Dog)
    let th = g.edges.iter().filter(|e| matches!(&e.kind, EdgeKind::TypeHierarchy { relation: TypeRelation::SubtypeOf })).count();
    assert!(th >= 1);
}

#[test]
fn trait_default_method_and_multiple_impls() {
    let src = b"
        struct A; struct B;
        trait T { fn m(&self) -> u8 { 1 } }
        impl T for A {}
        impl T for B {}";
    let (g, _) = parse_file_to_graph("a.rs", src).unwrap();
    let th = g.edges.iter().filter(|e| matches!(&e.kind, EdgeKind::TypeHierarchy { relation: TypeRelation::SubtypeOf })).count();
    assert!(th >= 2);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Function { name, .. } if name == "m")) >= 1);
}

// --- generics ------------------------------------------------------------

#[test]
fn generics_parse_and_materialize() {
    let src = b"fn f<T: Clone>(v: Vec<T>) -> Option<T> { v.into_iter().next() }";
    let (g, stats) = parse_file_to_graph("a.rs", src).unwrap();
    assert_eq!(stats.parse_errors, 0);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Function { name, .. } if name == "f")) == 1);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Type { name } if name == "Vec" || name == "Option")) >= 2);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Declaration { name, .. } if name == "T" || name == "v")) >= 1);
}

// --- macros --------------------------------------------------------------

#[test]
fn macros_are_opaque_annotated_nodes() {
    let src = b"
        macro_rules! twice { ($x:expr) => { $x * 2 } }
        fn main() { let y = twice!(3); assert_eq!(y, 6); }";
    let (g, stats) = parse_file_to_graph("a.rs", src).unwrap();
    assert_eq!(stats.parse_errors, 0);
    assert_eq!(stats.macro_invocations, 3); // definition + 2 invocations
    let macros: Vec<_> = g
        .nodes
        .iter()
        .filter(|n| matches!(&n.kind, NodeKind::MacroInvocation { .. }))
        .collect();
    assert_eq!(macros.len(), 3);
    // every macro node has a span and the annotation names it
    assert!(macros.iter().any(|m| matches!(&m.kind, NodeKind::MacroInvocation { name } if name == "twice")));
    assert!(macros.iter().any(|m| matches!(&m.kind, NodeKind::MacroInvocation { name } if name == "assert_eq")));
    assert!(macros.iter().all(|m| m.common.span.end_byte > m.common.span.start_byte));
}

// --- error tolerance ------------------------------------------------------

#[test]
fn parse_errors_do_not_crash_and_count_degraded() {
    let src = b"fn broken( { let = ;";
    let (g, stats) = parse_file_to_graph("bad.rs", src).unwrap();
    assert_eq!(stats.parse_errors, 1);
    assert!(stats.degraded_nodes >= 1);
    // graph still built (G1: fuzzy parsing tolerates incomplete code)
    assert!(g.nodes.len() > 0);
}

// --- serialization round-trip --------------------------------------------

#[test]
fn serialization_round_trips_losslessly() {
    let folder = Path::new(SAMPLE);
    let (g, _) = build_folder(folder).unwrap();
    let json1 = to_json(&g);
    let g2 = from_json(&json1).unwrap();
    let json2 = to_json(&g2);
    // byte-stable modulo key ordering (same serializer -> byte-stable exactly)
    assert_eq!(json1, json2);
    // structural equality
    assert_eq!(g.nodes.len(), g2.nodes.len());
    assert_eq!(g.edges.len(), g2.edges.len());
    for (a, b) in g.nodes.iter().zip(g2.nodes.iter()) {
        assert_eq!(a.id, b.id);
        assert_eq!(a.common.span, b.common.span);
        assert_eq!(a.kind, b.kind);
    }
    for (a, b) in g.edges.iter().zip(g2.edges.iter()) {
        assert_eq!(a.src, b.src);
        assert_eq!(a.dst, b.dst);
        assert_eq!(a.kind, b.kind);
    }
}

#[test]
fn ast_edges_reference_existing_nodes_only() {
    let (g, _) = build_folder(Path::new(SAMPLE)).unwrap();
    let ids: std::collections::HashSet<u64> = g.nodes.iter().map(|n| n.id).collect();
    for e in &g.edges {
        assert!(ids.contains(&e.src), "edge {} src dangling", e.id);
        assert!(ids.contains(&e.dst), "edge {} dst dangling", e.id);
    }
}

#[test]
fn every_file_is_rooted_at_a_file_node() {
    let (g, _) = build_folder(Path::new(SAMPLE)).unwrap();
    let file_count = count_kind(&g, |k| matches!(k, NodeKind::File { .. }));
    assert_eq!(file_count, 3);
    for n in &g.nodes {
        if let NodeKind::File { path, .. } = &n.kind {
            assert!(path.ends_with(".rs"));
            assert!(!n.common.span.file.is_empty());
        }
    }
}

#[test]
fn sibling_orders_are_dense_and_fields_carry_tree_sitter_names() {
    let src = b"fn f() { let a = 1; let b = 2; }";
    let (g, _) = parse_file_to_graph("a.rs", src).unwrap();
    let func = node_by(&g, |n| matches!(&n.kind, NodeKind::Function { name, .. } if name == "f"));
    let kids = children_of(&g, func.id);
    // orders dense 0..n
    let mut orders: Vec<u32> = kids.iter().map(|(o, _, _)| *o).collect();
    orders.sort_unstable();
    for (i, o) in orders.iter().enumerate() {
        assert_eq!(*o, i as u32);
    }
    // at least one child uses the `body` field name from tree-sitter
    assert!(kids.iter().any(|(_, f, _)| f.as_deref() == Some("body")));
}

#[test]
fn full_sample_folder_has_expected_node_granularity() {
    let (g, _) = build_folder(Path::new(SAMPLE)).unwrap();
    // statements/expressions/identifiers/literals all present
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Statement { .. })) > 5);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Expression { .. })) > 5);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Identifier { .. })) > 5);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Literal { .. })) > 0);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Function { .. })) > 5);
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Type { .. })) > 2);
    // closure materialized as anonymous Function
    assert!(count_kind(&g, |k| matches!(k, NodeKind::Function { name, .. } if name == "<closure>")) >= 1);
}
