//! CPG AST layer builder for Rust, via tree-sitter-rust.
//!
//! Maps the tree-sitter CST onto the CPG schema (see docs/cpg-schema.md and
//! the grammar-evaluation spike mapping table, t_49c7d4f7 REPORT.md §3).
//! Normalization principles:
//! - map named node kinds per the mapping table; skip anonymous punctuators;
//! - every node carries a full SourceSpan (byte range authoritative,
//!   line/col derived at parse time) and verbatim `code`;
//! - macro invocations become opaque `MacroInvocation` nodes annotated with
//!   name + span for a future expansion pass;
//! - errors never crash the builder: tree-sitter ERROR/MISSING subtrees are
//!   surfaced as degraded nodes and counted per file;
//! - unmapped named kinds degrade to `Expression { expr_kind: "raw:<kind>" }`
//!   so no structure is lost.
//!
//! Front-end contract (schema doc §8): this builder emits Nodes + Ast edges
//! (plus ModuleDependency for `mod x;` → file resolution); all other overlays
//! are built later by language-agnostic passes.

use crate::{cfg, dominators, overlays};
use crate::schema::{
    Edge, EdgeKind, Graph, Node, NodeCommon, NodeId, NodeKind, SourceSpan, TypeRelation,
};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tree_sitter::{Node as TsNode, Tree};

// ---------------------------------------------------------------------------
// File discovery / crate root
// ---------------------------------------------------------------------------

/// Walk a folder for `*.rs` files, gitignore-aware, skipping build dirs.
pub fn discover_rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            n != "target" && n != "build" && n != "dist"
        })
        .build();
    for entry in walker.flatten() {
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            && entry.path().extension().map(|e| e == "rs").unwrap_or(false)
        {
            out.push(entry.path().to_path_buf());
        }
    }
    out.sort();
    out
}

/// Crate root discovery: `main.rs` / `lib.rs` at root or under `src/`,
/// else top-level files, else every discovered file.
pub fn find_crate_roots(root: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut candidates = Vec::new();
    for base in [root.to_path_buf(), root.join("src")] {
        candidates.push(base.join("main.rs"));
        candidates.push(base.join("lib.rs"));
    }
    for c in candidates {
        if c.is_file() {
            roots.push(c);
        }
    }
    if roots.is_empty() {
        roots = files
            .iter()
            .filter(|f| f.parent() == Some(root))
            .cloned()
            .collect();
        if roots.is_empty() {
            roots = files.to_vec();
        }
    }
    roots
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct BuildStats {
    pub files_parsed: u32,
    pub parse_errors: u32,
    pub nodes: u64,
    pub ast_edges: u64,
    pub macro_invocations: u64,
    pub degraded_nodes: u64,
    pub module_links: u64,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

struct Builder<'a> {
    graph: Graph,
    src: &'a [u8],
    file_rel: String,
    stats: BuildStats,
    next_id: NodeId,
    /// top-level type name -> node id of its Type node, for impl association.
    type_nodes: HashMap<String, NodeId>,
}

impl<'a> Builder<'a> {
    fn new(src: &'a [u8], file_rel: String) -> Self {
        Self {
            graph: Graph::default(),
            src,
            file_rel,
            stats: BuildStats::default(),
            next_id: 1,
            type_nodes: HashMap::new(),
        }
    }

    fn span_of(&self, n: TsNode) -> SourceSpan {
        let sp = n.start_position();
        SourceSpan {
            file: self.file_rel.clone(),
            start_byte: n.start_byte() as u64,
            end_byte: n.end_byte() as u64,
            start_line: (sp.row + 1) as u32,
            start_col: sp.column as u32,
        }
    }

    fn code_of(&self, n: TsNode) -> String {
        String::from_utf8_lossy(&self.src[n.start_byte()..n.end_byte()]).into_owned()
    }

    fn add_node(&mut self, n: TsNode, order: u32, kind: NodeKind) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        let common = NodeCommon {
            span: self.span_of(n),
            code: self.code_of(n),
            order,
        };
        self.graph.nodes.push(Node { id, common, kind });
        self.stats.nodes += 1;
        id
    }

    fn add_ast_edge(&mut self, src: NodeId, dst: NodeId, order: u32, field: Option<&str>) {
        let id = self.graph.edges.len() as u64 + 1;
        self.graph.edges.push(Edge {
            id,
            src,
            dst,
            kind: EdgeKind::Ast {
                order,
                field: field.map(|f| f.to_string()),
            },
        });
        self.stats.ast_edges += 1;
    }

    fn add_extra_edge(&mut self, src: NodeId, dst: NodeId, kind: EdgeKind) {
        let id = self.graph.edges.len() as u64 + 1;
        self.graph.edges.push(Edge { id, src, dst, kind });
    }

    fn field_text(&self, n: TsNode, field: &str) -> Option<String> {
        let c = n.child_by_field_name(field)?;
        Some(String::from_utf8_lossy(&self.src[c.start_byte()..c.end_byte()]).into_owned())
    }
}

/// Entry: parse one source string into a Graph (single file).
pub fn parse_file_to_graph(path_rel: &str, src: &[u8]) -> Result<(Graph, BuildStats), String> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|e| format!("grammar error: {e}"))?;
    let tree: Tree = parser
        .parse(src, None)
        .ok_or_else(|| "tree-sitter returned None".to_string())?;
    let mut b = Builder::new(src, path_rel.to_string());
    b.stats.files_parsed = 1;
    if tree.root_node().has_error() {
        b.stats.parse_errors = 1;
        b.stats.degraded_nodes += count_error_nodes(tree.root_node());
    }
    let root = b.add_node(
        tree.root_node(),
        0,
        NodeKind::File {
            path: path_rel.to_string(),
            content_hash: fnv1a(src),
        },
    );
    build_ts_node(&mut b, tree.root_node(), root, 0, None);
    Ok((b.graph, b.stats))
}

fn fnv1a(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for &byte in data {
        h ^= byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn count_error_nodes(root: TsNode) -> u64 {
    let mut c = root.walk();
    let mut count = 0u64;
    loop {
        let n = c.node();
        if n.is_error() || n.is_missing() {
            count += 1;
        }
        if c.goto_first_child() {
            continue;
        }
        loop {
            if c.goto_next_sibling() {
                break;
            }
            if !c.goto_parent() {
                return count;
            }
        }
    }
}

/// Map a tree-sitter kind to a CPG node kind per the mapping table.
fn map_kind(b: &mut Builder, n: TsNode, field: Option<&str>) -> Option<NodeKind> {
    use NodeKind as NK;
    let kind_str = n.kind();
    match kind_str {
        "source_file" => None, // materialized as the File root
        "mod_item" | "mod_declaration" => {
            let name = b.field_text(n, "name").unwrap_or_default();
            Some(NK::Module { name })
        }
        "struct_item" | "enum_item" | "union_item" | "type_item" | "trait_item" => {
            let name = b.field_text(n, "name").unwrap_or_default();
            Some(NK::Type { name })
        }
        "function_item" | "function_signature_item" => {
            let name = b.field_text(n, "name").unwrap_or_default();
            let signature = b.field_text(n, "parameters").unwrap_or_default();
            Some(NK::Function { name, signature })
        }
        "closure_expression" => Some(NK::Function {
            name: "<closure>".into(),
            signature: String::new(),
        }),
        "block" => Some(NK::Block),
        "let_declaration" => Some(NK::Statement { stmt_kind: "let".into() }),
        "expression_statement" => Some(NK::Statement { stmt_kind: "expr".into() }),
        "if_expression" => Some(NK::Statement { stmt_kind: "if".into() }),
        "match_expression" => Some(NK::Statement { stmt_kind: "match".into() }),
        "for_expression" => Some(NK::Statement { stmt_kind: "for".into() }),
        "while_expression" => Some(NK::Statement { stmt_kind: "while".into() }),
        "loop_expression" => Some(NK::Statement { stmt_kind: "loop".into() }),
        "return_expression" => Some(NK::Statement { stmt_kind: "return".into() }),
        "break_expression" => Some(NK::Statement { stmt_kind: "break".into() }),
        "continue_expression" => Some(NK::Statement { stmt_kind: "continue".into() }),
        "call_expression" | "method_call_expression" => Some(NK::Expression {
            expr_kind: "call".into(),
            type_name: None,
        }),
        "binary_expression" => Some(NK::Expression {
            expr_kind: "binary".into(),
            type_name: None,
        }),
        "unary_expression" => Some(NK::Expression {
            expr_kind: "unary".into(),
            type_name: None,
        }),
        "assignment_expression" | "compound_assignment_expr" => Some(NK::Expression {
            expr_kind: "assignment".into(),
            type_name: None,
        }),
        "field_expression" => Some(NK::Expression {
            expr_kind: "field".into(),
            type_name: None,
        }),
        "index_expression" => Some(NK::Expression {
            expr_kind: "index".into(),
            type_name: None,
        }),
        "range_expression" => Some(NK::Expression {
            expr_kind: "range".into(),
            type_name: None,
        }),
        "await_expression" => Some(NK::Expression {
            expr_kind: "await".into(),
            type_name: None,
        }),
        "try_expression" => Some(NK::Expression {
            expr_kind: "try".into(),
            type_name: None,
        }),
        "reference_expression" => Some(NK::Expression {
            expr_kind: "reference".into(),
            type_name: None,
        }),
        "dereference_expression" => Some(NK::Expression {
            expr_kind: "dereference".into(),
            type_name: None,
        }),
        "array_expression" => Some(NK::Expression {
            expr_kind: "array".into(),
            type_name: None,
        }),
        "tuple_expression" => Some(NK::Expression {
            expr_kind: "tuple".into(),
            type_name: None,
        }),
        "struct_expression" => Some(NK::Expression {
            expr_kind: "struct".into(),
            type_name: None,
        }),
        "identifier" | "field_identifier" | "shorthand_field_initializer" => {
            if field == Some("pattern") {
                // declaring occurrence inside let_declaration / parameter
                Some(NK::Declaration { name: b.code_of(n), is_parameter: false })
            } else {
                Some(NK::Identifier { name: b.code_of(n) })
            }
        }
        "parameter" | "self_parameter" | "let_condition" => {
            let name = b
                .field_text(n, "pattern")
                .or_else(|| b.field_text(n, "name"))
                .unwrap_or_else(|| b.code_of(n));
            let is_parameter = kind_str == "parameter" || kind_str == "self_parameter";
            Some(NK::Declaration { name, is_parameter })
        }
        "static_item" | "const_item" => Some(NK::Declaration {
            name: b.field_text(n, "name").unwrap_or_default(),
            is_parameter: false,
        }),
        "primitive_type" | "generic_type" | "reference_type" | "pointer_type" | "tuple_type"
        | "function_type" | "type_identifier" => Some(NK::Type { name: b.code_of(n) }),
        "string_literal" | "raw_string_literal" | "char_literal" => Some(NK::Literal {
            value: b.code_of(n),
            literal_kind: "string".into(),
        }),
        "integer_literal" => Some(NK::Literal {
            value: b.code_of(n),
            literal_kind: "integer".into(),
        }),
        "float_literal" => Some(NK::Literal {
            value: b.code_of(n),
            literal_kind: "float".into(),
        }),
        "boolean_literal" => Some(NK::Literal {
            value: b.code_of(n),
            literal_kind: "bool".into(),
        }),
        "macro_invocation" => {
            b.stats.macro_invocations += 1;
            Some(NK::MacroInvocation {
                name: b.field_text(n, "macro").unwrap_or_default(),
            })
        }
        "macro_definition" => {
            // macro_rules! definitions are opaque too (spike §2).
            b.stats.macro_invocations += 1;
            Some(NK::MacroInvocation {
                name: "macro_rules!".into(),
            })
        }
        "use_declaration" => Some(NK::Import {
            path: b
                .field_text(n, "argument")
                .unwrap_or_else(|| b.code_of(n)),
            import_kind: "use".into(),
        }),
        "attribute_item" | "attribute" => Some(NK::Expression {
            expr_kind: "attribute".into(),
            type_name: None,
        }),
        "line_comment" | "block_comment" => None, // optional layer, dropped
        _ => Some(NK::Expression {
            expr_kind: format!("raw:{kind_str}"),
            type_name: None,
        }),
    }
}

/// Build one tree-sitter node, attach to `parent`, recurse into children.
/// Returns the CPG node id this subtree's children attach to.
fn build_ts_node(
    b: &mut Builder,
    n: TsNode,
    parent: NodeId,
    order: u32,
    field: Option<&str>,
) -> NodeId {
    // impl_item is a container with no CPG node (mapping table): its methods
    // attach to the impl'd type node; the `trait` field yields a
    // TypeHierarchy::SubtypeOf edge.
    if n.kind() == "impl_item" {
        let attach = resolve_impl_type_node(b, n, parent, order);
        if let Some(trait_txt) = b.field_text(n, "trait") {
            let trait_txt = trait_txt.trim().trim_end_matches('<').split('<').next().unwrap_or("").trim().to_string();
            if !trait_txt.is_empty() {
                let trait_id = ensure_type_node(b, n, trait_txt, attach);
                b.add_extra_edge(
                    attach,
                    trait_id,
                    EdgeKind::TypeHierarchy { relation: TypeRelation::SubtypeOf },
                );
            }
        }
        dispatch_children(b, n, attach);
        return attach;
    }

    if n.is_error() || n.is_missing() {
        // Degraded node: keep structure, mark as raw expression.
        let id = b.add_node(n, order, NodeKind::Expression {
            expr_kind: format!("error:{}", n.kind()),
            type_name: None,
        });
        b.add_ast_edge(parent, id, order, field);
        dispatch_children(b, n, id);
        return id;
    }

    let node_kind: Option<NodeKind> = if n.kind() == "source_file" {
        None
    } else {
        match map_kind(b, n, field) {
            Some(k) => Some(k),
            None => {
                // unmapped/error handled above; comments etc. dropped
                dispatch_children(b, n, parent);
                return parent;
            }
        }
    };

    let this = match node_kind {
        Some(k) => {
            let is_type_node = matches!(&k, NodeKind::Type { .. });
            let id = b.add_node(n, order, k);
            b.add_ast_edge(parent, id, order, field);
            // register top-level type nodes for impl association
            if is_type_node {
                if let NodeKind::Type { name } = &b.graph.nodes.last().unwrap().kind {
                    let is_top_level = matches!(
                        n.parent().map(|p| p.kind()),
                        Some("source_file") | Some("mod_item") | Some("declaration_list")
                    );
                    if is_top_level && !name.is_empty() {
                        b.type_nodes.insert(name.clone(), id);
                    }
                }
            }
            id
        }
        None => parent,
    };

    dispatch_children(b, n, this);
    this
}

/// Find or create the Type node that impl methods attach to.
fn resolve_impl_type_node(b: &mut Builder, n: TsNode, parent: NodeId, _order: u32) -> NodeId {
    let type_txt = b
        .field_text(n, "type")
        .map(|t| {
            t.trim()
                .trim_start_matches('&')
                .trim()
                .split('<')
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    if let Some(&id) = b.type_nodes.get(&type_txt) {
        return id;
    }
    // Type not declared (yet) in this file: synthesize a Type node at the impl.
    ensure_type_node(b, n, type_txt, parent)
}

fn ensure_type_node(b: &mut Builder, n: TsNode, name: String, parent: NodeId) -> NodeId {
    if let Some(&id) = b.type_nodes.get(&name) {
        return id;
    }
    if name.is_empty() {
        return parent;
    }
    let id = b.add_node(n, 0, NodeKind::Type { name: name.clone() });
    b.type_nodes.insert(name, id);
    id
}

/// Recurse into named children with a tree-sitter cursor (gets field names).
fn dispatch_children(b: &mut Builder, n: TsNode, parent: NodeId) {
    let mut cursor = n.walk();
    let mut child_order = 0u32;
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            let field = cursor.field_name().map(|s| s.to_string());
            if child.is_named() {
                build_ts_node(b, child, parent, child_order, field.as_deref());
                child_order += 1;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Folder driver
// ---------------------------------------------------------------------------

/// Build the AST-layer CPG for a folder of Rust code, merging per-file graphs.
/// `mod x;` modules link to `x.rs` / `x/mod.rs` via ModuleDependency edges
/// (front-end extension; the module tree is materialized as Module nodes).
pub fn build_folder(folder: &Path) -> Result<(Graph, BuildStats), String> {
    let files = discover_rust_files(folder);
    if files.is_empty() {
        return Err(format!("no .rs files found under {}", folder.display()));
    }
    let mut total = BuildStats::default();
    let mut graph = Graph::default();
    let mut module_nodes: HashMap<String, NodeId> = HashMap::new();
    let mut file_nodes: HashMap<String, NodeId> = HashMap::new();

    for f in &files {
        let rel = f
            .strip_prefix(folder)
            .unwrap_or(f)
            .to_string_lossy()
            .to_string();
        let src = fs::read(f).map_err(|e| format!("read {}: {e}", f.display()))?;
        let (g, stats) = parse_file_to_graph(&rel, &src)?;
        total.files_parsed += stats.files_parsed;
        total.parse_errors += stats.parse_errors;
        total.nodes += stats.nodes;
        total.ast_edges += stats.ast_edges;
        total.macro_invocations += stats.macro_invocations;
        total.degraded_nodes += stats.degraded_nodes;

        let id_base = graph.nodes.len() as u64;
        for mut node in g.nodes {
            node.id += id_base;
            if let NodeKind::File { .. } = &node.kind {
                file_nodes.insert(rel.clone(), node.id);
            }
            if let NodeKind::Module { name } = &node.kind {
                module_nodes.insert(name.clone(), node.id);
            }
            graph.nodes.push(node);
        }
        let edge_base = graph.edges.len() as u64;
        for mut edge in g.edges {
            edge.id += edge_base;
            edge.src += id_base;
            edge.dst += id_base;
            graph.edges.push(edge);
        }
    }

    // mod x; -> file resolution
    for (mod_name, &mod_id) in &module_nodes {
        for (file_rel, &file_id) in &file_nodes {
            let stem = Path::new(file_rel)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let is_mod_rs = file_rel.ends_with("/mod.rs")
                && file_rel.rsplit('/').nth(1) == Some(mod_name.as_str());
            if stem == *mod_name || is_mod_rs {
                graph.edges.push(Edge {
                    id: graph.edges.len() as u64 + 1,
                    src: mod_id,
                    dst: file_id,
                    kind: EdgeKind::ModuleDependency { import_path: file_rel.clone() },
                });
                total.module_links += 1;
            }
        }
    }
    Ok((graph, total))
}

/// Serialize the graph to JSON (v1 storage, schema doc §7).
pub fn to_json(graph: &Graph) -> String {
    serde_json::to_string_pretty(graph).expect("graph serializes")
}

/// Deserialize a graph from JSON.
pub fn from_json(json: &str) -> Result<Graph, String> {
    serde_json::from_str(json).map_err(|e| format!("json: {e}"))
}

/// Build the full pipeline: AST layer + DFG/call/import overlays (t_8306ad86)
/// + CFG (t_17450c16) + dominator tree, all attached to the shared node set.
///
/// Order matters: overlays and CFG attachment both re-index the graph, so each
/// pass sees the fully-extended graph of the previous ones. Overlay stats and
/// the per-function CFG count are returned alongside BuildStats.
pub fn build_folder_full(
    folder: &Path,
) -> Result<(Graph, BuildStats, overlays::OverlayStats, usize), String> {
    let (mut graph, stats) = build_folder(folder)?;
    let ostats = overlays::build_overlays(&mut graph, folder)?;
    let cfgs = cfg::build_cfgs(&graph);
    let n_cfgs = cfgs.len();
    for c in &cfgs {
        cfg::attach_cfg_edges(&mut graph, c);
    }
    for c in &cfgs {
        dominators::attach_dominator_edges(&mut graph, c, &dominators::AttachOptions::default());
    }
    Ok((graph, stats, ostats, n_cfgs))
}

