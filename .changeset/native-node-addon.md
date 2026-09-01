---
"openrscad-release-root": minor
---

Ship a native Node engine, `@taulabs/openrscad-engine-native`, that produces byte-identical artifacts to the WebAssembly build.

The pipeline moved into a new target-agnostic `openrscad-api` crate that both `openrscad-wasm` and a new `openrscad-napi` addon marshal over, so there is one implementation and no way for the two builds to drift. Three determinism fixes make the two byte-identical on every format, including full-precision 3MF: the convex-hull horizon is a `BTreeSet` (a `HashSet` made triangle order depend on the allocator, so the same model could export two different GLBs inside one process), and both the SCAD math builtins and the geometry tessellators now use the `libm` crate instead of the platform's, which disagreed in the last f64 ULP. `openrscad-geom` gains a `rust-relation` feature so a native build can select the pure-Rust CSG relation kernel the wasm build uses, and `openrscad-eval` gains a default-off `system-fonts` feature so only hosts that want OS font discovery (the CLI, the LSP, the desktop app) get it.
