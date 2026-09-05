// Node entry (the `node` + `import` export condition).
//
// One package, two payloads. `@taulabs/openrscad-engine` compiles one Rust
// pipeline (`crates/openrscad-api`) through two marshalling layers, so this
// entry binds the N-API addon when a platform package — or a colocated build —
// matches the host, and the in-package wasm Node build when none does. Both
// bindings are fed the same `makeApi()` facade, so there is no second place for
// the two engines' semantics to drift apart; `src/api-parity.ts` asserts this
// module and the browser entry stay structurally identical.
//
// The addon is loaded through the NAPI-RS-generated loader (never a
// hand-written platform table), which throws one `Error` whose `cause` chains
// every failed candidate. An uncovered platform must not lose the engine, so
// that error is kept on `backendCause` and the wasm build takes over instead of
// the import throwing. `backend` says which one bound — the fallback is never
// silent. (CommonJS `require()` consumers resolve the raw `pkg/node/openrscad.js`
// glue directly and are always wasm — see the package `exports` map.)
import { makeApi, type Backend, type RawEngine } from "./core.js";

/** Resolves immediately — whichever binding bound is initialized on import. */
export function ensureReady(): Promise<void> {
  return Promise.resolve();
}

async function loadWasm(): Promise<RawEngine> {
  const glue = await import("../pkg/node/openrscad.js");
  return {
    render_with_files: glue.render_with_files,
    export_2d: glue.export_2d,
    export_3d: glue.export_3d,
    render_to_glb: glue.render_to_glb,
    parameters: glue.parameters,
    version: glue.version,
    clear_cache: glue.clear_cache,
    cache_export: glue.cache_export,
    cache_import: glue.cache_import,
    cache_stats: glue.cache_stats,
    cache_keys: glue.cache_keys,
  } as unknown as RawEngine;
}

let bound: Backend = "native";
let cause: unknown;
let engine: RawEngine;
try {
  engine = (await import("./native/index.js")) as unknown as RawEngine;
} catch (error) {
  cause = error;
  bound = "wasm";
  engine = await loadWasm();
}

/** Which binding this process bound. `"wasm"` means no platform package matched
 *  the host and the in-package WebAssembly build took over. */
export const backend: Backend = bound;

/** The native loader's failure chain when `backend` is `"wasm"` and the addon
 *  was tried, else `undefined`. Never flattened to a string — it names every
 *  candidate the loader attempted. */
export const backendCause: unknown = cause;

const api = makeApi(engine, ensureReady);

export const render = api.render;
export const renderToGlb = api.renderToGlb;
export const exportShape2D = api.exportShape2D;
export const exportShape3D = api.exportShape3D;
export const parameters = api.parameters;
export const version = api.version;
export const clearCache = api.clearCache;
export const exportCache = api.exportCache;
export const importCache = api.importCache;
export const cacheStats = api.cacheStats;
export const cacheKeys = api.cacheKeys;

export type {
  Backend,
  CacheImportReport,
  CacheStats,
  Diagnostic,
  ExportGlbOptions,
  ExportShape3DFormat,
  ExportShape3DOutput,
  RenderOptions,
  RenderOutput,
  RenderToGlbOptions,
} from "./core.js";
