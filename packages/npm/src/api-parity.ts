// Compile-time proof that the two entry points of this package are one surface.
//
// `.` resolves `dist/browser.js` for browsers and bundlers and `dist/node.js`
// under the `node` condition, and `@taucad/openrscad`'s backend type is
// `typeof import('@taulabs/openrscad-engine')` — so a host that resolves the
// other condition must get a structurally identical module or the kernel will
// not typecheck. That is also what keeps the N-API addon a drop-in for the
// WebAssembly build: `dist/node.js` is the addon-bound module. `tsc` fails this
// file if a signature drifts; there is no runtime cost and nothing is emitted
// into the public surface.
import type * as browser from "./browser.js";
import type * as node from "./node.js";

type AssignableTo<Actual extends Expected, Expected> = Actual;

export type NativeIsADropInForWasm = AssignableTo<typeof node, typeof browser>;
export type WasmIsADropInForNative = AssignableTo<typeof browser, typeof node>;
