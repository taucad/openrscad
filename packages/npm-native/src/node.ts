// Node entry for the native engine.
//
// `@taulabs/openrscad-engine` and this package are the same Rust pipeline
// (`crates/openrscad-api`) behind two marshalling layers, so the public surface
// here is literally the wasm package's `makeApi()` facade fed the N-API addon
// instead of the wasm glue. That is the whole package: one import swap, one
// shared facade, and therefore no second place for the two engines' semantics
// to drift apart. `src/api-parity.ts` asserts the two module types match.
//
// The addon is loaded through the NAPI-RS-generated loader (never a hand-written
// platform table), which throws one `Error` whose `cause` chains every failed
// candidate when no binding matches the host.
import { makeApi, type RawEngine } from "@taulabs/openrscad-engine/core";
import * as binding from "./native/index.js";

/** Resolves immediately — the addon is fully initialized by `require`. Exists
 *  only for API parity with the browser entry. */
export function ensureReady(): Promise<void> {
  return Promise.resolve();
}

const api = makeApi(binding as unknown as RawEngine, ensureReady);

export const render = api.render;
export const renderToGlb = api.renderToGlb;
export const exportShape2D = api.exportShape2D;
export const exportShape3D = api.exportShape3D;
export const parameters = api.parameters;
export const version = api.version;
export const clearCache = api.clearCache;

export type {
  Diagnostic,
  ExportGlbOptions,
  ExportShape3DFormat,
  ExportShape3DOutput,
  RenderOptions,
  RenderOutput,
  RenderToGlbOptions,
} from "@taulabs/openrscad-engine/core";
