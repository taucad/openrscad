// Compile-time proof that the native package is a drop-in for the wasm one.
//
// `@taucad/openrscad`'s backend type is `typeof import('@taulabs/openrscad-engine')`,
// so a host that swaps the import must get a structurally identical module or
// the kernel will not typecheck. `tsc` fails this file if a signature drifts —
// there is no runtime cost and nothing is emitted into the public surface.
import type * as wasm from "@taulabs/openrscad-engine";
import type * as native from "./node.js";

type AssignableTo<Actual extends Expected, Expected> = Actual;

export type NativeIsADropInForWasm = AssignableTo<typeof native, typeof wasm>;
export type WasmIsADropInForNative = AssignableTo<typeof wasm, typeof native>;
