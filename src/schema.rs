//! Core CPG schema types — language-agnostic data model for the
//! Code Property Graph (Yamaguchi et al. 2014; Fraunhofer AISEC cpg 2022).
//!
//! One shared node set (all `NodeKind`s), overlays as namespaced edge sets.
//!
//! Upstream: copied verbatim from the schema task's crate (t_ebf8bd37, docs/cpg-schema.md).

use serde::{Deserialize, Serialize};

/// Global identifier for a node. Stable across serializations because it is
/// derived deterministically (per-file counter at build time; deduped by the
/// builder within one graph).
pub type NodeId = u64;

/// Stable identifier for an edge *instance* (source, target, kind, label, order).
pub type EdgeId = u64;

/// A source span: file path plus byte range (0-based, half-open) and 1-based
/// line/column start. Byte range is authoritative; line/col is a convenience
/// for tooling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    /// Path of the containing file, relative to the analyzed root.
    pub file: String,
    /// Start byte offset into the file content (0-based, inclusive).
    pub start_byte: u64,
    /// End byte offset (exclusive).
    pub end_byte: u64,
    /// 1-based start line.
    pub start_line: u32,
    /// 0-based start column (byte offset within line).
    pub start_col: u32,
}

/// Common payload on every node, mirroring the CPG `AST_NODE` base
/// properties (CODE, ORDER, LINE_NUMBER, COLUMN_NUMBER, OFFSET, OFFSET_END).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCommon {
    pub span: SourceSpan,
    /// Verbatim source text this node represents (CPG `CODE`).
    pub code: String,
    /// Position among siblings in the AST (CPG `ORDER`).
    pub order: u32,
}

/// Node types. Every node in the graph is one of these; overlays share the set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
pub enum NodeKind {
    /// A source file (translation unit / FILE).
    File { path: String, content_hash: String },
    /// A module / package / namespace (namespace, mod, TypeScript module).
    Module { name: String },
    /// A named function or method (METHOD).
    Function { name: String, signature: String },
    /// A compound statement / block (BLOCK).
    Block,
    /// A statement (STATEMENT-family: control structures, returns, etc.).
    Statement {
        /// e.g. "if", "for", "return", "let"; language-specific discriminants
        /// allowed via free-form string.
        stmt_kind: String,
    },
    /// An expression (EXPRESSION-family: operators, calls, casts...).
    Expression {
        expr_kind: String,
        /// Inferred/statically known type name if available (optional).
        type_name: Option<String>,
    },
    /// Identifier / variable reference (IDENTIFIER, FIELD_IDENTIFIER).
    Identifier { name: String },
    /// Variable or parameter declaration (identifiers that declare).
    Declaration { name: String, is_parameter: bool },
    /// A type reference or declaration (TYPE).
    Type { name: String },
    /// A literal (LITERAL).
    Literal { value: String, literal_kind: String },
    /// Macro invocation site, opaque until expanded (language-specific).
    MacroInvocation { name: String },
    /// Import / use declaration linking Module dependency edges.
    Import { path: String, import_kind: String },
}

/// The full node: kind plus common properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub common: NodeCommon,
    pub kind: NodeKind,
}

/// Edge namespaces. Each variant belongs to one overlay; the union is the
/// multi-graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "overlay")]
pub enum EdgeKind {
    /// AST parent-child (CPG `AST` edge). Parent -> child.
    Ast {
        /// Sibling order, duplicated from NodeCommon::order for fast lookup.
        order: u32,
        /// Named tree-sitter field, e.g. "name", "body", "condition".
        field: Option<String>,
    },
    /// Control-flow / evaluation-order edge (CPG `CFG`, AISEC "EOG").
    Cfg {
        /// Branch-condition label: Some("true"/"false") at branching points,
        /// None on straight-line flow.
        branch_label: Option<String>,
    },
    /// Data dependence, definition -> use (CPG `REACHING_DEF` / DFG edge).
    DataDependence {
        /// Name of the variable flowing through this edge.
        variable: String,
    },
    /// Call edge, call site -> (best-effort) callee (CPG `CALL` / `INVOKES`).
    Call {
        /// Static (monomorphic) resolution if known; None when dynamic.
        resolved: bool,
        /// 0-based argument index when this edge carries an argument,
        /// None for the call node itself.
        argument_index: Option<u32>,
    },
    /// Dominator tree edge (CPG `DOMINATE`): source immediately dominates target.
    Dominate,
    /// Post-dominator tree edge (CPG `POST_DOMINATE`).
    PostDominate,
    /// Type hierarchy edge, subtype -> supertype (contains-relations or
    /// inheritance; AISEC type sub-graph).
    TypeHierarchy { relation: TypeRelation },
    /// Import / module dependency (file or module depends on another).
    ModuleDependency { import_path: String },
}

/// Relation kinds inside the type hierarchy overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeRelation {
    /// Inherits from / implements.
    SubtypeOf,
    /// Contains a member of this type.
    Contains,
}

/// An edge: source, target, kind, plus overlay-specific properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub src: NodeId,
    pub dst: NodeId,
    pub kind: EdgeKind,
}

/// The whole graph: one shared node set + a set of named overlays.
/// New overlays can be added later without schema migration because
/// `EdgeKind` is a closed enum versioned by graph format (see design doc).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    /// Overlay name -> edges. AST overlay uses key "ast"; further overlays
    /// may register their own namespaced sets at runtime.
    pub edges: Vec<Edge>,
    pub nodes: Vec<Node>,
}

impl Graph {
    pub fn add_node(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        self.nodes.len() as u64
    }

    pub fn add_edge(&mut self, edge: Edge) -> EdgeId {
        self.edges.push(edge);
        self.edges.len() as u64
    }

    /// Filter edges by overlay name, derived from the EdgeKind discriminator.
    pub fn edges_by_overlay<'a>(&'a self, overlay: &'a str) -> impl Iterator<Item = &'a Edge> + use<'a> {
        self.edges.iter().filter(move |e| {
            let kind = match &e.kind {
                EdgeKind::Ast { .. } => "ast",
                EdgeKind::Cfg { .. } => "cfg",
                EdgeKind::DataDependence { .. } => "data-dep",
                EdgeKind::Call { .. } => "call",
                EdgeKind::Dominate => "dominators",
                EdgeKind::PostDominate => "dominators",
                EdgeKind::TypeHierarchy { .. } => "type-hierarchy",
                EdgeKind::ModuleDependency { .. } => "module-dep",
            };
            kind == overlay
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(file: &str) -> SourceSpan {
        SourceSpan { file: file.into(), start_byte: 0, end_byte: 10, start_line: 1, start_col: 0 }
    }

    fn node(id: NodeId) -> Node {
        Node {
            id,
            common: NodeCommon { span: span("src/main.rs"), code: "x".into(), order: 0 },
            kind: NodeKind::Identifier { name: "x".into() },
        }
    }

    #[test]
    fn ast_overlay_and_roundtrip() {
        let mut g = Graph::default();
        g.add_node(node(1));
        g.add_node(node(2));
        g.add_edge(Edge { id: 1, src: 1, dst: 2, kind: EdgeKind::Ast { order: 0, field: Some("body".into()) } });
        g.add_edge(Edge { id: 2, src: 1, dst: 2, kind: EdgeKind::Cfg { branch_label: Some("true".into()) } });
        g.add_edge(Edge { id: 3, src: 1, dst: 2, kind: EdgeKind::DataDependence { variable: "x".into() } });
        assert_eq!(g.edges_by_overlay("ast").count(), 1);
        assert_eq!(g.edges_by_overlay("cfg").count(), 1);
        assert_eq!(g.edges_by_overlay("data-dep").count(), 1);

        let json = serde_json::to_string(&g).unwrap();
        let back: Graph = serde_json::from_str(&json).unwrap();
        assert_eq!(back.nodes.len(), g.nodes.len());
        assert_eq!(back.edges.len(), g.edges.len());
        assert_eq!(back.edges[0].kind, g.edges[0].kind);
    }
}
