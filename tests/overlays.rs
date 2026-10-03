//! Integration tests for the three overlay passes (task t_8306ad86):
//! data dependence / def-use, call graph, module dependencies.
//!
//! Acceptance criteria (task body):
//! - def-use links correct for variable shadowing, loop-carried
//!   dependencies, and parameter passing
//! - call graph contains caller->callee edges for all direct calls in the
//!   sample crate, unresolved targets flagged (never dropped)
//! - import graph matches the crate's actual module structure

use cpg_ast::schema::{EdgeKind, Graph, Node, NodeId, NodeKind};
use cpg_ast::{build_folder_full, parse_file_to_graph};
use std::path::Path;

const SAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/sample-crate");

// --- helpers -------------------------------------------------------------

fn overlay_graph(g: &Graph) -> (Vec<&EdgeKind>, Vec<&Node>) {
    let kinds: Vec<&EdgeKind> = g.edges.iter().map(|e| &e.kind).collect();
    let nodes: Vec<&Node> = g.nodes.iter().collect();
    (kinds, nodes)
}

fn ddf_edges(g: &Graph) -> Vec<(&Node, &Node, &str)> {
    g.edges
        .iter()
        .filter_map(|e| match &e.kind {
            EdgeKind::DataDependence { variable } => Some((
                node_of(g, e.src),
                node_of(g, e.dst),
                variable.as_str(),
            )),
            _ => None,
        })
        .collect()
}

fn node_of<'g>(g: &'g Graph, id: NodeId) -> &'g Node {
    g.nodes.iter().find(|n| n.id == id).expect("node exists")
}

fn find_node<'g>(g: &'g Graph, pred: impl Fn(&Node) -> bool + Copy) -> &'g Node {
    g.nodes.iter().find(|n| pred(n)).expect("node not found")
}

fn call_edges(g: &Graph) -> Vec<(NodeId, NodeId, bool)> {
    g.edges
        .iter()
        .filter_map(|e| match &e.kind {
            EdgeKind::Call { resolved, .. } => Some((e.src, e.dst, *resolved)),
            _ => None,
        })
        .collect()
}

fn mod_dep_edges(g: &Graph) -> Vec<(NodeId, NodeId, String)> {
    g.edges
        .iter()
        .filter_map(|e| match &e.kind {
            EdgeKind::ModuleDependency { import_path } => {
                Some((e.src, e.dst, import_path.clone()))
            }
            _ => None,
        })
        .collect()
}

fn source_of(code: &str) -> Vec<u8> {
    code.as_bytes().to_vec()
}

// =========================================================================
// 1. Data dependence / def-use
// =========================================================================

#[test]
fn dfg_parameter_passing_links_param_decl_to_use() {
    let src = source_of("fn f(a: i32, b: i32) -> i32 { a + b }");
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (mut g, ostats) = run_overlays(g);
    assert!(ostats.dfg_edges > 0);
    let edges = ddf_edges(&g);
    // both parameter declarations flow to their uses
    for p in ["a", "b"] {
        let d = find_node(&g, |n| {
            matches!(&n.kind, NodeKind::Declaration { name, is_parameter: true } if name == p)
        });
        assert!(
            edges.iter().any(|(s, _t, v)| s.id == d.id && *v == p),
            "missing param->use edge for {p}"
        );
    }
}

#[test]
fn dfg_let_and_use_linked() {
    let src = source_of("fn f() { let x = 1; let y = x + 2; y }");
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (g, _) = run_overlays(g);
    let edges = ddf_edges(&g);
    // x flows into `x + 2`
    let xdef = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Declaration { name, .. } if name == "x")
    });
    let xuse = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Identifier { name } if name == "x")
    });
    assert!(
        edges.iter().any(|(s, t, v)| s.id == xdef.id && t.id == xuse.id && *v == "x"),
        "let x -> use x missing"
    );
    // y flows to its final use
    let ydef = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Declaration { name, .. } if name == "y")
    });
    assert!(edges.iter().any(|(s, _, v)| s.id == ydef.id && *v == "y"));
}

#[test]
fn dfg_variable_shadowing_picks_innermost() {
    let src = source_of(
        "fn f() {\n  let x = 1;\n  { let x = 2; use_x(x); }\n  use_x(x);\n}",
    );
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (g, _) = run_overlays(g);
    let edges = ddf_edges(&g);
    let xs: Vec<&Node> = g
        .nodes
        .iter()
        .filter(|n| matches!(&n.kind, NodeKind::Declaration { name, .. } if name == "x"))
        .collect();
    assert_eq!(xs.len(), 2, "two x declarations expected");
    // inner use must link only to the inner (second) declaration
    let inner_use = g
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Identifier { name } if name == "x")
            && n.common.span.start_line == 3)
        .expect("inner use");
    let xuses: Vec<(&Node, &Node, &str)> = edges
        .iter()
        .filter(|(_, t, _)| t.id == inner_use.id)
        .map(|(s, t, v)| (*s, *t, *v))
        .collect();
    assert_eq!(xuses.len(), 1, "inner x must have exactly one reaching def");
    assert_eq!(xuses[0].0.id, xs[1].id, "inner def wins (shadowing)");
    // outer use links only to the outer declaration
    let outer_use = g
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Identifier { name } if name == "x")
            && n.common.span.start_line == 4)
        .expect("outer use");
    let outer_x: Vec<(&Node, &Node, &str)> = edges
        .iter()
        .filter(|(_, t, _)| t.id == outer_use.id)
        .map(|(s, t, v)| (*s, *t, *v))
        .collect();
    assert_eq!(outer_x.len(), 1);
    assert_eq!(outer_x[0].0.id, xs[0].id, "outer def shadows nothing here");
}

#[test]
fn dfg_loop_carried_dependency() {
    // `acc` is re-assigned inside the loop and used in the same iteration's
    // RHS *before* the assignment textually — loop-carried dependence.
    let src = source_of(
        "fn f() {\n  let mut acc = 0;\n  for i in 0..10 {\n    acc = acc + i;\n  }\n  acc\n}",
    );
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (g, _) = run_overlays(g);
    let edges = ddf_edges(&g);
    let acc_uses_in_loop: Vec<&Node> = g
        .nodes
        .iter()
        .filter(|n| matches!(&n.kind, NodeKind::Identifier { name } if name == "acc"))
        .filter(|n| n.common.span.start_line == 4)
        .collect();
    assert!(!acc_uses_in_loop.is_empty(), "use of acc on line 4 expected");
    // the use on line 4 must reach at least one def (initial let OR the
    // in-loop assignment — the loop-carried rule allows both)
    for u in &acc_uses_in_loop {
        let reaching: Vec<NodeId> = edges
            .iter()
            .filter(|(_, t, _)| t.id == u.id)
            .map(|(s, _, _)| s.id)
            .collect();
        assert!(!reaching.is_empty(), "acc use at line 4 has no reaching def");
    }
    // and the assignment's def node (assignment expression) is among the
    // defs of the *later* uses, proving the loop-carried edge exists
    let acc_assign = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Expression { expr_kind, .. } if expr_kind == "assignment")
            && n.common.span.start_line == 4
    });
    // the RHS use of acc on line 4 (`acc + i`) reaches BOTH the initial let
    // and the in-loop assignment — the latter textually after the use, linked
    // only by the loop-carried rule.
    let rhs_use = g
        .nodes
        .iter()
        .find(|n| {
            matches!(&n.kind, NodeKind::Identifier { name } if name == "acc")
                && n.common.span.start_line == 4
                && n.common.span.start_byte > acc_assign.common.span.start_byte
        })
        .expect("RHS acc use inside loop");
    let reaching: Vec<NodeId> = edges
        .iter()
        .filter(|(_, t, _)| t.id == rhs_use.id)
        .map(|(s, _, _)| s.id)
        .collect();
    assert!(
        reaching.contains(&acc_assign.id),
        "acc use inside the loop must be loop-carried-reachable from the in-loop assignment"
    );
    // and the initial let also reaches (it dominates loop entry)
    let acc_let = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Declaration { name, .. } if name == "acc")
    });
    assert!(
        reaching.contains(&acc_let.id),
        "initial let acc must also reach the in-loop use"
    );
}

#[test]
fn dfg_assignment_creates_def() {
    let src = source_of("fn f() { let mut x = 1; x = 2; use_x(x); }");
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (g, _) = run_overlays(g);
    let edges = ddf_edges(&g);
    let x_assign = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Expression { expr_kind, .. } if expr_kind == "assignment")
    });
    let x_use = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Identifier { name } if name == "x")
    });
    assert!(
        edges.iter().any(|(s, t, v)| s.id == x_assign.id && t.id == x_use.id && *v == "x"),
        "assignment LHS must act as a definition for later uses"
    );
}

#[test]
fn dfg_closure_capture_is_best_effort_linked() {
    // closure body analyzed in the enclosing scope: capture of `n` links
    let src = source_of("fn f() { let n = 3; let cl = |v| v + n; cl(1) }");
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (g, _) = run_overlays(g);
    let edges = ddf_edges(&g);
    let n_def = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Declaration { name, .. } if name == "n")
    });
    // `n` used inside the closure must link back to the outer let
    let uses_of_n: Vec<&Node> = g
        .nodes
        .iter()
        .filter(|n| matches!(&n.kind, NodeKind::Identifier { name } if name == "n"))
        .collect();
    assert_eq!(uses_of_n.len(), 1);
    assert!(
        edges.iter().any(|(s, t, _)| s.id == n_def.id && t.id == uses_of_n[0].id),
        "closure capture of n must link to outer def"
    );
}

#[test]
fn dfg_call_argument_flows_to_parameter() {
    let src = source_of("fn callee(p: u64) -> u64 { p } fn caller() { let a = 5; callee(a); }");
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (g, _) = run_overlays(g);
    let edges = ddf_edges(&g);
    let arg_use = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Identifier { name } if name == "a")
    });
    let param = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Declaration { name, is_parameter: true } if name == "p")
    });
    assert!(
        edges.iter().any(|(_s, t, v)| t.id == param.id && *v == "<arg:0>"),
        "argument must flow to callee parameter"
    );
    assert!(
        edges.iter().any(|(s, t, _)| s.id == arg_use.id && t.id == param.id),
        "argument value chain: a -> p"
    );
}

#[test]
fn dfg_return_flows_to_call_site() {
    let src = source_of("fn callee() -> u64 { return 7; } fn caller() { let r = callee(); r }");
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (g, _) = run_overlays(g);
    let edges = ddf_edges(&g);
    let ret = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Statement { stmt_kind } if stmt_kind == "return")
    });
    let call = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Expression { expr_kind, .. } if expr_kind == "call")
    });
    assert!(
        edges
            .iter()
            .any(|(s, t, v)| s.id == ret.id && t.id == call.id && *v == "<ret>"),
        "return must flow to the call site"
    );
}

// =========================================================================
// 2. Call graph
// =========================================================================

#[test]
fn call_graph_direct_calls_in_sample_crate_resolved() {
    let (g, _, ostats, _) = build_folder_full(Path::new(SAMPLE)).unwrap();
    let edges = call_edges(&g);
    // direct calls in sample-crate main.rs: do_fetch, Dog::new,
    // helper (via closure/method context), deep_fn via path
    let callee_of = |id: NodeId| node_of(&g, id).common.code.clone();
    let resolved: Vec<(String, String)> = edges
        .iter()
        .filter(|(_, _, r)| *r)
        .map(|(s, d, _)| (callee_of(*s), callee_of(*d)))
        .collect();
    // do_fetch is called directly from fetch
    assert!(
        resolved.iter().any(|(_, d)| d.contains("do_fetch")),
        "do_fetch direct call must resolve; got {resolved:?}"
    );
    // Dog::new path call resolves to the inherent impl method
    assert!(
        resolved.iter().any(|(_, d)| d.contains("new")),
        "Dog::new must resolve"
    );
    // deep_fn (defined in utils/deep.rs) is not called in the sample; but the
    // sample's direct calls (do_fetch, Dog::new) and the method call d.speak()
    // must resolve. deep.rs only calls x.wrapping_add (unresolved, std).
    assert!(
        resolved.iter().any(|(_, d)| d.contains("speak")),
        "d.speak() method call must resolve to the Dog impl"
    );
    assert!(ostats.call_edges > 0);
}

#[test]
fn call_graph_cross_file_direct_call_resolves() {
    // main.rs of the synthetic crate calls helper(1, 2), defined in
    // utils/mod.rs — cross-file resolution must find it.
    let dir = std::env::temp_dir().join(format!("cpg-ov-call-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/utils")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"synth\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.rs"),
        "mod utils;\nfn main() { utils::helper(1, 2); }",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/utils/mod.rs"),
        "pub fn helper(a: i32, b: i32) -> i32 { a + b }",
    )
    .unwrap();
    let (g, _, _, _) = build_folder_full(&dir).unwrap();
    let edges = call_edges(&g);
    let helper_def = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::Function { name, .. } if name == "helper")
    });
    assert!(
        edges.iter().any(|(_, d, r)| *d == helper_def.id && *r),
        "utils::helper(...) must resolve to the cross-file definition"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn call_graph_method_calls_resolve_via_inferred_receiver_type() {
    let (g, _, _, _) = build_folder_full(Path::new(SAMPLE)).unwrap();
    let edges = call_edges(&g);
    // `d.speak()` where d = Dog::new(..): resolved to Animal::speak (Dog impl)
    let resolved: Vec<String> = edges
        .iter()
        .filter(|(_, _, r)| *r)
        .map(|(_, d, _)| node_of(&g, *d).common.code.clone())
        .collect();
    // speak is defined in both Dog and Cat impls; d's receiver type is Dog,
    // so resolution must pick the Dog impl (exactly one speak edge is resolved
    // from d.speak — the Cat impl is not called anywhere)
    let speak_resolved = resolved.iter().filter(|c| c.contains("speak")).count();
    assert_eq!(speak_resolved, 1, "exactly one resolved speak edge expected");
}

#[test]
fn call_graph_unresolved_targets_flagged_not_dropped() {
    let (g, _, ostats, _) = build_folder_full(Path::new(SAMPLE)).unwrap();
    let edges = call_edges(&g);
    // sample-crate calls std methods (map.len, to_string, println!, ...)
    // — none of these have in-graph definitions, so they must appear as
    // resolved:false edges to synthetic <unresolved:...> nodes.
    assert!(ostats.call_unresolved > 0, "unresolved calls must be flagged");
    let unresolved_targets: Vec<&Node> = edges
        .iter()
        .filter(|(_, _, r)| !*r)
        .map(|(_, d, _)| node_of(&g, *d))
        .collect();
    assert!(
        unresolved_targets
            .iter()
            .all(|n| matches!(&n.kind, NodeKind::Function { name, .. } if name.starts_with("<unresolved:"))),
        "all unresolved targets must be synthetic <unresolved:...> nodes"
    );
    // specific: println! is a macro with no in-graph definition
    assert!(
        unresolved_targets
            .iter()
            .any(|n| n.common.code.contains("println")),
        "println! must be flagged unresolved"
    );
}

#[test]
fn call_graph_macro_invocation_links_to_definition() {
    let (g, _, _, _) = build_folder_full(Path::new(SAMPLE)).unwrap();
    let edges = call_edges(&g);
    // twice! is defined via macro_rules! in main.rs and invoked twice
    let def = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::MacroInvocation { .. }) && n.common.code.starts_with("macro_rules!")
    });
    let invocations = edges
        .iter()
        .filter(|(s, d, r)| *r && *d == def.id && matches!(&node_of(&g, *s).kind, NodeKind::MacroInvocation { .. }))
        .count();
    assert_eq!(invocations, 2, "twice! invoked twice, both must link to def");
}

#[test]
fn call_graph_trait_dynamic_dispatch_flagged() {
    // call through a variable whose type is not inferrable: method exists in
    // several impls -> ambiguous -> resolved:false but edges present
    let src = source_of(
        "trait T { fn m(&self) -> u8; } struct A; struct B;\nimpl T for A { fn m(&self) -> u8 { 1 } }\nimpl T for B { fn m(&self) -> u8 { 2 } }\nfn f(x: &A) { x.m(); }",
    );
    let (g, _) = parse_file_to_graph("t.rs", &src).unwrap();
    let (g, _) = run_overlays(g);
    let edges = call_edges(&g);
    assert!(!edges.is_empty(), "method call must not be dropped");
    // x's type IS inferrable here (x: &A), so resolved via receiver type
    // truly dynamic case: unknown receiver
    let src2 = source_of("fn f(v: Vec<u8>) { v.len(); }");
    let (g2, _) = parse_file_to_graph("t.rs", &src2).unwrap();
    let (g2, _) = run_overlays(g2);
    let e2 = call_edges(&g2);
    assert_eq!(e2.len(), 1, "unknown-target call kept, not dropped");
    assert!(!e2[0].2, "unknown-target call flagged resolved:false");
}

// =========================================================================
// 3. Module / package dependencies
// =========================================================================

#[test]
fn import_graph_matches_sample_crate_module_structure() {
    let (g, _, _, _) = build_folder_full(Path::new(SAMPLE)).unwrap();
    let deps = mod_dep_edges(&g);
    // actual structure: main.rs declares mod inner (inline) + mod utils;
    // utils/mod.rs declares pub mod deep; so:
    //   main.rs (via mod utils) -> utils/mod.rs
    //   utils/mod.rs -> utils/deep.rs
    //   main.rs has `use std::collections::HashMap` (std: not an edge)
    let paths: Vec<&str> = deps.iter().map(|(_, _, p)| p.as_str()).collect();
    // builder's mod x; links (from the AST layer) must still be present
    assert!(paths.iter().any(|p| p.ends_with("utils/mod.rs")), "mod utils; -> utils/mod.rs");
    assert!(paths.iter().any(|p| p.ends_with("utils/deep.rs")), "mod deep; -> utils/deep.rs");
    // std imports produce no ModuleDependency edge
    assert!(
        !paths.iter().any(|p| p.contains("std")),
        "std use must not create a module dependency edge"
    );
    // every ModuleDependency target is a real File node in the graph
    for (s, d, _) in &deps {
        let src_node = node_of(&g, *s);
        let dst_node = node_of(&g, *d);
        assert!(
            matches!(&dst_node.kind, NodeKind::File { .. }),
            "ModuleDependency target must be a File"
        );
        let _ = src_node;
    }
}

#[test]
fn import_graph_internal_use_resolves_to_file() {
    // utils::helper use in a crate-root-like file resolves to utils/mod.rs
    // (sample-crate does not `use` internal paths, so build a synthetic crate)
    let dir = std::env::temp_dir().join(format!("cpg-ov-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src/utils")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"synth\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1\"\n").unwrap();
    std::fs::write(dir.join("src/main.rs"), "mod utils;\nuse utils::helper;\nuse serde::Serialize;\nfn main() { helper(1, 2); }").unwrap();
    std::fs::write(dir.join("src/utils/mod.rs"), "pub fn helper(a: i32, b: i32) -> i32 { a + b }").unwrap();
    let (g, _, _, _) = build_folder_full(&dir).unwrap();
    let deps = mod_dep_edges(&g);
    let main_file = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::File { path, .. } if path == "src/main.rs")
    });
    let utils_file = find_node(&g, |n| {
        matches!(&n.kind, NodeKind::File { path, .. } if path == "src/utils/mod.rs")
    });
    assert!(
        deps.iter().any(|(s, d, p)| *s == main_file.id && *d == utils_file.id && p == "utils::helper"),
        "use utils::helper must link main.rs -> utils/mod.rs; got {deps:?}"
    );
    // serde (declared in Cargo.toml) -> synthetic <crate:serde> module node
    let crate_node = g
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Module { name } if name == "<crate:serde>"))
        .expect("synthetic crate node for serde");
    assert!(
        deps.iter().any(|(s, d, _)| *s == main_file.id && *d == crate_node.id),
        "use serde::... must link to the synthetic crate node"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn overlay_edges_are_well_formed() {
    let (g, _, _, _) = build_folder_full(Path::new(SAMPLE)).unwrap();
    // no dangling endpoints across ALL overlays
    let ids: std::collections::HashSet<NodeId> = g.nodes.iter().map(|n| n.id).collect();
    for e in &g.edges {
        assert!(ids.contains(&e.src) && ids.contains(&e.dst), "dangling edge {}", e.id);
    }
    // edge ids unique
    let mut eids: Vec<u64> = g.edges.iter().map(|e| e.id).collect();
    eids.sort();
    eids.dedup();
    assert_eq!(eids.len(), g.edges.len(), "edge ids must be unique");
    // AST layer intact: every node reachable from a File root via Ast edges
    let mut parents: std::collections::HashMap<NodeId, NodeId> = Default::default();
    for e in &g.edges {
        if matches!(e.kind, EdgeKind::Ast { .. }) {
            parents.insert(e.dst, e.src);
        }
    }
    for n in &g.nodes {
        let mut cur = n.id;
        let mut hops = 0;
        // synthetic overlay nodes (unresolved callees, external crates) have
        // no span/file and are exempt from AST reachability
        let synthetic = n.common.span.file.is_empty();
        while !synthetic {
            match parents.get(&cur) {
                Some(&p) => {
                    cur = p;
                    hops += 1;
                    assert!(hops < 10_000, "AST parent cycle");
                }
                None => break,
            }
        }
        if !synthetic {
            assert!(
                matches!(&node_of(&g, cur).kind, NodeKind::File { .. }),
                "every non-synthetic node reachable from a File root"
            );
        }
    }
    // round-trip still lossless with overlays present
    let json = cpg_ast::to_json(&g);
    let back = cpg_ast::from_json(&json).unwrap();
    assert_eq!(back.nodes.len(), g.nodes.len());
    assert_eq!(back.edges.len(), g.edges.len());
}

// --- plumbing ------------------------------------------------------------

fn run_overlays(g: Graph) -> (Graph, cpg_ast::overlays::OverlayStats) {
    let mut g = g;
    let stats = cpg_ast::overlays::build_overlays(&mut g, Path::new(".")).unwrap();
    (g, stats)
}
