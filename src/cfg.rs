//! CFG construction over the Rust AST layer of the CPG.
//!
//! ## Design decision: statement-level flow nodes
//!
//! The CFG uses **statement-level flow nodes** rather than basic blocks:
//! one CFG node per AST `Statement` node (let / expression-statement /
//! if / match / while / loop / for / break / continue / return), plus
//! condition nodes for branching decisions, plus block tail-expression
//! nodes, plus virtual `Entry`, `Exit` and `Panic` nodes per function.
//! Reasons:
//!
//! - the AST layer already materializes kinded Statement nodes with spans
//!   (t_75f6e158), so CFG nodes map 1:1 to source locations — good for
//!   cross-overlay queries;
//! - branching must be labelled per-branch (`true`/`false`, match-arm
//!   patterns), which basic blocks would hide;
//! - splitting into basic blocks is a purely local re-computation any
//!   downstream consumer can do from these edges.
//!
//! `Block` AST nodes are transparent: their statements are inlined into the
//! enclosing construct's flow (the Block node itself is NOT a CFG node).
//! Block tail expressions (non-Statement AST nodes) get their own flow node.
//!
//! ## Edges (`EdgeKind::Cfg { branch_label }`, "cfg" namespace)
//!
//! - sequencing: no label;
//! - `if`: condition → first node of each branch ("true"/"false"); a branch
//!   with no else falls through to the join with label "false";
//! - `match`: condition → arm-entry nodes ("0", "m if ...", "_" — pattern
//!   text labels); a guarded arm chains: guard → body ("true"), guard →
//!   next arm-entry (pattern miss);
//! - `loop`: head node; body back-edges to head; break → loop-exit node;
//! - `while`: condition → body ("true") / loop-exit ("false"); back edge;
//! - `for`: Iterate node (binds the loop variable) → condition → body
//!   ("true") / loop-exit ("false"); body back-edges to condition;
//! - `break` → loop-exit ("break"), `continue` → loop head ("continue"),
//!   `return` → Exit ("return");
//! - **always-panic statements** (`panic!`/`unreachable!`/`todo!`/
//!   `unimplemented!` expressions) flow to the `Panic` node ("panic") and
//!   do NOT fall through;
//! - **maybe-panic operations (best-effort)**: statements whose
//!   subexpressions contain indexing, `.unwrap()`/`.expect()`, division/
//!   remainder with a possibly-zero divisor, or a parse-degraded
//!   (`error:`) subtree, get an EXTRA edge → the function's `Panic` node
//!   labelled "panic", in addition to normal flow. Conservative superset
//!   by design.

use crate::schema::{Edge, EdgeKind, Graph, Node as CpgNode, NodeId, NodeKind, SourceSpan};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Helpers on the AST graph
// ---------------------------------------------------------------------------

fn stmt_kind(n: &CpgNode) -> Option<&str> {
    match &n.kind {
        NodeKind::Statement { stmt_kind } => Some(stmt_kind.as_str()),
        _ => None,
    }
}

/// Named child of an AST node by tree-sitter field name.
fn ast_child(g: &Graph, id: NodeId, field: &str) -> Option<NodeId> {
    g.edges.iter().find_map(|e| match &e.kind {
        EdgeKind::Ast { field: f, .. } if e.src == id && f.as_deref() == Some(field) => Some(e.dst),
        _ => None,
    })
}

/// Children of an AST node in source (sibling-order) sequence.
fn children(g: &Graph, id: NodeId) -> Vec<NodeId> {
    let mut out: Vec<(u32, NodeId)> = Vec::new();
    for e in &g.edges {
        if let EdgeKind::Ast { order, .. } = &e.kind {
            if e.src == id {
                out.push((*order, e.dst));
            }
        }
    }
    out.sort_by_key(|(o, _)| *o);
    out.into_iter().map(|(_, d)| d).collect()
}

fn node_code(g: &Graph, id: NodeId) -> String {
    g.nodes[(id - 1) as usize].common.code.clone()
}

// ---------------------------------------------------------------------------
// Panic classification (best-effort)
// ---------------------------------------------------------------------------

const PANIC_MACROS: [&str; 7] = [
    "panic!", "unreachable!", "todo!", "unimplemented!", "assert!", "assert_eq!", "assert_ne!",
];

const ALWAYS_PANIC_EXPR_KINDS: [&str; 1] = ["raw:panic_expression"];

/// Does this AST subtree contain an ALWAYS-panicking expression (panic! etc.)?
fn contains_always_panic(g: &Graph, id: NodeId) -> bool {
    let n = &g.nodes[(id - 1) as usize];
    if let NodeKind::Expression { expr_kind, .. } = &n.kind {
        if ALWAYS_PANIC_EXPR_KINDS.contains(&expr_kind.as_str()) {
            return true;
        }
        if expr_kind == "call" {
            let callee = ast_child(g, id, "function")
                .map(|c| node_code(g, c))
                .unwrap_or_default();
            if callee.contains("panic!")
                || callee.contains("unreachable!")
                || callee.contains("todo!")
                || callee.contains("unimplemented!")
            {
                return true;
            }
        }
    }
    // MacroInvocation nodes named panic!/unreachable!/todo!/unimplemented!
    // (the AST layer records the name WITHOUT the trailing '!')
    if let NodeKind::MacroInvocation { name } = &n.kind {
        let name = name.trim_start_matches("std::").trim_end_matches('!');
        if matches!(name, "panic" | "unreachable" | "todo" | "unimplemented") {
            return true;
        }
    }
    children(g, id)
        .into_iter()
        .any(|c| contains_always_panic(g, c))
}

/// May evaluating this AST node panic (best-effort conservative)?
/// Includes always-panics, indexing, unwrap/expect, fallible division,
/// and parse-degraded subtrees.
fn expr_may_panic(g: &Graph, id: NodeId) -> bool {
    if contains_always_panic(g, id) {
        return true;
    }
    let n = &g.nodes[(id - 1) as usize];
    match &n.kind {
        NodeKind::Expression { expr_kind, .. } => {
            if expr_kind.starts_with("error:") {
                return true; // parse-degraded subtree: assume it can panic
            }
            match expr_kind.as_str() {
                "index" => return true,
                "call" => {
                    let callee = ast_child(g, id, "function")
                        .map(|c| node_code(g, c))
                        .unwrap_or_default();
                    if PANIC_MACROS.iter().any(|p| callee.contains(p)) {
                        return true;
                    }
                    // method calls: x.unwrap(), x.expect("...")
                    if let Some(base) = callee.rsplit('.').next() {
                        let base = base.trim();
                        if matches!(base, "unwrap" | "expect" | "unwrap_err") {
                            return true;
                        }
                    }
                }
                "binary" => {
                    let code = &n.common.code;
                    for op in ["/", "%"] {
                        if code.contains(op) {
                            // conservatively may-panic unless the divisor is a
                            // nonzero literal.
                            if let Some(rhs) = ast_child(g, id, "right") {
                                let r = &g.nodes[(rhs - 1) as usize];
                                let literal_nonzero = matches!(&r.kind, NodeKind::Literal { .. })
                                    && !r.common.code.contains('0');
                                if !literal_nonzero {
                                    return true;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            children(g, id).into_iter().any(|c| expr_may_panic(g, c))
        }
        _ => children(g, id).into_iter().any(|c| expr_may_panic(g, c)),
    }
}

// ---------------------------------------------------------------------------
// CFG data model
// ---------------------------------------------------------------------------

/// Discriminant of a CFG node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CfgNodeKind {
    Entry,
    Exit,
    Panic,
    /// Statement-level flow node bound to an AST Statement node (or a block
    /// tail expression).
    Statement { ast: NodeId },
    /// Branch/decision node bound to an expression or match-arm AST node.
    Condition { ast: NodeId },
    /// Loop-variable binding node of a `for` loop.
    Iterate { ast: NodeId },
}

/// One CFG node, with source span + code of the bound AST node ("" virtual).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CfgNode {
    pub id: NodeId,
    pub kind: CfgNodeKind,
    pub span: SourceSpan,
    pub code: String,
}

/// The CFG for one function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cfg {
    pub function: NodeId,
    pub entry: NodeId,
    pub exit: NodeId,
    pub nodes: Vec<CfgNode>,
    pub edges: Vec<Edge>,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Labeled flow points: (cfg node, branch label on the edge INTO the next node).
type FlowPts = Vec<(NodeId, Option<String>)>;

struct CfgBuilder<'a> {
    g: &'a Graph,
    cfg: Cfg,
    next_cfg_id: NodeId,
    /// shared Panic cfg node (one per function, lazy).
    panic_node: Option<NodeId>,
    /// loop nesting stack: (break target, continue target).
    loops: Vec<(NodeId, NodeId)>,
}

/// Build the CFG for one function body (`fn_ast` is the Function node id).
/// Returns None if the function has no `body` (e.g. trait signatures).
pub fn build_function_cfg(g: &Graph, fn_ast: NodeId) -> Option<Cfg> {
    let body = ast_child(g, fn_ast, "body")?;
    let fn_span = g.nodes[(fn_ast - 1) as usize].common.span.clone();
    let mut b = CfgBuilder {
        g,
        cfg: Cfg {
            function: fn_ast,
            entry: 0,
            exit: 0,
            nodes: Vec::new(),
            edges: Vec::new(),
        },
        next_cfg_id: 1,
        panic_node: None,
        loops: Vec::new(),
    };
    let entry = b.new_node(CfgNodeKind::Entry, fn_span.clone(), String::new());
    let exit = b.new_node(CfgNodeKind::Exit, fn_span, String::new());
    b.cfg.entry = entry;
    b.cfg.exit = exit;
    let tail = b.walk_block(body, vec![(entry, None)]);
    for (p, l) in tail {
        b.edge(p, exit, l.as_deref());
    }
    Some(b.cfg)
}

impl<'a> CfgBuilder<'a> {
    fn new_node(&mut self, kind: CfgNodeKind, span: SourceSpan, code: String) -> NodeId {
        let id = self.next_cfg_id;
        self.next_cfg_id += 1;
        self.cfg.nodes.push(CfgNode { id, kind, span, code });
        id
    }

    fn span_of(&self, ast: NodeId) -> SourceSpan {
        self.g.nodes[(ast - 1) as usize].common.span.clone()
    }

    fn code_of(&self, ast: NodeId) -> String {
        node_code(self.g, ast)
    }

    fn edge(&mut self, src: NodeId, dst: NodeId, label: Option<&str>) {
        self.cfg.edges.push(Edge {
            id: self.cfg.edges.len() as u64 + 1,
            src,
            dst,
            kind: EdgeKind::Cfg {
                branch_label: label.map(|l| l.to_string()),
            },
        });
    }

    fn ensure_panic_node(&mut self) -> NodeId {
        if let Some(p) = self.panic_node {
            return p;
        }
        let span = self.span_of(self.cfg.function);
        let id = self.new_node(CfgNodeKind::Panic, span, String::new());
        self.panic_node = Some(id);
        id
    }

    // --- structure -----------------------------------------------------------

    /// Walk a block's contents in order starting from the labeled entry points;
    /// returns the labeled fall-through points.
    fn walk_block(&mut self, block: NodeId, from: FlowPts) -> FlowPts {
        let mut cur = from;
        let mut jumped = false;
        for st in children(self.g, block) {
            if jumped {
                // Unreachable statements keep their nodes but get no flow.
                self.walk_statement(st, Vec::new());
                continue;
            }
            cur = self.walk_statement(st, cur);
            jumped = cur.is_empty();
        }
        cur
    }

    /// Body of a construct: a Block (inline) or a single statement/expression.
    fn walk_body(&mut self, body: NodeId, from: FlowPts) -> FlowPts {
        if self.g.nodes[(body - 1) as usize].kind == NodeKind::Block {
            self.walk_block(body, from)
        } else {
            self.walk_statement_or_expr(body, from)
        }
    }

    /// A flow point that is not wrapped in a Statement node (arm result,
    /// block tail expression): gets its own flow node.
    fn walk_statement_or_expr(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        if stmt_kind(&self.g.nodes[(st - 1) as usize]).is_some() {
            return self.walk_statement(st, from);
        }
        let node = self.new_node(
            CfgNodeKind::Statement { ast: st },
            self.span_of(st),
            self.code_of(st),
        );
        self.edges_from(&from, node);
        self.maybe_panic_edge(st, node);
        vec![(node, None)]
    }

    fn edges_from(&mut self, from: &FlowPts, dst: NodeId) {
        for (f, l) in from {
            self.edge(*f, dst, l.as_deref());
        }
    }

    fn walk_statement(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let kind = stmt_kind(&self.g.nodes[(st - 1) as usize])
            .unwrap_or("")
            .to_string();
        // Control-flow expressions are wrapped in an expression_statement in
        // the AST layer; unwrap to the inner statement node.
        if kind == "expr" {
            if let Some(inner) = children(self.g, st)
                .into_iter()
                .find(|c| stmt_kind(&self.g.nodes[(c - 1) as usize]).is_some())
            {
                return self.walk_statement(inner, from);
            }
        }
        match kind.as_str() {
            "if" => self.walk_if(st, from),
            "match" => self.walk_match(st, from),
            "loop" => self.walk_loop(st, from),
            "while" => self.walk_while(st, from),
            "for" => self.walk_for(st, from),
            "return" => self.walk_return(st, from),
            "break" => self.walk_break(st, from),
            "continue" => self.walk_continue(st, from),
            _ => self.walk_plain(st, from),
        }
    }

    fn walk_plain(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let node = self.new_node(
            CfgNodeKind::Statement { ast: st },
            self.span_of(st),
            self.code_of(st),
        );
        // Always-panic statement: flow goes to Panic and stops.
        if children(self.g, st)
            .into_iter()
            .any(|c| contains_always_panic(self.g, c))
        {
            let p = self.ensure_panic_node();
            self.edges_from(&from, node);
            self.edge(node, p, Some("panic"));
            return Vec::new();
        }
        self.edges_from(&from, node);
        self.maybe_panic_edge(st, node);
        vec![(node, None)]
    }

    /// Best-effort maybe-panic edge: statement → Panic ("panic") as an EXTRA
    /// edge alongside normal flow.
    fn maybe_panic_edge(&mut self, st: NodeId, cfg_node: NodeId) {
        let panics = children(self.g, st)
            .into_iter()
            .any(|c| expr_may_panic(self.g, c));
        if panics {
            let p = self.ensure_panic_node();
            self.edge(cfg_node, p, Some("panic"));
        }
    }

    // --- if -------------------------------------------------------------------

    fn walk_if(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let cond_ast = ast_child(self.g, st, "condition").expect("if has condition");
        let cond = self.new_node(
            CfgNodeKind::Condition { ast: cond_ast },
            self.span_of(cond_ast),
            self.code_of(cond_ast),
        );
        self.edges_from(&from, cond);
        let mut out = Vec::new();
        // true branch
        let cons = ast_child(self.g, st, "consequence").expect("if has consequence");
        out.extend(self.walk_body(cons, vec![(cond, Some("true".into()))]));
        // false branch
        match ast_child(self.g, st, "alternative") {
            Some(alt) => {
                // else_clause raw node → its block child
                let alt_block = children(self.g, alt)
                    .into_iter()
                    .find(|c| self.g.nodes[(c - 1) as usize].kind == NodeKind::Block)
                    .unwrap_or(alt);
                out.extend(self.walk_body(alt_block, vec![(cond, Some("false".into()))]));
            }
            None => out.push((cond, Some("false".into()))), // no else: fall-through
        }
        out
    }

    // --- match ------------------------------------------------------------------

    fn walk_match(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let cond_ast = ast_child(self.g, st, "value").expect("match has value");
        let cond = self.new_node(
            CfgNodeKind::Condition { ast: cond_ast },
            self.span_of(cond_ast),
            self.code_of(cond_ast),
        );
        self.edges_from(&from, cond);
        let mut out = Vec::new();
        let body = ast_child(self.g, st, "body").expect("match has body");
        let mut dispatch = cond; // node that dispatches to the next arm
        for arm in children(self.g, body) {
            let arm_entry = self.new_node(
                CfgNodeKind::Condition { ast: arm },
                self.span_of(arm),
                self.code_of(arm),
            );
            self.edge(dispatch, arm_entry, None);
            let pattern = ast_child(self.g, arm, "pattern").expect("arm has pattern");
            let body_ast = ast_child(self.g, arm, "value").expect("arm has body");
            let label = self.code_of(pattern).trim().to_string();
            match ast_child(self.g, pattern, "condition")
                .or_else(|| ast_child(self.g, pattern, "guard"))
            {
                Some(guard) => {
                    let gnode = self.new_node(
                        CfgNodeKind::Condition { ast: guard },
                        self.span_of(guard),
                        self.code_of(guard),
                    );
                    self.edge(arm_entry, gnode, None);
                    out.extend(self.walk_body(body_ast, vec![(gnode, Some("true".into()))]));
                    dispatch = gnode; // guard miss → next arm
                }
                None => {
                    out.extend(self.walk_body(body_ast, vec![(arm_entry, Some(label))]));
                    dispatch = arm_entry; // pattern miss → next arm
                }
            }
        }
        out
    }

    // --- loops ---------------------------------------------------------------

    fn walk_loop(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let body = ast_child(self.g, st, "body").expect("loop has body");
        let head = self.new_node(
            CfgNodeKind::Condition { ast: st },
            self.span_of(st),
            self.code_of(st),
        );
        self.edges_from(&from, head);
        let exit = self.new_node(
            CfgNodeKind::Statement { ast: st },
            self.span_of(st),
            String::new(),
        );
        self.loops.push((exit, head));
        let t = self.walk_body(body, vec![(head, None)]);
        for p in t {
            self.edge(p.0, head, None); // back edge
        }
        self.loops.pop();
        vec![(exit, None)]
    }

    fn walk_while(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let cond_ast = ast_child(self.g, st, "condition").expect("while has condition");
        let cond = self.new_node(
            CfgNodeKind::Condition { ast: cond_ast },
            self.span_of(cond_ast),
            self.code_of(cond_ast),
        );
        self.edges_from(&from, cond);
        let body = ast_child(self.g, st, "body").expect("while has body");
        let exit = self.new_node(
            CfgNodeKind::Statement { ast: st },
            self.span_of(st),
            String::new(),
        );
        self.loops.push((exit, cond));
        let t = self.walk_body(body, vec![(cond, Some("true".into()))]);
        for p in t {
            self.edge(p.0, cond, None); // back edge
        }
        self.loops.pop();
        self.edge(cond, exit, Some("false"));
        vec![(exit, None)]
    }

    fn walk_for(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        // The pattern child can be absent: tree-sitter's wildcard `_` pattern
        // is an anonymous token, so the AST layer (which recurses into named
        // children only) has no `pattern` edge for `for _ in ...` loops.
        let pat_ast = ast_child(self.g, st, "pattern");
        let iter_ast = match ast_child(self.g, st, "value") {
            Some(v) => v,
            None => return self.walk_plain(st, from),
        };
        let body_ast = match ast_child(self.g, st, "body") {
            Some(b) => b,
            None => return self.walk_plain(st, from),
        };
        let pat_text = pat_ast.map(|p| self.code_of(p)).unwrap_or_else(|| "_".into());
        let iterate = self.new_node(
            CfgNodeKind::Iterate { ast: st },
            self.span_of(iter_ast),
            format!("for {} in {}", pat_text, self.code_of(iter_ast)),
        );
        self.edges_from(&from, iterate);
        let cond = self.new_node(
            CfgNodeKind::Condition { ast: iter_ast },
            self.span_of(iter_ast),
            self.code_of(iter_ast),
        );
        self.edge(iterate, cond, None);
        let exit = self.new_node(
            CfgNodeKind::Statement { ast: st },
            self.span_of(st),
            String::new(),
        );
        self.loops.push((exit, cond));
        let t = self.walk_body(body_ast, vec![(cond, Some("true".into()))]);
        for p in t {
            self.edge(p.0, cond, None); // back edge
        }
        self.loops.pop();
        self.edge(cond, exit, Some("false"));
        vec![(exit, None)]
    }

    // --- jumps ------------------------------------------------------------------

    fn walk_return(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let node = self.new_node(
            CfgNodeKind::Statement { ast: st },
            self.span_of(st),
            self.code_of(st),
        );
        self.edges_from(&from, node);
        self.edge(node, self.cfg.exit, Some("return"));
        self.maybe_panic_edge(st, node);
        Vec::new()
    }

    fn walk_break(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let node = self.new_node(
            CfgNodeKind::Statement { ast: st },
            self.span_of(st),
            self.code_of(st),
        );
        self.edges_from(&from, node);
        if let Some(&(break_target, _)) = self.loops.last() {
            self.edge(node, break_target, Some("break"));
        }
        Vec::new()
    }

    fn walk_continue(&mut self, st: NodeId, from: FlowPts) -> FlowPts {
        let node = self.new_node(
            CfgNodeKind::Statement { ast: st },
            self.span_of(st),
            self.code_of(st),
        );
        self.edges_from(&from, node);
        if let Some(&(_, cont_target)) = self.loops.last() {
            self.edge(node, cont_target, Some("continue"));
        }
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Folder-level driver: build CFGs for every function in a graph
// ---------------------------------------------------------------------------

/// Build CFGs for all functions in `g`, in graph node order.
pub fn build_cfgs(g: &Graph) -> Vec<Cfg> {
    let mut out = Vec::new();
    for n in &g.nodes {
        if matches!(&n.kind, NodeKind::Function { .. }) && ast_child(g, n.id, "body").is_some() {
            if let Some(cfg) = build_function_cfg(g, n.id) {
                out.push(cfg);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Attach CFG edges back into the shared Graph (schema "cfg" namespace)
// ---------------------------------------------------------------------------

/// Attach CFG edges to the shared graph. CFG nodes are NOT added to the
/// shared node set (the AST Statement nodes are the shared nodes); each
/// `EdgeKind::Cfg` edge is emitted as AST-node → AST-node where both
/// endpoints are bound to AST statements. Edges touching virtual nodes
/// (Entry/Exit/Panic, loop-exit nodes with empty code) are dropped —
/// their semantics is already captured (return edges go to Exit, etc.).
pub fn attach_cfg_edges(g: &mut Graph, cfg: &Cfg) {
    for e in &cfg.edges {
        let src_ast = cfg_node_ast(cfg, e.src);
        let dst_ast = cfg_node_ast(cfg, e.dst);
        if let (Some(s), Some(d)) = (src_ast, dst_ast) {
            g.add_edge(Edge {
                id: g.edges.len() as u64 + 1,
                src: s,
                dst: d,
                kind: e.kind.clone(),
            });
        }
    }
}

fn cfg_node_ast(cfg: &Cfg, id: NodeId) -> Option<NodeId> {
    cfg.nodes
        .iter()
        .find(|n| n.id == id)
        .and_then(|n| match &n.kind {
            CfgNodeKind::Statement { ast }
            | CfgNodeKind::Condition { ast }
            | CfgNodeKind::Iterate { ast } => Some(*ast),
            _ => None,
        })
}
