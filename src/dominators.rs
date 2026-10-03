//! Dominator tree computation via the Lengauer–Tarjan algorithm
//! (Lengauer & Tarjan 1979, "A Fast Algorithm for Finding Dominators in a
//! Directed Graph"), implemented from scratch — no external dominator crate.
//!
//! Entry points:
//! - `dom_edges(edges, root)`  → immediate-dominator pairs on the forward graph;
//! - `postdom_edges(edges, exit)` → the same on the reversed graph
//!   (post-dominators).
//!
//! Textbook LT with explicit `label` arrays and iterative path compression.
//! Nodes unreachable from the root are excluded (they have no idom).

use crate::cfg::{Cfg, CfgNodeKind};
use crate::schema::{Edge, EdgeKind, Graph, NodeId};
use std::collections::{HashMap, HashSet};

pub type Gid = u64;

/// Compute the immediate-dominator map (node -> idom) for `edges`, rooted at
/// `root`. `root` itself is absent from the map.
pub fn idom_map(edges: &[(Gid, Gid)], root: Gid) -> HashMap<Gid, Gid> {
    let (succ, pred): (HashMap<Gid, Vec<Gid>>, HashMap<Gid, Vec<Gid>>) = adjacency(edges);

    // ---- 1. DFS from root: preorder numbering, DFS parents ----------------
    let mut preorder: Vec<Gid> = Vec::new(); // preorder index -> node
    let mut pre_index: HashMap<Gid, usize> = HashMap::new();
    let mut parent: Vec<usize> = Vec::new(); // preorder idx -> dfs parent idx
    let mut stack: Vec<(Gid, usize)> = vec![(root, usize::MAX)];
    // Iterative preorder, children in edge order (sort successors for
    // determinism regardless of edge insertion order).
    while let Some((v, p)) = stack.pop() {
        if pre_index.contains_key(&v) {
            continue;
        }
        let idx = preorder.len();
        pre_index.insert(v, idx);
        preorder.push(v);
        parent.push(p);
        let mut succs: Vec<Gid> = succ.get(&v).cloned().unwrap_or_default();
        succs.sort_unstable();
        for w in succs.into_iter().rev() {
            if !pre_index.contains_key(&w) {
                stack.push((w, idx));
            }
        }
    }
    let n = preorder.len();

    // Predecessors restricted to the DFS-visited set, as preorder indices.
    let mut pred_idx: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (v_idx, v) in preorder.iter().enumerate() {
        for u in pred.get(v).cloned().unwrap_or_default() {
            if let Some(&ui) = pre_index.get(&u) {
                pred_idx[v_idx].push(ui);
            }
        }
    }

    // ---- 2. Lengauer–Tarjan ----------------------------------------------
    // Arrays indexed by preorder index.
    let mut semi: Vec<usize> = (0..n).collect(); // semi-dominator (preorder idx)
    let mut ancestor: Vec<usize> = (0..n).collect(); // forest parent
    let mut label: Vec<usize> = (0..n).collect(); // min-semi vertex on path
    let mut idom: Vec<Option<usize>> = vec![None; n];
    let mut bucket: Vec<Vec<usize>> = vec![Vec::new(); n]; // semi[w] -> {w}

    /// Path-compressing `eval`: returns the vertex with minimal semi on the
    /// path from v to its forest root.
    fn eval(ancestor: &mut Vec<usize>, label: &mut [usize], semi: &[usize], v: usize) -> usize {
        if ancestor[v] == v {
            return label[v];
        }
        // Walk to the root collecting the path, then compress bottom-up.
        let mut path: Vec<usize> = Vec::new();
        let mut x = v;
        while ancestor[x] != x {
            path.push(x);
            x = ancestor[x];
        }
        let root = x;
        // label of the root is authoritative for the whole path.
        let mut best = label[root];
        for &p in path.iter().rev() {
            if semi[label[p]] < semi[best] {
                best = label[p];
            }
            ancestor[p] = root;
        }
        label[v] = best;
        best
    }

    // Step 1–3: process vertices in reverse preorder; compute semi-dominators.
    for v in (1..n).rev() {
        let mut s = v;
        for &u in &pred_idx[v] {
            let su = eval(&mut ancestor, &mut label, &semi, u);
            if semi[su] < semi[s] {
                s = su;
            }
        }
        semi[v] = s;
        bucket[s].push(v);
        let pv = parent[v];
        if pv != usize::MAX {
            ancestor[v] = pv;
        }
        // Step 4: process bucket of the parent — each vertex w whose semi is
        // the parent's preorder index; entries must be unique (dedupe).
        if pv != usize::MAX {
            let mut done: Vec<usize> = Vec::new();
            for &w in bucket[pv].clone().iter() {
                if idom[w].is_some() {
                    continue;
                }
                let e = eval(&mut ancestor, &mut label, &semi, w);
                idom[w] = if semi[e] < semi[w] { Some(e) } else { Some(pv) };
                done.push(w);
            }
            for &w in &done {
                bucket[pv].retain(|&x| x != w);
            }
        }
    }

    // Step 5: convert semi-dominator placeholders into true idoms, preorder.
    for v in 1..n {
        match idom[v] {
            Some(d) if d != semi[v] => idom[v] = idom[d],
            _ => idom[v] = Some(semi[v]),
        }
    }

    let mut out = HashMap::new();
    for v in 1..n {
        if let Some(d) = idom[v] {
            out.insert(preorder[v], preorder[d]);
        }
    }
    out
}

/// Immediate-dominator edge pairs (dominator, dominated), excluding the root.
pub fn dom_edges(edges: &[(Gid, Gid)], root: Gid) -> Vec<(Gid, Gid)> {
    // idom_map yields (node, idom); flip to (idom, node) as documented.
    idom_map(edges, root).into_iter().map(|(v, d)| (d, v)).collect()
}

/// Post-dominator pairs on the reversed graph, rooted at `exit`.
pub fn postdom_edges(edges: &[(Gid, Gid)], exit: Gid) -> Vec<(Gid, Gid)> {
    let rev: Vec<(Gid, Gid)> = edges.iter().map(|&(a, b)| (b, a)).collect();
    dom_edges(&rev, exit)
}

fn adjacency(edges: &[(Gid, Gid)]) -> (HashMap<Gid, Vec<Gid>>, HashMap<Gid, Vec<Gid>>) {
    let mut succ: HashMap<Gid, Vec<Gid>> = HashMap::new();
    let mut pred: HashMap<Gid, Vec<Gid>> = HashMap::new();
    for &(a, b) in edges {
        succ.entry(a).or_default().push(b);
        pred.entry(b).or_default().push(a);
    }
    (succ, pred)
}

/// Nodes of the sub-graph reachable from `root` (for validation).
pub fn reachable(edges: &[(Gid, Gid)], root: Gid) -> HashSet<Gid> {
    let (succ, _) = adjacency(edges);
    let mut seen = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(v) = stack.pop() {
        for w in succ.get(&v).cloned().unwrap_or_default() {
            if seen.insert(w) {
                stack.push(w);
            }
        }
    }
    seen
}

// ---------------------------------------------------------------------------
// Graph attachment: materialize dominator edges into the shared Graph
// ---------------------------------------------------------------------------

/// Options for `attach_dominator_edges`.
#[derive(Debug, Clone, Copy)]
pub struct AttachOptions {
    /// Attach `EdgeKind::Dominate` edges (immediate dominators).
    pub with_dominators: bool,
    /// Attach `EdgeKind::PostDominate` edges (immediate post-dominators).
    pub with_post_dominators: bool,
}

impl Default for AttachOptions {
    fn default() -> Self {
        Self {
            with_dominators: true,
            with_post_dominators: true,
        }
    }
}

/// Materialize dominator-tree edges into the shared graph, in the
/// "dominators" overlay namespace.
///
/// Each CFG node that is bound to an AST node (Statement / Condition /
/// Iterate) contributes its dominator relation as an AST-node → AST-node
/// edge: for every CFG edge pair (d, v) where BOTH d and v are AST-bound,
/// an `EdgeKind::Dominate` edge (src = dominator's AST node, dst = dominated
/// node's AST node) is added. Virtual nodes (Entry/Exit/Panic, loop-exit)
/// have no AST counterpart and are skipped — their relations are already
/// captured implicitly (return edges to Exit, etc.). Post-dominators are
/// attached the same way as `EdgeKind::PostDominate` when requested.
pub fn attach_dominator_edges(g: &mut Graph, cfg: &Cfg, opts: &AttachOptions) {
    let edges: Vec<(Gid, Gid)> = cfg.edges.iter().map(|e| (e.src, e.dst)).collect();
    let ast_of = |id: Gid| -> Option<NodeId> {
        cfg.nodes.iter().find_map(|n| {
            if n.id != id {
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
    let attach = |g: &mut Graph, pairs: Vec<(Gid, Gid)>, kind: EdgeKind| {
        for (d, v) in pairs {
            if let (Some(ds), Some(vs)) = (ast_of(d), ast_of(v)) {
                if ds != vs {
                    g.add_edge(Edge {
                        id: g.edges.len() as u64 + 1,
                        src: ds,
                        dst: vs,
                        kind: kind.clone(),
                    });
                }
            }
        }
    };
    if opts.with_dominators {
        let pairs = dom_edges(&edges, cfg.entry);
        attach(g, pairs, EdgeKind::Dominate);
    }
    if opts.with_post_dominators {
        let pairs = postdom_edges(&edges, cfg.exit);
        attach(g, pairs, EdgeKind::PostDominate);
    }
}