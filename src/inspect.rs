//! Function-subgraph inspection (task t_4ffa769c).
//!
//! `inspect` dumps one function's subgraph (AST body + its CFG edges,
//! dominator/post-dominator edges, data-dependence edges, call edges) as DOT
//! or JSON for manual verification. Selection is by function name; the
//! subgraph includes the Function node, everything reachable from its body
//! via Ast edges, plus all non-Ast edges whose endpoints both fall inside
//! that set (that is exactly the CFG/dominators/DFG/call activity of the
//! function, since all overlays attach between existing AST nodes).

use crate::schema::{EdgeKind, Graph, NodeId, NodeKind};
use std::collections::{BTreeSet, HashMap, HashSet};

fn function_id(g: &Graph, name: &str) -> Option<NodeId> {
    g.nodes
        .iter()
        .find(|n| matches!(&n.kind, NodeKind::Function { name: fn_name, .. } if fn_name == name))
        .map(|n| n.id)
}

/// The closure of AST-reachable nodes from the function's body plus the
/// function node itself. Also returns any synthetic nodes referenced by
/// resolved call edges leaving the subgraph (so call targets are visible).
fn subgraph_nodes(g: &Graph, fn_id: NodeId) -> BTreeSet<NodeId> {
    let mut children: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for e in &g.edges {
        if matches!(e.kind, EdgeKind::Ast { .. }) {
            children.entry(e.src).or_default().push(e.dst);
        }
    }
    let mut out = BTreeSet::new();
    out.insert(fn_id);
    let mut stack = vec![fn_id];
    while let Some(id) = stack.pop() {
        if let Some(cs) = children.get(&id) {
            for &c in cs {
                if out.insert(c) {
                    stack.push(c);
                }
            }
        }
    }
    out
}

fn node_label(g: &Graph, id: NodeId) -> String {
    let n = &g.nodes[(id - 1) as usize];
    let kind = match &n.kind {
        NodeKind::File { path, .. } => format!("File {path}"),
        NodeKind::Module { name } => format!("Module {name}"),
        NodeKind::Function { name, .. } => format!("Function {name}"),
        NodeKind::Block => "Block".into(),
        NodeKind::Statement { stmt_kind } => format!("Stmt {stmt_kind}"),
        NodeKind::Expression { expr_kind, .. } => format!("Expr {expr_kind}"),
        NodeKind::Identifier { name } => format!("Id {name}"),
        NodeKind::Declaration { name, is_parameter } => {
            format!("Decl {name}{}", if *is_parameter { " (param)" } else { "" })
        }
        NodeKind::Type { name, .. } => format!("Type {name}"),
        NodeKind::Literal { value, .. } => format!("Lit {value}"),
        NodeKind::MacroInvocation { name } => format!("Macro {name}"),
        NodeKind::Import { path, .. } => format!("Import {path}"),
    };
    let kind_escaped = kind
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let code = n.common.code.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    let short: String = code.chars().take(40).collect();
    format!("{} [line {}] | {}", kind_escaped, n.common.span.start_line, short)
}

fn overlay_name(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Ast { .. } => "ast",
        EdgeKind::Cfg { .. } => "cfg",
        EdgeKind::DataDependence { .. } => "dfg",
        EdgeKind::Call { .. } => "call",
        EdgeKind::Dominate { .. } => "dom",
        EdgeKind::PostDominate { .. } => "postdom",
        EdgeKind::TypeHierarchy { .. } => "type",
        EdgeKind::ModuleDependency { .. } => "moddep",
    }
}

/// Collect the edges belonging to the function subgraph: Ast edges inside the
/// node set, plus any other edge with BOTH endpoints inside (overlay edges are
/// always AST-node → AST-node), plus Call edges FROM inside to synthetic
/// targets (resolved:false) so unresolved callees are visible.
fn subgraph_edges(
    g: &Graph,
    nodes: &BTreeSet<NodeId>,
) -> (Vec<(u64, NodeId, NodeId, EdgeKind)>, BTreeSet<NodeId>) {
    let mut edges = Vec::new();
    let mut extra = BTreeSet::new();
    for e in &g.edges {
        let inside_src = nodes.contains(&e.src);
        let inside_dst = nodes.contains(&e.dst);
        if inside_src && inside_dst {
            edges.push((e.id, e.src, e.dst, e.kind.clone()));
        } else if inside_src && matches!(e.kind, EdgeKind::Call { .. }) {
            edges.push((e.id, e.src, e.dst, e.kind.clone()));
            extra.insert(e.dst);
        }
    }
    (edges, extra)
}

/// JSON dump: nodes with kind/label, edges with overlay + payload.
pub fn function_subgraph_json(g: &Graph, function: &str) -> Result<String, String> {
    let fn_id = function_id(g, function)
        .ok_or_else(|| format!("function {function:?} not found"))?;
    let nodes = subgraph_nodes(g, fn_id);
    let (edges, extra) = subgraph_edges(g, &nodes);

    let mut out = String::from("{\n  \"function\": ");
    out.push_str(&serde_json::to_string(function).unwrap());
    out.push_str(",\n  \"nodes\": [");
    let mut first = true;
    for &id in nodes.iter().chain(extra.iter()) {
        if !first {
            out.push(',');
        }
        first = false;
        let n = &g.nodes[(id - 1) as usize];
        out.push_str(&format!(
            "\n    {{\"id\": {}, \"kind\": {:?}, \"label\": {}}}",
            id,
            n.kind,
            serde_json::to_string(&node_label(g, id)).unwrap()
        ));
    }
    out.push_str("\n  ],\n  \"edges\": [");
    let mut first = true;
    for (id, src, dst, kind) in &edges {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&format!(
            "\n    {{\"id\": {}, \"src\": {}, \"dst\": {}, \"overlay\": \"{}\", \"kind\": {:?}}}",
            id,
            src,
            dst,
            overlay_name(kind),
            kind
        ));
    }
    out.push_str("\n  ]\n}\n");
    Ok(out)
}

/// DOT dump with per-overlay edge styling.
pub fn function_subgraph_dot(g: &Graph, function: &str) -> Result<String, String> {
    let fn_id = function_id(g, function)
        .ok_or_else(|| format!("function {function:?} not found"))?;
    let nodes = subgraph_nodes(g, fn_id);
    let (edges, extra) = subgraph_edges(g, &nodes);

    let mut out = format!(
        "// CPG subgraph for function {function} ({} nodes, {} edges)\n",
        nodes.len() + extra.len(),
        edges.len()
    );
    out.push_str("digraph cpg {\n  rankdir=TB;\n  node [shape=box, fontname=\"Helvetica\", fontsize=10];\n");
    for &id in nodes.iter().chain(extra.iter()) {
        let n = &g.nodes[(id - 1) as usize];
        let label = node_label(g, id).replace('"', "\\\"");
        let (shape, color) = match &n.kind {
            NodeKind::Function { .. } => ("component", "#9370db"),
            NodeKind::Statement { .. } => ("box", "#a0d8ef"),
            NodeKind::Expression { .. } => ("ellipse", "#f4a460"),
            NodeKind::Declaration { .. } => ("note", "#98fb98"),
            NodeKind::Identifier { .. } => ("plain", "#dddddd"),
            NodeKind::Literal { .. } => ("parallelogram", "#ffe4b5"),
            NodeKind::MacroInvocation { .. } => ("hexagon", "#ffb6c1"),
            _ => ("box", "#cccccc"),
        };
        out.push_str(&format!(
            "  n{} [label=\"{}\", shape={}, style=filled, fillcolor=\"{}\"];\n",
            id, label, shape, color
        ));
    }
    let style = |k: &EdgeKind| match k {
        EdgeKind::Ast { field, .. } => (
            "solid",
            match field {
                Some(f) if f == "body" || f == "consequence" || f == "alternative" => "#333333",
                _ => "#888888",
            },
            field.clone().unwrap_or_default(),
        ),
        EdgeKind::Cfg { branch_label } => (
            "bold",
            "#c0392b",
            format!("cfg{}", branch_label.as_ref().map(|b| format!(":{b}")).unwrap_or_default()),
        ),
        EdgeKind::DataDependence { variable } => ("dashed", "#2980b9", format!("dfg:{variable}")),
        EdgeKind::Call { resolved, .. } => (
            "bold",
            if *resolved { "#27ae60" } else { "#e67e22" },
            format!("call{}", if *resolved { "" } else { "?" }),
        ),
        EdgeKind::Dominate => ("dotted", "#8e44ad", "dom".into()),
        EdgeKind::PostDominate => ("dotted", "#16a085", "postdom".into()),
        EdgeKind::TypeHierarchy { .. } => ("solid", "#7f8c8d", "type".into()),
        EdgeKind::ModuleDependency { import_path } => {
            ("solid", "#7f8c8d", format!("moddep:{import_path}"))
        }
    };
    let mut seen: HashSet<u64> = HashSet::new();
    for (id, src, dst, kind) in &edges {
        if !seen.insert(*id) {
            continue;
        }
        let (st, color, label) = style(kind);
        out.push_str(&format!(
            "  n{} -> n{} [style={}, color=\"{}\", label=\"{}\", fontsize=8, fontcolor=\"{}\"];\n",
            src, dst, st, color, label, color
        ));
    }
    out.push_str("}\n");
    Ok(out)
}
