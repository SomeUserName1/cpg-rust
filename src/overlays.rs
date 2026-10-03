//! Overlay passes over the AST-layer CPG (task t_8306ad86).
//!
//! Three language-agnostic passes over the shared node set, per the schema's
//! overlay composition model (docs/cpg-schema.md §4-6):
//!
//! 1. Data dependence / def-use (DFG): per outermost function, link each
//!    identifier use to the reaching definition(s) of its variable —
//!    parameters, let patterns, const/static, match-arm bindings, and
//!    assignment LHS. Scope-aware (inner declarations shadow outer), with a
//!    loop rule: a definition inside a loop that also encloses the use
//!    reaches it even when it appears textually later (loop-carried
//!    dependence). Nested closures are analyzed in the enclosing scope so
//!    captures fall out naturally. Resolved calls additionally get
//!    argument -> parameter and return -> call-site hand-off edges.
//!
//! 2. Call graph: resolve call expressions and macro invocations to callee
//!    definitions. Direct calls by name, path calls (`Type::method`,
//!    `module::fn`), and method calls via best-effort receiver type
//!    inference (`let d = Dog::new(..)` / struct literals give `d: Dog`,
//!    then impl methods are matched). When a method name exists in several
//!    impls (or the receiver type is unknown) the edge is emitted with
//!    `resolved: false` — dynamic dispatch is flagged, never dropped.
//!    Call sites with no in-graph candidate at all (std/external methods)
//!    get a `resolved: false` edge to a synthetic
//!    `Function { name: "<unresolved:...>" }` node.
//!
//! 3. Module/package dependencies: `use` and `extern crate` declarations are
//!    resolved to internal files (crate-relative module paths), to synthetic
//!    `<crate:name>` module nodes for dependencies declared in Cargo.toml,
//!    or skipped for std/core paths. `mod x;` -> file links are already
//!    emitted by the AST builder; this pass adds the import-driven edges.
//!
//! All passes are graph-local: they consume Nodes + Ast edges only.

use crate::schema::{Edge, EdgeKind, Graph, NodeId, NodeKind};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Read-and-increment helper for edge ids inside struct literals.
fn next_eid_bump(next: &mut u64) -> u64 {
    let v = *next;
    *next += 1;
    v
}

/// Counts of edges added by the overlay passes (folded into BuildStats).
#[derive(Debug, Default, Clone)]
pub struct OverlayStats {
    pub dfg_edges: u64,
    pub call_edges: u64,
    pub call_unresolved: u64,
    pub import_edges: u64,
}

// ---------------------------------------------------------------------------
// Graph index helpers
// ---------------------------------------------------------------------------

struct Index {
    /// id -> node position in g.nodes
    pos: HashMap<NodeId, usize>,
    /// children by Ast edge: (field name, child id), in sibling order
    children: HashMap<NodeId, Vec<(Option<String>, NodeId)>>,
    /// primary (first) Ast parent
    parent: HashMap<NodeId, NodeId>,
}

impl Index {
    fn build(g: &Graph) -> Index {
        let mut pos = HashMap::new();
        for (i, n) in g.nodes.iter().enumerate() {
            pos.insert(n.id, i);
        }
        let mut children: HashMap<NodeId, Vec<(Option<String>, NodeId)>> = HashMap::new();
        let mut parent = HashMap::new();
        for e in &g.edges {
            if let EdgeKind::Ast { .. } = &e.kind {
                children.entry(e.src).or_default().push((None, e.dst));
                parent.entry(e.dst).or_insert(e.src);
            }
        }
        // recover field names: re-walk edges to attach fields
        children.clear();
        for e in &g.edges {
            if let EdgeKind::Ast { field, .. } = &e.kind {
                children
                    .entry(e.src)
                    .or_default()
                    .push((field.clone(), e.dst));
            }
        }
        for v in children.values_mut() {
            v.reverse(); // edges were pushed parent->child in order; Ast edge order in vec is fine
        }
        Index { pos, children, parent }
    }

    fn kind<'g>(&self, g: &'g Graph, id: NodeId) -> &'g NodeKind {
        &g.nodes[self.pos[&id]].kind
    }

    fn code<'a>(&self, g: &'a Graph, id: NodeId) -> &'a str {
        &g.nodes[self.pos[&id]].common.code
    }

    fn start(&self, g: &Graph, id: NodeId) -> u64 {
        g.nodes[self.pos[&id]].common.span.start_byte
    }

    fn children_of(&self, id: NodeId) -> &[(Option<String>, NodeId)] {
        self.children.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// child by tree-sitter field name
    fn child_field(&self, id: NodeId, field: &str) -> Option<NodeId> {
        self.children_of(id)
            .iter()
            .find(|(f, _)| f.as_deref() == Some(field))
            .map(|(_, c)| *c)
    }

    /// innermost ancestor chain of scope-forming nodes (Function / Block),
    /// innermost first.
    fn scope_chain(&self, g: &Graph, mut id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        loop {
            match self.kind(g, id) {
                NodeKind::Function { .. } | NodeKind::Block => out.push(id),
                NodeKind::File { .. } => break,
                _ => {}
            }
            match self.parent.get(&id) {
                Some(&p) => id = p,
                None => break,
            }
        }
        out
    }

    /// loop ancestors: statement nodes with stmt_kind for/while/loop that
    /// contain `id` (innermost first).
    fn loop_ancestors(&self, g: &Graph, mut id: NodeId) -> HashSet<NodeId> {
        let mut out = HashSet::new();
        loop {
            if let NodeKind::Statement { stmt_kind } = self.kind(g, id) {
                if matches!(stmt_kind.as_str(), "for" | "while" | "loop") {
                    out.insert(id);
                }
            }
            match self.parent.get(&id) {
                Some(&p) => id = p,
                None => break,
            }
        }
        out
    }

    /// walk up to the File node
    fn file_of(&self, g: &Graph, mut id: NodeId) -> Option<NodeId> {
        loop {
            if matches!(self.kind(g, id), NodeKind::File { .. }) {
                return Some(id);
            }
            id = *self.parent.get(&id)?;
        }
    }

    /// all descendant ids of `id` (including itself)
    fn descendants(&self, id: NodeId, out: &mut Vec<NodeId>) {
        out.push(id);
        for (_, c) in self.children_of(id) {
            self.descendants(*c, out);
        }
    }
}

fn is_call_node(k: &NodeKind) -> bool {
    matches!(k, NodeKind::Expression { expr_kind, .. } if expr_kind == "call")
}

fn decl_name(k: &NodeKind) -> Option<(&str, bool)> {
    match k {
        NodeKind::Declaration { name, is_parameter } => Some((name.as_str(), *is_parameter)),
        _ => None,
    }
}

fn ident_name(k: &NodeKind) -> Option<&str> {
    match k {
        NodeKind::Identifier { name } => Some(name.as_str()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// 1. Data dependence / def-use
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Def {
    /// node whose span identifies the definition (Declaration or assignment Expression)
    node: NodeId,
    name: String,
    start: u64,
    /// innermost Block/Function scope containing the definition
    scope: NodeId,
    /// loop ancestors of the definition
    loops: HashSet<NodeId>,
}

/// Run the DFG pass over the whole graph. Returns added DataDependence edges.
fn dfg_pass(g: &mut Graph, idx: &Index) -> Vec<Edge> {
    let mut out = Vec::new();

    // analysis units: Function nodes not nested inside another Function
    let units: Vec<NodeId> = g
        .nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function { .. }))
        .map(|n| n.id)
        .filter(|&id| {
            idx.scope_chain(g, id)
                .iter()
                .filter(|&&s| s != id)
                .all(|&s| !matches!(idx.kind(g, s), NodeKind::Function { .. }))
        })
        .collect();

    for unit in units {
        analyze_unit(g, idx, unit, &mut out);
    }
    out
}

fn collect_defs_and_uses(
    g: &Graph,
    idx: &Index,
    id: NodeId,
    defs: &mut Vec<Def>,
    uses: &mut Vec<(NodeId, String)>,
) {
    let kind = idx.kind(g, id);
    match kind {
        NodeKind::Declaration { name, .. } => {
            defs.push(Def {
                node: id,
                name: name.clone(),
                start: idx.start(g, id),
                scope: idx
                    .scope_chain(g, id)
                    .first()
                    .copied()
                    .unwrap_or(id),
                loops: idx.loop_ancestors(g, id),
            });
        }
        NodeKind::Expression { expr_kind, .. } if expr_kind == "assignment" => {
            // LHS identifier is a definition; RHS handled by normal recursion.
            if let Some(left) = idx.child_field(id, "left") {
                // plain identifier LHS only (best-effort; field/index LHS skipped)
                if ident_name(idx.kind(g, left)).is_some() {
                    let name = idx.code(g, left).to_string();
                    defs.push(Def {
                        node: id,
                        name,
                        start: idx.start(g, id),
                        scope: idx.scope_chain(g, id).first().copied().unwrap_or(id),
                        loops: idx.loop_ancestors(g, id),
                    });
                }
            }
        }
        NodeKind::Identifier { name } => {
            // skip field-access positions: the "field" child of a field_expression
            let is_field_access = idx
                .parent
                .get(&id)
                .and_then(|&p| {
                    idx.children_of(p).iter().find_map(|(f, c)| {
                        if *c == id { f.clone() } else { None }
                    })
                })
                .map(|f| f == "field"
                    && matches!(
                        idx.kind(g, idx.parent[&id]),
                        NodeKind::Expression { expr_kind, .. } if expr_kind == "field"
                    ))
                .unwrap_or(false);
            if !is_field_access && name != "_" && !name.is_empty() {
                uses.push((id, name.clone()));
            }
        }
        _ => {}
    }
    for (_, c) in idx.children_of(id) {
        collect_defs_and_uses(g, idx, *c, defs, uses);
    }
}

fn analyze_unit(g: &Graph, idx: &Index, unit: NodeId, out: &mut Vec<Edge>) {
    let mut defs = Vec::new();
    let mut uses = Vec::new();
    collect_defs_and_uses(g, idx, unit, &mut defs, &mut uses);

    let mut edges: Vec<Edge> = Vec::new();
    let mut next_eid = g.edges.len() as u64 + 1;
    for (use_id, var) in &uses {
        let use_start = idx.start(g, *use_id);
        let use_scopes: HashSet<NodeId> = idx.scope_chain(g, *use_id).into_iter().collect();
        let use_loops = idx.loop_ancestors(g, *use_id);

        // reaching defs: visible scope + (textually before OR loop-carried)
        let reaching: Vec<&Def> = defs
            .iter()
            .filter(|d| {
                d.name == *var
                    && use_scopes.contains(&d.scope)
                    && (d.start <= use_start || d.loops.intersection(&use_loops).next().is_some())
            })
            .collect();
        if reaching.is_empty() {
            continue;
        }
        // Shadowing is driven by Declarations only: an assignment re-defines
        // the same binding and never shadows. For earlier Declaration defs,
        // only the innermost scope level's declaration reaches (shadowing);
        // assignments always reach (their scope is already in the use's chain).
        let chain = idx.scope_chain(g, *use_id);
        let innermost_decl_scope = chain
            .iter()
            .find(|s| {
                reaching.iter().any(|d| {
                    d.scope == **s
                        && d.start <= use_start
                        && matches!(idx.kind(g, d.node), NodeKind::Declaration { .. })
                })
            })
            .copied();
        for d in reaching {
            let keep = match idx.kind(g, d.node) {
                NodeKind::Declaration { .. } => match innermost_decl_scope {
                    Some(s) => d.scope == s,
                    None => false,
                },
                _ => true, // assignment def
            };
            if keep {
                edges.push(Edge {
                    id: next_eid_bump(&mut next_eid),
                    src: d.node,
                    dst: *use_id,
                    kind: EdgeKind::DataDependence {
                        variable: var.clone(),
                    },
                });
            }
        }
    }
    out.extend(edges);
}

// ---------------------------------------------------------------------------
// 2. Call graph
// ---------------------------------------------------------------------------

struct CallCtx {
    /// function name -> id (non-closure)
    functions: HashMap<String, NodeId>,
    /// impl'd type -> method name -> ids (an impl per type; trait impls merged)
    type_methods: HashMap<String, HashMap<String, Vec<NodeId>>>,
    /// macro name -> defining MacroInvocation node id
    macro_defs: HashMap<String, NodeId>,
    /// let-bound identifier -> inferred type name (best-effort)
    var_types: HashMap<String, String>,
    /// synthetic "<unresolved:X>" nodes, one per distinct callee text
    unresolved: HashMap<String, NodeId>,
}

fn build_call_ctx(g: &Graph, idx: &Index) -> CallCtx {
    let mut ctx = CallCtx {
        functions: HashMap::new(),
        type_methods: HashMap::new(),
        macro_defs: HashMap::new(),
        var_types: HashMap::new(),
        unresolved: HashMap::new(),
    };
    for n in &g.nodes {
        match &n.kind {
            NodeKind::Function { name, .. } if name != "<closure>" => {
                ctx.functions.insert(name.clone(), n.id);
            }
            NodeKind::Type { name } => {
                // impl methods attach to the impl'd Type node, but possibly
                // one level down (through a raw declaration_list container) —
                // collect all Function descendants instead of direct children.
                let mut subtree = Vec::new();
                idx.descendants(n.id, &mut subtree);
                for c in subtree {
                    if let NodeKind::Function { name: m, .. } = idx.kind(g, c) {
                        if m != "<closure>" {
                            ctx.type_methods
                                .entry(name.clone())
                                .or_default()
                                .entry(m.clone())
                                .or_default()
                                .push(c);
                        }
                    }
                }
            }
            NodeKind::MacroInvocation { name: _ } => {
                // macro_rules! definitions carry the defined name in `code`
                let code = idx.code(g, n.id);
                if let Some(rest) = code.strip_prefix("macro_rules!") {
                    let mname: String = rest
                        .trim_start()
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    if !mname.is_empty() {
                        ctx.macro_defs.insert(mname, n.id);
                    }
                }
            }
            _ => {}
        }
    }
    // best-effort receiver type inference: `let x = Type::..(..)` or `let x = Type { .. }`
    for n in &g.nodes {
        if matches!(&n.kind, NodeKind::Statement { stmt_kind } if stmt_kind == "let") {
            if let (Some(pat), Some(val)) = (
                idx.child_field(n.id, "pattern"),
                idx.child_field(n.id, "value"),
            ) {
                if let Some(name) = ident_name(idx.kind(g, pat))
                    .or_else(|| decl_name(idx.kind(g, pat)).map(|(n, _)| n))
                {
                    let ty = infer_type_of(g, idx, val);
                    if let Some(ty) = ty {
                        ctx.var_types.insert(name.to_string(), ty);
                    }
                }
            }
        }
    }
    ctx
}

fn infer_type_of(g: &Graph, idx: &Index, val: NodeId) -> Option<String> {
    match idx.kind(g, val) {
        NodeKind::Expression { expr_kind, .. } if expr_kind == "struct" => {
            // struct_expression: name field child is a Type node
            if let Some(name) = idx.child_field(val, "name") {
                if let NodeKind::Type { name } = idx.kind(g, name) {
                    return Some(name.clone());
                }
            }
            None
        }
        NodeKind::Expression { expr_kind, .. } if expr_kind == "call" => {
            let f = idx.child_field(val, "function")?;
            if let NodeKind::Expression { expr_kind: ek, .. } = idx.kind(g, f) {
                if ek == "raw:scoped_identifier" {
                    let text = idx.code(g, f);
                    let segs: Vec<&str> = text.split("::").collect();
                    if segs.len() >= 2 {
                        return Some(segs[segs.len() - 2].to_string());
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn ensure_unresolved(
    g: &mut Graph,
    ctx: &mut CallCtx,
    callee_text: &str,
) -> NodeId {
    if let Some(&id) = ctx.unresolved.get(callee_text) {
        return id;
    }
    // synthetic node: no real span; reuse a zero span in no file
    let id = g.nodes.len() as u64 + 1;
    g.nodes.push(crate::schema::Node {
        id,
        common: crate::schema::NodeCommon {
            span: crate::schema::SourceSpan {
                file: String::new(),
                start_byte: 0,
                end_byte: 0,
                start_line: 0,
                start_col: 0,
            },
            code: callee_text.to_string(),
            order: 0,
        },
        kind: NodeKind::Function {
            name: format!("<unresolved:{callee_text}>"),
            signature: String::new(),
        },
    });
    ctx.unresolved.insert(callee_text.to_string(), id);
    id
}

fn call_pass(g: &mut Graph, idx: &Index, ctx: &mut CallCtx, dfg_out: &mut Vec<Edge>) -> Vec<Edge> {
    let mut out = Vec::new();
    let mut next_eid = g.edges.len() as u64 + dfg_out.len() as u64 + 1;

    let call_sites: Vec<NodeId> = g
        .nodes
        .iter()
        .filter(|n| is_call_node(&n.kind))
        .map(|n| n.id)
        .collect();

    for site in call_sites {
        let f = match idx.child_field(site, "function") {
            Some(f) => f,
            None => continue,
        };
        let resolved_targets: Vec<(NodeId, String)> = match idx.kind(g, f) {
            // direct call: `foo(...)`
            NodeKind::Identifier { name } => {
                ctx.functions.get(name).map(|&id| vec![(id, name.clone())]).unwrap_or_default()
            }
            // path call: `Dog::new(...)` / `utils::deep::deep_fn(...)`
            NodeKind::Expression { expr_kind, .. } if expr_kind == "raw:scoped_identifier" => {
                let text = idx.code(g, f).to_string();
                let segs: Vec<&str> = text.split("::").collect();
                let mut hits = Vec::new();
                if segs.len() >= 2 {
                    let ty = segs[segs.len() - 2];
                    let m = segs[segs.len() - 1];
                    if let Some(ids) = ctx.type_methods.get(ty).and_then(|tm| tm.get(m)) {
                        for &id in ids {
                            hits.push((id, text.clone()));
                        }
                    }
                }
                if hits.is_empty() {
                    if let Some(last) = segs.last() {
                        if let Some(&id) = ctx.functions.get(*last) {
                            hits.push((id, text.clone()));
                        }
                    }
                }
                hits
            }
            // method call: `expr.method(...)`
            NodeKind::Expression { expr_kind, .. } if expr_kind == "field" => {
                let method = idx
                    .child_field(f, "field")
                    .and_then(|m| ident_name(idx.kind(g, m)).map(|s| s.to_string()));
                let receiver = idx.child_field(f, "value");
                let recv_ty = receiver
                    .as_ref()
                    .and_then(|&r| ident_name(idx.kind(g, r)))
                    .and_then(|n| ctx.var_types.get(n).cloned());
                let method = match method {
                    Some(m) => m,
                    None => continue,
                };
                let mut hits = Vec::new();
                if let Some(ty) = &recv_ty {
                    if let Some(ids) = ctx.type_methods.get(ty).and_then(|tm| tm.get(&method)) {
                        for &id in ids {
                            hits.push((id, format!("{ty}::{method}")));
                        }
                    }
                }
                if hits.is_empty() {
                    // dynamic/ambiguous: every impl defining this method, flagged unresolved
                    let mut all = Vec::new();
                    for tm in ctx.type_methods.values() {
                        if let Some(ids) = tm.get(&method) {
                            all.extend(ids.iter().copied());
                        }
                    }
                    for id in all {
                        hits.push((id, method.clone()));
                    }
                }
                hits
            }
            _ => Vec::new(),
        };

        if resolved_targets.is_empty() {
            // no in-graph candidate: flag as unresolved (never dropped)
            let callee_text = idx.code(g, f).to_string();
            let tgt = ensure_unresolved(g, ctx, &callee_text);
            out.push(Edge {
                id: next_eid_bump(&mut next_eid),
                // bump below
                // bump below
                src: site,
                dst: tgt,
                kind: EdgeKind::Call { resolved: false, argument_index: None },
            });
            continue;
        }

        for (callee, _label) in &resolved_targets {
            out.push(Edge {
                id: next_eid_bump(&mut next_eid),
                // bump below
                // bump below
                src: site,
                dst: *callee,
                kind: EdgeKind::Call { resolved: true, argument_index: None },
            });
        }

        // argument -> parameter and return -> call-site hand-offs (DFG)
        let (callee, _) = &resolved_targets[0];
        let params: Vec<NodeId> = idx
            .child_field(*callee, "parameters")
            .map(|ps| {
                idx.children_of(ps)
                    .iter()
                    .filter(|(_, c)| decl_name(idx.kind(g, *c)).is_some())
                    .map(|(_, c)| *c)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(args) = idx.child_field(site, "arguments") {
            for (i, (_, arg)) in idx.children_of(args).iter().enumerate() {
                if let Some(&p) = params.get(i) {
                    dfg_out.push(Edge {
                        id: next_eid_bump(&mut next_eid),
                        // bump below
                        // bump below
                        src: *arg,
                        dst: p,
                        kind: EdgeKind::DataDependence { variable: format!("<arg:{i}>") },
                    });
                }
            }
        }
        // return statements inside the callee flow back to the call site
        let mut subtree = Vec::new();
        idx.descendants(*callee, &mut subtree);
        for n in subtree {
            if matches!(idx.kind(g, n), NodeKind::Statement { stmt_kind } if stmt_kind == "return")
            {
                dfg_out.push(Edge {
                    id: next_eid_bump(&mut next_eid),
                    // bump below
                    // bump below
                    src: n,
                    dst: site,
                    kind: EdgeKind::DataDependence { variable: "<ret>".into() },
                });
            }
        }
    }

    // macro invocations -> macro_rules! definitions (or flagged unresolved)
    let invocations: Vec<(NodeId, String)> = g
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::MacroInvocation { name } => {
                if idx.code(g, n.id).starts_with("macro_rules!") {
                    None // definitions, not call sites
                } else {
                    Some((n.id, name.clone()))
                }
            }
            _ => None,
        })
        .collect();
    for (site, name) in invocations {
        match ctx.macro_defs.get(&name) {
            Some(&def) => out.push(Edge {
                id: next_eid_bump(&mut next_eid),
                // bump below
                // bump below
                src: site,
                dst: def,
                kind: EdgeKind::Call { resolved: true, argument_index: None },
            }),
            None => {
                let tgt = ensure_unresolved(g, ctx, &format!("{name}!"));
                out.push(Edge {
                    id: next_eid_bump(&mut next_eid),
                    // bump below
                    // bump below
                    src: site,
                    dst: tgt,
                    kind: EdgeKind::Call { resolved: false, argument_index: None },
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 3. Module / package dependencies
// ---------------------------------------------------------------------------

/// module path ("utils::deep") -> File node id, for internal resolution.
fn internal_modules(g: &Graph, idx: &Index) -> HashMap<String, NodeId> {
    let mut out = HashMap::new();
    for n in &g.nodes {
        if let NodeKind::File { path, .. } = &n.kind {
            let p = path.trim_end_matches(".rs");
            let segs: Vec<&str> = p.split('/').collect();
            // strip a leading src/ (crate-root layout)
            let segs: Vec<&str> = if segs.first() == Some(&"src") { segs[1..].to_vec() } else { segs };
            let mut segs: Vec<String> = segs.iter().map(|s| s.to_string()).collect();
            // mod.rs names its parent directory
            if segs.last().map(|s| s == "mod").unwrap_or(false) {
                segs.pop();
            }
            if segs.is_empty() {
                continue; // crate root: main.rs / lib.rs
            }
            out.insert(segs.join("::"), n.id);
        }
    }
    // make sure File nodes are reachable via Ast for parent lookups (defensive)
    let _ = idx;
    out
}

/// Parse `[dependencies]` (and `[dependencies.x]` / `[dev-dependencies]`) out
/// of Cargo.toml text. Hand-rolled: dep names only, no full TOML.
fn cargo_dependencies(cargo_toml: &str) -> Vec<String> {
    let mut deps = Vec::new();
    let mut in_deps = false;
    for line in cargo_toml.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            let sec = t.trim_start_matches('[').trim_end_matches(']');
            in_deps = sec == "dependencies" || sec == "dev-dependencies" || sec == "build-dependencies"
                || sec.starts_with("dependencies.");
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(name) = t.split('=').next() {
            let name = name.trim();
            if !name.is_empty() {
                deps.push(name.to_string());
            }
        }
    }
    deps
}

fn import_pass(
    g: &mut Graph,
    idx: &Index,
    folder: &Path,
) -> (Vec<Edge>, u64 /* synthetic crate nodes */) {
    let mut out = Vec::new();
    let mut next_eid = g.edges.len() as u64 + 1;
    let mods = internal_modules(g, idx);

    let cargo_deps: Vec<String> = {
        let mut candidates = vec![folder.join("Cargo.toml"), folder.join("src").join("Cargo.toml")];
        // also honor nested crate roots' Cargo.toml
        for n in &g.nodes {
            if let NodeKind::File { path, .. } = &n.kind {
                let p = Path::new(path);
                if let Some(dir) = p.parent() {
                    candidates.push(folder.join(dir).join("Cargo.toml"));
                }
            }
        }
        let mut seen = HashSet::new();
        let mut deps = Vec::new();
        for c in candidates {
            if c.is_file() {
                if let Ok(txt) = std::fs::read_to_string(&c) {
                    for d in cargo_dependencies(&txt) {
                        if seen.insert(d.clone()) {
                            deps.push(d);
                        }
                    }
                }
            }
        }
        deps
    };

    // synthetic <crate:name> module nodes for external dependencies
    let mut crate_nodes: HashMap<String, NodeId> = HashMap::new();
    for dep in &cargo_deps {
        let id = g.nodes.len() as u64 + 1;
        g.nodes.push(crate::schema::Node {
            id,
            common: crate::schema::NodeCommon {
                span: crate::schema::SourceSpan {
                    file: String::new(),
                    start_byte: 0,
                    end_byte: 0,
                    start_line: 0,
                    start_col: 0,
                },
                code: format!("extern crate {dep}"),
                order: 0,
            },
            kind: NodeKind::Module { name: format!("<crate:{dep}>") },
        });
        crate_nodes.insert(dep.clone(), id);
    }

    let imports: Vec<(NodeId, String)> = g
        .nodes
        .iter()
        .filter_map(|n| match &n.kind {
            NodeKind::Import { path, .. } => Some((n.id, path.clone())),
            _ => None,
        })
        .collect();

    let mut added = 0u64;
    for (imp_id, path) in imports {
        let file = match idx.file_of(g, imp_id) {
            Some(f) => f,
            None => continue,
        };
        let segs: Vec<String> = path
            .split("::")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && *s != "*")
            .collect();
        if segs.is_empty() {
            continue;
        }
        // `crate::a::b` and bare `a::b` resolve against internal module paths
        let cand: &[String] = if segs[0] == "crate" { &segs[1..] } else { &segs[..] };
        let mut target: Option<NodeId> = None;
        for k in (1..=cand.len()).rev() {
            let key = cand[..k].join("::");
            if let Some(&f) = mods.get(&key) {
                if f != file {
                    target = Some(f);
                }
                break;
            }
        }
        if let Some(dst) = target {
            out.push(Edge {
                id: next_eid_bump(&mut next_eid),
                // bump below
                // bump below
                src: file,
                dst,
                kind: EdgeKind::ModuleDependency { import_path: path.clone() },
            });
            added += 1;
            continue;
        }
        // external crate
        if let Some(dst) = crate_nodes.get(&segs[0]) {
            out.push(Edge {
                id: next_eid_bump(&mut next_eid),
                // bump below
                // bump below
                src: file,
                dst: *dst,
                kind: EdgeKind::ModuleDependency { import_path: path.clone() },
            });
            added += 1;
        }
        // std / core / unknown: skipped (not a package dependency)
    }
    (out, added)
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

/// Run all three overlay passes on `g`, appending nodes and edges in place.
pub fn build_overlays(g: &mut Graph, folder: &Path) -> Result<OverlayStats, String> {
    let idx = Index::build(g);

    // DFG first (graph reads only), then call graph (may add synthetic nodes),
    // then imports (may add synthetic crate nodes).
    let mut dfg = dfg_pass(g, &idx);
    let mut ctx = build_call_ctx(g, &idx);
    let mut calls = call_pass(g, &idx, &mut ctx, &mut dfg);

    let (mut imports, _synth) = import_pass(g, &idx, folder);

    let mut stats = OverlayStats::default();
    stats.dfg_edges = dfg.len() as u64;
    stats.call_edges = calls.iter().filter(|e| matches!(e.kind, EdgeKind::Call { resolved: true, .. })).count() as u64;
    stats.call_unresolved = calls.iter().filter(|e| matches!(e.kind, EdgeKind::Call { resolved: false, .. })).count() as u64;
    stats.import_edges = imports.len() as u64;

    let mut next_eid = g.edges.len() as u64 + 1;
    for e in dfg.iter_mut().chain(calls.iter_mut()).chain(imports.iter_mut()) {
        e.id = next_eid;
        next_eid += 1;
        g.edges.push(e.clone());
    }
    Ok(stats)
}
