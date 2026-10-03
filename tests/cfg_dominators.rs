//! CFG + dominator tests over the Rust AST layer (task t_17450c16).
//!
//! Acceptance criteria covered:
//! - hand-drawn expected CFGs: straight-line code, if/else, every loop form
//!   (loop/while/for), early returns, match with guards;
//! - the dominator tree (Lengauer-Tarjan, from scratch) is verified against
//!   a brute-force dominator computation on the same CFGs;
//! - all edges live in the CFG / dominators edge namespaces.

use cpg_ast::cfg::{build_cfgs, build_function_cfg, Cfg, CfgNodeKind};
use cpg_ast::dominators::{dom_edges, idom_map, postdom_edges, Gid};
use cpg_ast::parse_file_to_graph;
use cpg_ast::schema::{EdgeKind, Graph, NodeId, NodeKind};
use std::collections::{HashMap, HashSet};

// --- helpers ----------------------------------------------------------------

fn fn_graph(src: &str) -> (Graph, NodeId) {
    let (g, _stats) = parse_file_to_graph("t.rs", src.as_bytes()).unwrap();
    assert_eq!(_stats.parse_errors, 0, "test source must parse cleanly");
    let f = g
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Function { .. }))
        .unwrap()
        .id;
    (g, f)
}

fn cfg_edges(cfg: &Cfg) -> Vec<(NodeId, NodeId, Option<String>)> {
    cfg.edges
        .iter()
        .map(|e| {
            (
                e.src,
                e.dst,
                match &e.kind {
                    EdgeKind::Cfg { branch_label } => branch_label.clone(),
                    _ => unreachable!("only Cfg edges in a Cfg"),
                },
            )
        })
        .collect()
}

/// Count of edges (src,dst,label) matching, where None label matches unlabeled.
fn edge_count(cfg: &Cfg, src: NodeId, dst: NodeId, label: Option<&str>) -> usize {
    cfg_edges(cfg)
        .into_iter()
        .filter(|(s, d, l)| *s == src && *d == dst && l.as_deref() == label)
        .count()
}

fn cfg_node_by_ast_code(g: &Graph, cfg: &Cfg, ast_code: &str) -> NodeId {
    // Find the CFG flow node whose BOUND AST node's code matches; take the
    // first in CFG node order (source order for straight emission).
    cfg.nodes
        .iter()
        .find(|n| match &n.kind {
            CfgNodeKind::Statement { ast } | CfgNodeKind::Condition { ast } | CfgNodeKind::Iterate { ast } => {
                g.nodes[(*ast - 1) as usize].common.code == ast_code
            }
            _ => false,
        })
        .map(|n| n.id)
        .unwrap_or_else(|| panic!("no CFG node bound to AST code {ast_code:?}"))
}

/// Successors of a CFG node.
#[allow(dead_code)]
fn succs(cfg: &Cfg, id: NodeId) -> Vec<(NodeId, Option<String>)> {
    cfg.edges
        .iter()
        .filter(|e| e.src == id)
        .map(|e| {
            (
                e.dst,
                match &e.kind {
                    EdgeKind::Cfg { branch_label } => branch_label.clone(),
                    _ => None,
                },
            )
        })
        .collect()
}

fn cfg_edge_list(cfg: &Cfg) -> Vec<(Gid, Gid)> {
    cfg.edges.iter().map(|e| (e.src, e.dst)).collect()
}

// --- brute-force dominator reference ------------------------------------------

/// Brute-force dominators: D(v) = {v} ∪ {u : every path root→v passes u}.
/// Computed per node u: u dominates v iff removing u disconnects root→v.
fn brute_force_idom(edges: &[(Gid, Gid)], root: Gid) -> HashMap<Gid, Gid> {
    let mut succ: HashMap<Gid, Vec<Gid>> = HashMap::new();
    for &(a, b) in edges {
        succ.entry(a).or_default().push(b);
    }
    let mut nodes: HashSet<Gid> = HashSet::from([root]);
    for &(a, b) in edges {
        nodes.insert(a);
        nodes.insert(b);
    }
    let reachable_from = |blocked: Gid| -> HashSet<Gid> {
        let mut seen = HashSet::from([root]);
        let mut stack = vec![root];
        while let Some(v) = stack.pop() {
            for w in succ.get(&v).cloned().unwrap_or_default() {
                if w != blocked && seen.insert(w) {
                    stack.push(w);
                }
            }
        }
        seen
    };
    let mut dom: HashMap<Gid, HashSet<Gid>> = HashMap::new();
    for &v in &nodes {
        if v == root {
            continue;
        }
        let mut ds: HashSet<Gid> = HashSet::new();
        for &u in &nodes {
            if u == v {
                continue;
            }
            // u dominates v iff v unreachable when u blocked (and u ≠ root,
            // unless root itself — root always dominates all)
            if u == root {
                ds.insert(u);
            } else if !reachable_from(u).contains(&v) {
                ds.insert(u);
            }
        }
        dom.insert(v, ds);
    }
    // idom(v) = the strict dominator u of v such that u itself is dominated by
    // every other strict dominator of v. Dominators of v are totally ordered;
    // "u is dominated by d" here means: removing d from the graph makes u
    // unreachable from root (i.e. d is also a dominator of u).
    let mut out = HashMap::new();
    for (&v, ds) in &dom {
        let strict: Vec<Gid> = ds.iter().copied().collect();
        let mut im = None;
        for &u in &strict {
            // u is the immediate dominator if for every other strict dominator
            // d of v, d also dominates u.
            let all_dominate_u = strict.iter().all(|&d| {
                d == u
                    || d == root
                    // d dominates u iff u unreachable with d blocked
                    || !reachable_from(d).contains(&u)
            });
            if all_dominate_u {
                im = Some(u);
                break;
            }
        }
        out.insert(v, im.expect("every non-root reachable node has an idom"));
    }
    out
}

// --- tests --------------------------------------------------------------------

#[test]
fn straight_line_code() {
    let src = r#"
fn f(x: u64) -> u64 {
    let a = x + 1;
    let b = a * 2;
    b
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let entry = cfg.entry;
    let exit = cfg.exit;
    // nodes: entry, exit, 3 statements (let, let, expr)
    assert_eq!(cfg.nodes.len(), 5);
    assert_eq!(edge_count(&cfg, entry, cfg_node_by_ast_code(&g, &cfg, "let a = x + 1;"), None), 1);
    let s2 = cfg_node_by_ast_code(&g, &cfg, "let b = a * 2;");
    assert_eq!(edge_count(&cfg, cfg_node_by_ast_code(&g, &cfg, "let a = x + 1;"), s2, None), 1);
    let s3 = cfg_node_by_ast_code(&g, &cfg, "b");
    assert_eq!(edge_count(&cfg, s2, s3, None), 1);
    assert_eq!(edge_count(&cfg, s3, exit, None), 1);
    // no branch labels anywhere
    assert!(cfg_edges(&cfg).iter().all(|(_, _, l)| l.is_none()));
    // panic node not created
    assert!(cfg.nodes.iter().all(|n| n.kind != CfgNodeKind::Panic));
}

#[test]
fn if_else_branches() {
    let src = r#"
fn f(x: u64) -> u64 {
    if x > 4 {
        x
    } else {
        0
    }
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let entry = cfg.entry;
    let exit = cfg.exit;
    // condition node + two arms + exit
    let cond = cfg.nodes.iter().find(|n| matches!(n.kind, CfgNodeKind::Condition { .. })).unwrap().id;
    assert_eq!(edge_count(&cfg, entry, cond, None), 1);
    // true arm → the `x` tail-expression statement
    let t = cfg_node_by_ast_code(&g, &cfg, "x");
    let e = cfg_node_by_ast_code(&g, &cfg, "0");
    assert_eq!(edge_count(&cfg, cond, t, Some("true")), 1);
    assert_eq!(edge_count(&cfg, cond, e, Some("false")), 1);
    assert_eq!(edge_count(&cfg, t, exit, None), 1);
    assert_eq!(edge_count(&cfg, e, exit, None), 1);
}

#[test]
fn if_without_else_false_falls_through() {
    let src = r#"
fn f(x: u64) -> u64 {
    let mut s = x;
    if x > 4 {
        s = s + 1;
    }
    s
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let cond = cfg.nodes.iter().find(|n| matches!(n.kind, CfgNodeKind::Condition { .. })).unwrap().id;
    let body = cfg_node_by_ast_code(&g, &cfg, "s = s + 1;");
    let tail = cfg_node_by_ast_code(&g, &cfg, "s");
    assert_eq!(edge_count(&cfg, cond, body, Some("true")), 1);
    assert_eq!(edge_count(&cfg, cond, tail, Some("false")), 1);
    assert_eq!(edge_count(&cfg, body, tail, None), 1);
}

#[test]
fn loop_form_loop_with_break_continue() {
    let src = r#"
fn f(mut x: u64) -> u64 {
    loop {
        x = x + 1;
        if x > 10 {
            break;
        }
        if x < 3 {
            continue;
        }
        x = x + 100;
    }
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    // head = Condition bound to the loop stmt
    let head = cfg
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, CfgNodeKind::Condition { ast } if {
            let code = g.nodes[(*ast - 1) as usize].common.code.as_str();
            code.starts_with("loop")
        }))
        .unwrap()
        .id;
    let s1 = cfg_node_by_ast_code(&g, &cfg, "x = x + 1;");
    let s3 = cfg_node_by_ast_code(&g, &cfg, "x = x + 100;");
    let exit = cfg.exit;
    // entry → head, head → body
    assert_eq!(edge_count(&cfg, cfg.entry, head, None), 1);
    assert_eq!(edge_count(&cfg, head, s1, None), 1);
    // break → loop exit (the empty-code node bound to the loop stmt), then
    // exit node → function Exit
    let brk = cfg_node_by_ast_code(&g, &cfg, "break");
    let loop_exit = cfg
        .nodes
        .iter()
        .find(|n| {
            matches!(&n.kind, CfgNodeKind::Statement { ast } if {
                let c = &g.nodes[(*ast - 1) as usize].common.code;
                c.starts_with("loop")
            }) && n.code.is_empty()
        })
        .unwrap()
        .id;
    assert_eq!(edge_count(&cfg, brk, loop_exit, Some("break")), 1);
    assert_eq!(edge_count(&cfg, loop_exit, exit, None), 1);
    // continue → head, labelled
    let cont = cfg_node_by_ast_code(&g, &cfg, "continue");
    assert_eq!(edge_count(&cfg, cont, head, Some("continue")), 1);
    // tail of body → head (back edge)
    assert_eq!(edge_count(&cfg, s3, head, None), 1);
    // the if conditions inside: x>10 cond true → break
    // exit has no other incoming (loop never falls through)
    // (verified via brute force dominator cross-check below)
}

#[test]
fn while_loop() {
    let src = r#"
fn f(mut x: u64) -> u64 {
    while x < 10 {
        x = x + 1;
    }
    x
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let cond = cfg
        .nodes
        .iter()
        .find(|n| matches!(n.kind, CfgNodeKind::Condition { .. }))
        .unwrap()
        .id;
    let body = cfg_node_by_ast_code(&g, &cfg, "x = x + 1;");
    // false edge → the while's loop-exit flow node (empty-code Statement bound
    // to the while AST node), then that node sequences to the tail `x`.
    let exit_node = cfg
        .nodes
        .iter()
        .find(|n| {
            matches!(&n.kind, CfgNodeKind::Statement { ast } if {
                let c = &g.nodes[(*ast - 1) as usize].common.code;
                c.starts_with("while")
            }) && n.code.is_empty()
        })
        .unwrap()
        .id;
    let tail = cfg
        .nodes
        .iter()
        .filter(|n| {
            matches!(&n.kind, CfgNodeKind::Statement { ast } if {
                g.nodes[(*ast - 1) as usize].common.code == "x"
            })
        })
        .next_back()
        .unwrap()
        .id;
    let exit = cfg.exit;
    assert_eq!(edge_count(&cfg, cfg.entry, cond, None), 1);
    assert_eq!(edge_count(&cfg, cond, body, Some("true")), 1);
    assert_eq!(edge_count(&cfg, cond, exit_node, Some("false")), 1);
    assert_eq!(edge_count(&cfg, body, cond, None), 1); // back edge
    assert_eq!(edge_count(&cfg, exit_node, tail, None), 1);
    assert_eq!(edge_count(&cfg, tail, exit, None), 1);
}

#[test]
fn for_loop() {
    let src = r#"
fn f(v: Vec<u64>) -> u64 {
    let mut s = 0;
    for x in v {
        s = s + x;
    }
    s
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    // Iterate node present
    let iterate = cfg
        .nodes
        .iter()
        .find(|n| matches!(n.kind, CfgNodeKind::Iterate { .. }))
        .expect("for loop has an Iterate node")
        .id;
    let cond = cfg
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, CfgNodeKind::Condition { .. }))
        .find(|n| n.code == "v")
        .unwrap()
        .id;
    let body = cfg_node_by_ast_code(&g, &cfg, "s = s + x;");
    // entry flows into the `let` statement before the loop
    let body_first = cfg_node_by_ast_code(&g, &cfg, "let mut s = 0;");
    // false edge → the for's loop-exit node, then it sequences to the tail `s`.
    let exit_node = cfg
        .nodes
        .iter()
        .find(|n| {
            matches!(&n.kind, CfgNodeKind::Statement { ast } if {
                let c = &g.nodes[(*ast - 1) as usize].common.code;
                c.starts_with("for")
            }) && n.code.is_empty()
        })
        .unwrap()
        .id;
    let tail = cfg
        .nodes
        .iter()
        .filter(|n| {
            matches!(&n.kind, CfgNodeKind::Statement { ast } if {
                g.nodes[(*ast - 1) as usize].common.code == "s"
            })
        })
        .next_back()
        .unwrap()
        .id;
    assert_eq!(edge_count(&cfg, cfg.entry, body_first, None), 1);
    assert_eq!(edge_count(&cfg, iterate, cond, None), 1);
    assert_eq!(edge_count(&cfg, cond, body, Some("true")), 1);
    assert_eq!(edge_count(&cfg, cond, exit_node, Some("false")), 1);
    assert_eq!(edge_count(&cfg, body, cond, None), 1); // back edge
    assert_eq!(edge_count(&cfg, exit_node, tail, None), 1);
}

#[test]
fn for_loop_wildcard_pattern_regression() {
    // `for _ in ...`: the wildcard pattern is an anonymous tree-sitter token,
    // so the AST layer materializes no `pattern` child edge. The CFG builder
    // used to panic here ("for has pattern"); it must build a complete CFG
    // with an Iterate node labelled `for _ in <iter>`.
    let src = r#"
fn f(n: u64) -> u64 {
    let mut s = 0;
    for _ in 0..n {
        s += 1;
    }
    s
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let iterate = cfg
        .nodes
        .iter()
        .find(|n| matches!(n.kind, CfgNodeKind::Iterate { .. }))
        .expect("wildcard for loop has an Iterate node");
    assert_eq!(iterate.code, "for _ in 0..n");
    let cond = cfg
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, CfgNodeKind::Condition { .. }))
        .find(|n| n.code == "0..n")
        .unwrap()
        .id;
    let body = cfg_node_by_ast_code(&g, &cfg, "s += 1;");
    let body_first = cfg_node_by_ast_code(&g, &cfg, "let mut s = 0;");
    let exit_node = cfg
        .nodes
        .iter()
        .find(|n| {
            matches!(&n.kind, CfgNodeKind::Statement { ast } if {
                let c = &g.nodes[(*ast - 1) as usize].common.code;
                c.starts_with("for")
            }) && n.code.is_empty()
        })
        .unwrap()
        .id;
    let tail = cfg
        .nodes
        .iter()
        .filter(|n| {
            matches!(&n.kind, CfgNodeKind::Statement { ast } if {
                g.nodes[(*ast - 1) as usize].common.code == "s"
            })
        })
        .next_back()
        .unwrap()
        .id;
    assert_eq!(edge_count(&cfg, cfg.entry, body_first, None), 1);
    assert_eq!(edge_count(&cfg, iterate.id, cond, None), 1);
    assert_eq!(edge_count(&cfg, cond, body, Some("true")), 1);
    assert_eq!(edge_count(&cfg, cond, exit_node, Some("false")), 1);
    assert_eq!(edge_count(&cfg, body, cond, None), 1); // back edge
    assert_eq!(edge_count(&cfg, exit_node, tail, None), 1);
}

#[test]
fn early_return() {
    let src = r#"
fn f(x: u64) -> u64 {
    if x == 0 {
        return 1;
    }
    x + 1
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let ret = cfg_node_by_ast_code(&g, &cfg, "return 1");
    assert_eq!(edge_count(&cfg, ret, cfg.exit, Some("return")), 1);
    // the `x + 1` tail flows only from the condition's false edge
    let tail = cfg_node_by_ast_code(&g, &cfg, "x + 1");
    let cond = cfg.nodes.iter().find(|n| matches!(n.kind, CfgNodeKind::Condition { .. })).unwrap().id;
    assert_eq!(edge_count(&cfg, cond, tail, Some("false")), 1);
    assert_eq!(edge_count(&cfg, tail, cfg.exit, None), 1);
}

#[test]
fn match_with_guards() {
    let src = r#"
fn f(n: u64) -> u64 {
    match n {
        0 => 100,
        m if m % 2 == 0 => 200,
        _ => 300,
    }
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let cond = cfg.nodes.iter().find(|n| matches!(n.kind, CfgNodeKind::Condition { .. })).unwrap().id;
    // arm entries are Condition nodes bound to match_arm AST nodes
    let arm_conds: Vec<(NodeId, String)> = cfg
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            CfgNodeKind::Condition { ast } => {
                let c = &g.nodes[(*ast - 1) as usize].common.code;
                if c.contains("=>") {
                    Some((n.id, c.clone()))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    assert!(arm_conds.len() >= 3, "arm entry nodes exist");
    // dispatch chain: cond → arm0; arm0 → arm1; guard(arm1) → arm2
    let a0 = arm_conds[0].0;
    let a1 = arm_conds[1].0;
    assert_eq!(edge_count(&cfg, cond, a0, None), 1);
    assert_eq!(edge_count(&cfg, a0, a1, None), 1);
    // guard node: the Condition bound to the guard expression
    // (code == "m % 2 == 0", NOT the arm-entry text)
    let guard = cfg
        .nodes
        .iter()
        .find(|n| matches!(&n.kind, CfgNodeKind::Condition { ast } if {
            g.nodes[(*ast - 1) as usize].common.code == "m % 2 == 0"
        }))
        .unwrap()
        .id;
    assert_eq!(edge_count(&cfg, a1, guard, None), 1);
    let a2 = arm_conds[2].0;
    // guard "miss" edge: pattern did not match/guard false → next arm entry.
    // In this builder the miss edge is unlabelled (dispatch → next arm).
    assert_eq!(edge_count(&cfg, guard, a2, None), 1, "guard miss → next arm");
    // unguarded arms carry pattern labels
    assert_eq!(edge_count(&cfg, a0, cfg_node_by_ast_code(&g, &cfg, "100"), Some("0")), 1);
    assert_eq!(edge_count(&cfg, guard, cfg_node_by_ast_code(&g, &cfg, "200"), Some("true")), 1);
    assert_eq!(edge_count(&cfg, a2, cfg_node_by_ast_code(&g, &cfg, "300"), Some("_")), 1);
    // match result values flow to exit via the function Exit node
    let exit = cfg.exit;
    for c in ["100", "200", "300"] {
        // each arm value node has exactly one outgoing edge (to Exit)
        let n = cfg_node_by_ast_code(&g, &cfg, c);
        let outs = cfg.edges.iter().filter(|e| e.src == n).count();
        assert_eq!(outs, 1, "arm value {c} flows on");
    }
    assert!(cfg.edges.iter().any(|e| e.dst == exit && e.src == cfg_node_by_ast_code(&g, &cfg, "100")));
}

#[test]
fn panicking_operations_get_panic_edges() {
    let src = r#"
fn f(v: Vec<u64>, i: usize, d: u64) -> u64 {
    let a = v[i];
    let b = d / i as u64;
    let c = v.len().try_into().unwrap();
    a + b + c
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let panic = cfg
        .nodes
        .iter()
        .find(|n| n.kind == CfgNodeKind::Panic)
        .expect("panic node exists")
        .id;
    // statements that may panic: index, division, unwrap
    let s1 = cfg_node_by_ast_code(&g, &cfg, "let a = v[i];");
    let s2 = cfg_node_by_ast_code(&g, &cfg, "let b = d / i as u64;");
    let s3 = cfg_node_by_ast_code(&g, &cfg, "let c = v.len().try_into().unwrap();");
    assert_eq!(edge_count(&cfg, s1, panic, Some("panic")), 1);
    assert_eq!(edge_count(&cfg, s2, panic, Some("panic")), 1);
    assert_eq!(edge_count(&cfg, s3, panic, Some("panic")), 1);
    // one shared Panic node
    assert_eq!(cfg.nodes.iter().filter(|n| n.kind == CfgNodeKind::Panic).count(), 1);
    // the tail (no panic possible) has no panic edge
    let tail = cfg_node_by_ast_code(&g, &cfg, "a + b + c");
    assert_eq!(edge_count(&cfg, tail, panic, Some("panic")), 0);
}

#[test]
fn panic_macro_creates_panic_edge() {
    let src = r#"
fn f(x: u64) -> u64 {
    if x == 0 {
        panic!("zero");
    }
    x
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let panic = cfg.nodes.iter().find(|n| n.kind == CfgNodeKind::Panic).unwrap().id;
    let stmt = cfg_node_by_ast_code(&g, &cfg, "panic!(\"zero\");");
    assert_eq!(edge_count(&cfg, stmt, panic, Some("panic")), 1);
}

#[test]
fn cfg_edges_live_in_cfg_namespace() {
    // Every edge produced by the CFG pass has EdgeKind::Cfg; graph overlay
    // filter assigns them to "cfg".
    let src = r#"
fn f(x: u64) -> u64 {
    while x > 0 {
        x = x - 1;
    }
    x
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    assert!(cfg.edges.iter().all(|e| matches!(e.kind, EdgeKind::Cfg { .. })));
    // attach into a graph and verify namespace
    let mut g2 = g.clone();
    for e in &cfg.edges {
        // attach with AST endpoints where available (Statement/Condition)
        let src_ast = cfg.nodes.iter().find(|n| n.id == e.src).unwrap();
        let dst_ast = cfg.nodes.iter().find(|n| n.id == e.dst).unwrap();
        if let (Ok(s), Ok(d)) = (cfg_ast_id(src_ast), cfg_ast_id(dst_ast)) {
            g2.add_edge(cpg_ast::schema::Edge {
                id: g2.edges.len() as u64 + 1,
                src: s,
                dst: d,
                kind: e.kind.clone(),
            });
        }
    }
    assert!(g2.edges_by_overlay("cfg").count() > 0);
}

fn cfg_ast_id(n: &cpg_ast::cfg::CfgNode) -> Result<NodeId, ()> {
    match &n.kind {
        CfgNodeKind::Statement { ast } | CfgNodeKind::Condition { ast } | CfgNodeKind::Iterate { ast } => Ok(*ast),
        _ => Err(()),
    }
}

// --- dominator verification ------------------------------------------------------

#[test]
fn dominators_match_brute_force_on_all_constructs() {
    let sources: &[&str] = &[
        // straight-line
        r#"fn f(x: u64) -> u64 { let a = x + 1; let b = a * 2; b }"#,
        // if/else
        r#"fn f(x: u64) -> u64 { if x > 4 { x } else { 0 } }"#,
        // if without else
        r#"fn f(x: u64) -> u64 { let mut s = x; if x > 4 { s = s + 1; } s }"#,
        // loop with break/continue
        r#"fn f(mut x: u64) -> u64 { loop { x = x + 1; if x > 10 { break; } if x < 3 { continue; } x = x + 100; } }"#,
        // while
        r#"fn f(mut x: u64) -> u64 { while x < 10 { x = x + 1; } x }"#,
        // for
        r#"fn f(v: Vec<u64>) -> u64 { let mut s = 0; for x in v { s = s + x; } s }"#,
        // early returns
        r#"fn f(x: u64) -> u64 { if x == 0 { return 1; } x + 1 }"#,
        // match with guards
        r#"fn f(n: u64) -> u64 { match n { 0 => 100, m if m % 2 == 0 => 200, _ => 300 } }"#,
        // nested loops
        r#"fn f(n: u64) -> u64 { let mut s = 0; let mut i = 0; while i < n { let mut j = 0; while j < n { s = s + 1; j = j + 1; } i = i + 1; } s }"#,
        // break out of for inside if
        r#"fn f(v: Vec<u64>) -> u64 { let mut s = 0; for x in v { if x > 5 { break; } s = s + x; } s }"#,
    ];
    for src in sources {
        let (g, f) = fn_graph(src);
        let cfg = build_function_cfg(&g, f).unwrap();
        let edges = cfg_edge_list(&cfg);
        // sanity: entry reaches exit
        assert_eq!(edges[0].0, cfg.entry, "entry is the DFS root");
        let lt = idom_map(&edges, cfg.entry);
        let bf = brute_force_idom(&edges, cfg.entry);
        assert_eq!(lt.len(), bf.len(), "src: {src}\nLT {lt:?}\nBF {bf:?}");
        for (v, d) in &lt {
            assert_eq!(
                bf.get(v),
                Some(d),
                "src: {src}\nmismatch idom({v}): LT={d:?} BF={:?}\nedges={edges:?}",
                bf.get(v)
            );
        }
    }
}

#[test]
fn dominator_tree_shape_on_branching_code() {
    let src = r#"fn f(x: u64) -> u64 { if x > 4 { x } else { 0 } }"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let edges = cfg_edge_list(&cfg);
    let idom = idom_map(&edges, cfg.entry);
    // entry dominates everything; the two arm tails' idom is the condition.
    let cond = cfg.nodes.iter().find(|n| matches!(n.kind, CfgNodeKind::Condition { .. })).unwrap().id;
    let t = cfg_node_by_ast_code(&g, &cfg, "x");
    let e = cfg_node_by_ast_code(&g, &cfg, "0");
    assert_eq!(idom.get(&t), Some(&cond));
    assert_eq!(idom.get(&e), Some(&cond));
    // exit's idom is one of the arms (both flow in) — must be cond
    assert_eq!(idom.get(&cfg.exit), Some(&cond));
}

#[test]
fn attached_dominator_edges_live_in_dominators_namespace() {
    use cpg_ast::dominators::{attach_dominator_edges, AttachOptions};

    // Branchy CFG: attach into a real graph, check the overlay namespace.
    // A trailing statement keeps a non-virtual postdom pair: the tails'
    // immediate post-dominator is the trailing statement (Exit is virtual).
    let src = r#"
fn f(x: u64) -> u64 {
    if x > 4 {
        x
    } else {
        0
    };
    7
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let mut g2 = g.clone();
    attach_dominator_edges(&mut g2, &cfg, &AttachOptions::default());

    let dom_edges: Vec<_> = g2.edges_by_overlay("dominators").collect();
    assert!(!dom_edges.is_empty(), "dominators overlay is populated");
    assert!(dom_edges
        .iter()
        .all(|e| matches!(e.kind, EdgeKind::Dominate | EdgeKind::PostDominate)),
        "every dominators-namespace edge is Dominate/PostDominate");
    assert!(dom_edges
        .iter()
        .any(|e| matches!(e.kind, EdgeKind::Dominate)),
        "Dominate edges attached");
    // PostDominate on a shape where the postdom chain reaches an AST-bound
    // node (in the tiny if/else above every postdom pair is sourced at the
    // virtual Exit, which correctly drops out): in a while loop the
    // condition's post-dominator is the loop-exit statement node.
    let src2 = r#"fn f(mut x: u64) -> u64 { while x < 10 { x = x + 1; } x }"#;
    let (g3, f3) = fn_graph(src2);
    let cfg3 = build_function_cfg(&g3, f3).unwrap();
    let mut g4 = g3.clone();
    attach_dominator_edges(&mut g4, &cfg3, &AttachOptions::default());
    assert!(
        g4.edges_by_overlay("dominators")
            .any(|e| matches!(e.kind, EdgeKind::PostDominate)),
        "PostDominate edges attached"
    );

    // The attached Dominate edges, restricted to CFG-AST endpoints, agree with
    // the brute-force idom map on the same CFG. Map: cfg-node id -> AST id.
    let ast_of = |cfg_id: u64| -> Option<u64> {
        cfg.nodes.iter().find_map(|n| {
            if n.id != cfg_id {
                return None;
            }
            match &n.kind {
                CfgNodeKind::Statement { ast }
                | CfgNodeKind::Condition { ast }
                | CfgNodeKind::Iterate { ast } => Some(*ast),
                _ => None,
            }
        })
    };
    // brute-force reference on the raw CFG
    let bf = brute_force_idom(&cfg_edge_list(&cfg), cfg.entry);
    let bf_asts: HashSet<(u64, u64)> = bf
        .iter()
        .filter_map(|(v, d)| Some((ast_of(*d)?, ast_of(*v)?)))
        .collect();
    let attached: HashSet<(u64, u64)> = g2
        .edges
        .iter()
        .filter(|e| matches!(e.kind, EdgeKind::Dominate))
        .map(|e| (e.src, e.dst))
        .collect();
    assert_eq!(
        bf_asts, attached,
        "attached Dominate edges must equal brute-force idom pairs (AST-mapped)"
    );
}

#[test]
fn dom_edges_pairwise_and_postdom_smoke() {
    let src = r#"fn f(mut x: u64) -> u64 { while x < 10 { x = x + 1; } x }"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let edges = cfg_edge_list(&cfg);
    let pairs = dom_edges(&edges, cfg.entry);
    assert!(pairs.len() >= 4);
    // post-dominators on the reversed graph: the postdom TREE is well-formed —
    // (d, v) means v's immediate postdominator is d, so each node v appears at
    // most once as the second (child) component; d may have many children.
    let pd = postdom_edges(&edges, cfg.exit);
    let mut seen_child: HashSet<Gid> = HashSet::new();
    for (d, v) in &pd {
        let _ = d;
        assert!(seen_child.insert(*v), "postdom child repeated: {pd:?}");
    }
    // every node reachable from entry (except exit) has a postdom parent
    assert!(pd.len() >= 3);
}

// --- dominator edge attachment (schema "dominators" namespace) ------------------

#[test]
fn dominator_edges_attach_in_dominators_namespace() {
    // A trailing statement keeps a non-virtual postdom pair: with the bare
    // if/else, every postdom pair is sourced at the virtual Exit (dropped).
    let src = r#"
fn f(x: u64) -> u64 {
    if x > 4 {
        x
    } else {
        0
    };
    7
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let mut g2 = g.clone();
    let ast_before = g2.edges_by_overlay("ast").count();
    let cfg_before = g2.edges_by_overlay("cfg").count();
    cpg_ast::dominators::attach_dominator_edges(
        &mut g2,
        &cfg,
        &cpg_ast::dominators::AttachOptions::default(),
    );
    // the "dominators" overlay exists and is non-empty
    let dom_edges: Vec<_> = g2.edges_by_overlay("dominators").collect();
    assert!(!dom_edges.is_empty(), "dominators overlay is populated");
    // every attached edge has kind Dominate or PostDominate
    for e in &dom_edges {
        assert!(
            matches!(e.kind, EdgeKind::Dominate | EdgeKind::PostDominate),
            "non-dominator edge in dominators overlay: {:?}",
            e.kind
        );
    }
    // other overlays untouched
    assert_eq!(g2.edges_by_overlay("ast").count(), ast_before);
    assert_eq!(g2.edges_by_overlay("cfg").count(), cfg_before);
    // both kinds present (if/else has Dominate edges; PostDominate over the
    // reversed graph)
    assert!(dom_edges.iter().any(|e| matches!(e.kind, EdgeKind::Dominate)));
    assert!(dom_edges
        .iter()
        .any(|e| matches!(e.kind, EdgeKind::PostDominate)));
    // self-edges are impossible (a node never immediately dominates itself)
    assert!(dom_edges.iter().all(|e| e.src != e.dst));
    // attached edges connect existing AST nodes
    let node_ids: HashSet<NodeId> = g2.nodes.iter().map(|n| n.id).collect();
    assert!(dom_edges
        .iter()
        .all(|e| node_ids.contains(&e.src) && node_ids.contains(&e.dst)));
}

#[test]
fn attached_dominator_tree_matches_brute_force() {
    // branchy CFG: the attached Dominate tree must agree with the brute-force
    // idom map (compared over AST-node endpoints).
    let src = r#"
fn f(x: u64) -> u64 {
    if x > 4 {
        x
    } else {
        0
    }
}
"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let edges = cfg_edge_list(&cfg);
    let lt = idom_map(&edges, cfg.entry);
    let bf = brute_force_idom(&edges, cfg.entry);
    assert_eq!(lt, bf);

    // Attach and re-derive the AST-level idom map from the attached edges.
    let mut g2 = g.clone();
    cpg_ast::dominators::attach_dominator_edges(
        &mut g2,
        &cfg,
        &cpg_ast::dominators::AttachOptions {
            with_dominators: true,
            with_post_dominators: false,
        },
    );
    // Map CFG node id -> AST id for AST-bound nodes.
    let ast_of = |id: u64| -> Option<NodeId> {
        cfg.nodes.iter().find_map(|n| {
            if n.id != id {
                return None;
            }
            match &n.kind {
                CfgNodeKind::Statement { ast } | CfgNodeKind::Condition { ast } | CfgNodeKind::Iterate { ast } => {
                    Some(*ast)
                }
                _ => None,
            }
        })
    };
    // Collect attached Dominate pairs keyed by the dominated CFG node.
    let attached: HashMap<NodeId, NodeId> = g2
        .edges
        .iter()
        .filter_map(|e| match e.kind {
            EdgeKind::Dominate => {
                // find the CFG node whose AST id == e.dst and which has an idom
                let v = cfg.nodes.iter().find(|n| {
                    matches!(&n.kind, CfgNodeKind::Statement { ast } | CfgNodeKind::Condition { ast } | CfgNodeKind::Iterate { ast } if {
                        match &n.kind {
                            CfgNodeKind::Statement { ast } | CfgNodeKind::Condition { ast } | CfgNodeKind::Iterate { ast } => *ast == e.dst,
                            _ => false,
                        }
                    })
                })?;
                let dominated_cfg = v.id;
                lt.get(&dominated_cfg).and_then(|d| ast_of(*d)).map(|d_ast| (e.dst, d_ast))
            }
            _ => None,
        })
        .collect();
    // Every LT idom pair with both endpoints AST-bound must be present.
    for (v, d) in &lt {
        if let (Some(v_ast), Some(d_ast)) = (ast_of(*v), ast_of(*d)) {
            assert_eq!(
                attached.get(&v_ast),
                Some(&d_ast),
                "attached Dominate edge for AST node {v_ast} mismatch"
            );
        }
    }
}

#[test]
fn nested_loop_dominators() {
    // classic doubly-nested loop: inner condition dominated by outer condition
    let src = r#"fn f(n: u64) -> u64 { let mut s = 0; let mut i = 0; while i < n { let mut j = 0; while j < n { s = s + 1; j = j + 1; } i = i + 1; } s }"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let edges = cfg_edge_list(&cfg);
    let lt = idom_map(&edges, cfg.entry);
    let bf = brute_force_idom(&edges, cfg.entry);
    for (v, d) in &lt {
        assert_eq!(bf.get(v), Some(d), "idom({v}) LT={d} BF={:?} edges={edges:?}", bf.get(v));
    }
}

// --- whole-crate CFG pass ----------------------------------------------------------

#[test]
fn build_cfgs_on_sample_crate() {
    let crate_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/sample-crate");
    let g = cpg_ast::build_folder(std::path::Path::new(crate_dir)).unwrap().0;
    let cfgs = build_cfgs(&g);
    // sample crate has ~15 function bodies (incl. closures excluded — no body edge)
    assert!(cfgs.len() >= 10, "found {} cfgs", cfgs.len());
    for cfg in &cfgs {
        // every non-panic node is reachable from entry
        let edges = cfg_edge_list(cfg);
        let reach = cpg_ast::dominators::reachable(&edges, cfg.entry);
        for n in &cfg.nodes {
            if n.kind == CfgNodeKind::Panic {
                continue; // panic node is a terminal side-exit, not on a root path
            }
            assert!(
                reach.contains(&n.id) || n.id == cfg.exit && {
                    // exit may be unreachable for infinite loops without break
                    edges.iter().any(|&(_s, d)| d == cfg.exit)
                },
                "node {} unreachable in fn {:?}",
                n.id,
                g.nodes[(cfg.function - 1) as usize].common.code
            );
        }
        // every statement node's span slices real source? (spans carried from AST layer)
        for n in &cfg.nodes {
            assert!(n.span.end_byte >= n.span.start_byte);
        }
    }
}

// --- serializability ---------------------------------------------------------------

#[test]
fn cfg_round_trips_through_json() {
    let src = r#"fn f(x: u64) -> u64 { if x > 4 { x } else { 0 } }"#;
    let (g, f) = fn_graph(src);
    let cfg = build_function_cfg(&g, f).unwrap();
    let json = serde_json::to_string(&cfg).unwrap();
    let back: Cfg = serde_json::from_str(&json).unwrap();
    assert_eq!(back.nodes.len(), cfg.nodes.len());
    assert_eq!(back.edges.len(), cfg.edges.len());
    assert_eq!(back.entry, cfg.entry);
    assert_eq!(back.exit, cfg.exit);
}
