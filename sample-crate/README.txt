Sample crate exercising the AST-layer acceptance areas:
nested modules, impl blocks, traits + generics, macros.

- src/main.rs: crate root; `mod inner { ... }` inline module;
  `mod utils;` -> src/utils/mod.rs; trait Animal + generic struct + impls;
  macro_rules! + invocation.
- src/utils/mod.rs: nested module contents.
- src/utils/deep.rs: two-level nesting via utils::deep (linked from mod.rs).
