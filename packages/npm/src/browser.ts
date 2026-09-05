// Browser / bundler entry (the default `.` export for web and CDN consumers).
//
// The wasm module is fetched and instantiated on first use via `init()`. Call
// `ensureReady(wasmUrl?)` up front to control when — and from where — the
// `.wasm` loads (preload it, or point at a custom/CDN URL); otherwise the first
// `render()` initializes it, resolving `openrscad_bg.wasm` next to the glue module.
import init, {
  render_with_files as rawRenderWithFiles,
  export_2d as rawExport2d,
  export_3d as rawExport3d,
  render_to_glb as rawRenderToGlb,
  parameters as rawParameters,
  version as rawVersion,
  clear_cache as rawClearCache,
} from "../pkg/web/openrscad.js";
import { makeApi, type Backend, type RawEngine } from "./core.js";

let ready: Promise<void> | null = null;

/** Initialize the wasm module (idempotent — safe to call repeatedly). Pass a
 *  URL/Request/etc. to load the `.wasm` from a custom location; omit it to
 *  resolve `openrscad_bg.wasm` beside the glue module. */
export function ensureReady(wasmUrl?: string | URL | Request): Promise<void> {
  return (ready ??= init(wasmUrl ? { module_or_path: wasmUrl } : undefined).then(() => undefined));
}

const engine = {
  render_with_files: rawRenderWithFiles,
  export_2d: rawExport2d,
  export_3d: rawExport3d,
  render_to_glb: rawRenderToGlb,
  parameters: rawParameters,
  version: rawVersion,
  clear_cache: rawClearCache,
} as unknown as RawEngine;

/** Always `"wasm"` here: the browser entry has no addon to bind. The Node entry
 *  is the one that can report `"native"`. */
export const backend: Backend = "wasm";

/** Always `undefined` here — kept so the browser and Node entries stay
 *  interchangeable (`src/api-parity.ts`). */
export const backendCause: unknown = undefined;

const api = makeApi(engine, ensureReady);

export const render = api.render;
export const renderToGlb = api.renderToGlb;
export const exportShape2D = api.exportShape2D;
export const exportShape3D = api.exportShape3D;
export const parameters = api.parameters;
export const version = api.version;
export const clearCache = api.clearCache;

export type {
  Backend,
  Diagnostic,
  ExportGlbOptions,
  ExportShape3DFormat,
  ExportShape3DOutput,
  RenderOptions,
  RenderOutput,
  RenderToGlbOptions,
} from "./core.js";
