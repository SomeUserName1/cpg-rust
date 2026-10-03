// Full-pipeline CLI (task t_4ffa769c):
//   cpg build <FOLDER> [-o out.json]   — AST + CFG + dominators + DFG/call/imports
//   cpg inspect <graph.json> <FUNCTION> [--format dot|json] [-o out]
//        — dump one function's subgraph for manual verification
use cpg_ast::build_folder_full;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("usage:");
        eprintln!("  cpg build <FOLDER> [-o out.json]");
        eprintln!("  cpg inspect <graph.json> <FUNCTION> [--format dot|json] [-o out]");
        std::process::exit(if args.len() < 2 { 2 } else { 0 });
    }
    match args[1].as_str() {
        "build" => cmd_build(&args[2..]),
        "inspect" => cmd_inspect(&args[2..]),
        other => {
            eprintln!("unknown subcommand: {other} (expected build|inspect)");
            std::process::exit(2);
        }
    }
}

fn cmd_build(args: &[String]) {
    if args.is_empty() {
        eprintln!("usage: cpg build <FOLDER> [-o out.json]");
        std::process::exit(2);
    }
    let folder = PathBuf::from(&args[0]);
    let out = args
        .iter()
        .position(|a| a == "-o")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cpg.json"));
    match build_folder_full(&folder) {
        Ok((graph, stats, ostats, n_cfgs)) => {
            let json = cpg_ast::to_json(&graph);
            std::fs::write(&out, &json).unwrap();
            println!(
                "wrote {} ({} nodes, {} edges; files={} parse_errors={} macros={} module_links={} dfg={} call_resolved={} call_unresolved={} imports={} functions_with_cfg={})",
                out.display(),
                stats.nodes,
                graph.edges.len(),
                stats.files_parsed,
                stats.parse_errors,
                stats.macro_invocations,
                stats.module_links,
                ostats.dfg_edges,
                ostats.call_edges,
                ostats.call_unresolved,
                ostats.import_edges,
                n_cfgs,
            );
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_inspect(args: &[String]) {
    if args.len() < 2 {
        eprintln!("usage: cpg inspect <graph.json> <FUNCTION> [--format dot|json] [-o out]");
        std::process::exit(2);
    }
    let path = PathBuf::from(&args[0]);
    let func = &args[1];
    let fmt = args
        .iter()
        .position(|a| a == "--format")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("dot");
    let out = args
        .iter()
        .position(|a| a == "-o")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    let json = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("error: read {}: {e}", path.display());
        std::process::exit(1);
    });
    let graph = cpg_ast::from_json(&json).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });
    let result = match fmt {
        "json" => cpg_ast::inspect::function_subgraph_json(&graph, func),
        "dot" => cpg_ast::inspect::function_subgraph_dot(&graph, func),
        other => {
            eprintln!("unknown format: {other} (expected dot|json)");
            std::process::exit(2);
        }
    };
    match result {
        Ok(text) => match out {
            Some(p) => {
                std::fs::write(&p, &text).unwrap();
                println!("wrote {} ({} bytes)", p.display(), text.len());
            }
            None => print!("{}", text),
        },
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
