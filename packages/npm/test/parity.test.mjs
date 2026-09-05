// The native addon and the Wasm build are the same Rust pipeline behind two
// marshalling layers, and one package now ships both. This is the gate that
// keeps the claim literally true: every built-in fixture, both edge modes, GLB
// and 3MF, byte for byte.
//
// `../dist/node.js` is the entry under test — it binds the addon when one
// matches this host. The Wasm side is rebuilt in-process from `./core`'s
// `makeApi()` over the raw `./node` glue, which is the same facade the entry
// falls back to, so no second package and no published tarball is involved.
// Requires a local `npm run build` (wasm + addon + facade).
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  builtInFixtures,
  nativeVsWasmParity,
} from "../../../benchmarks/export-shape3d-benchmark.mjs";
import { makeApi } from "../dist/core.js";
import * as nativeApi from "../dist/node.js";
import * as glue from "../pkg/node/openrscad.js";

const wasmApi = makeApi(glue, () => Promise.resolve());

const fixtures = async () => {
  const font = await readFile(
    new URL("../../../crates/openrscad-eval/fonts/LiberationSans-Regular.ttf", import.meta.url),
  );
  return builtInFixtures.map((fixture) =>
    fixture.name === "text" ? { ...fixture, options: { fontFiles: [font] } } : fixture,
  );
};

test("the Node entry binds the native addon on a covered host", () => {
  assert.equal(
    nativeApi.backend,
    "native",
    `the entry fell back to wasm, so this file would compare wasm to itself: ${nativeApi.backendCause}`,
  );
});

test("every built-in artifact is byte-identical between the native addon and Wasm", async () => {
  const result = await nativeVsWasmParity({
    wasmApi,
    nativeApi,
    fixtures: await fixtures(),
  });
  assert.equal(
    result.mismatches.length,
    0,
    `native/wasm divergence: ${JSON.stringify(result.mismatches, null, 2)}`,
  );
  assert.equal(result.total, 30, "6 fixtures x (2 render-glb + 2 export-glb + 1 3mf)");
});

test("the native engine reports the same version as the Wasm engine", async () => {
  assert.equal(await nativeApi.version(), await wasmApi.version());
});

test("repeated exports in one process are byte-identical", async () => {
  const seen = new Set();
  for (let index = 0; index < 20; index += 1) {
    await nativeApi.clearCache();
    const output = await nativeApi.exportShape3D("hull() { cube(2); translate([9,0,3]) sphere(3); }", "3mf");
    assert.ok(output.ok, output.error);
    seen.add(Buffer.from(output.bytes).toString("base64"));
  }
  assert.equal(seen.size, 1, `${seen.size} distinct artifacts in 20 in-process exports`);
});
