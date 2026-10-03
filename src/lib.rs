//! CPG crate: Rust AST layer (tree-sitter front-end) plus control-flow overlays.
//!
//! - `ast_builder`: the approved Rust AST-layer builder from t_75f6e158
//!   (Files/Modules/Functions/Statements/Expressions/Identifiers with
//!   byte-authoritative spans, Ast edges, ModuleDependency/TypeHierarchy).
//! - `schema`: the shared node set + namespaced edge overlays (t_ebf8bd37).
//! - `cfg`: CFG construction over the AST layer (statement-level flow graph,
//!   entry/exit nodes, branch-labelled edges, loops, break/continue/return,
//!   best-effort panic edges) — task t_17450c16.
//! - `dominators`: Lengauer-Tarjan immediate dominators over a CFG, attached
//!   in the `dominators` edge namespace (schema `Dominate` edges) via
//!   `dominators::attach_dominator_edges` (also post-dominators).

pub mod cfg;
pub mod dominators;
pub mod inspect;
pub mod overlays;
pub mod schema;
pub mod ast_builder;

pub use ast_builder::{
    build_folder, build_folder_full, discover_rust_files, find_crate_roots, from_json,
    parse_file_to_graph, to_json, BuildStats,
};
