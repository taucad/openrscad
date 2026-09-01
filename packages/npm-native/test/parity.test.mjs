// The native addon and the Wasm build are the same Rust pipeline behind two
// marshalling layers. This is the gate that keeps that literally true: every
// built-in fixture, both edge modes, GLB and 3MF, byte for byte.
//
// Requires local builds of BOTH engines (`npm run build` here and in
// ../npm) — never the published tarball, whose Rust predates any change
// under test.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  builtInFixtures,
  nativeVsWasmParity,
} from "../../../benchmarks/export-shape3d-benchmark.mjs";
import * as nativeApi from "../dist/node.js";
import * as wasmApi from "../../npm/dist/node.js";

const fixtures = async () => {
  const font = await readFile(
    new URL("../../../crates/openrscad-eval/fonts/LiberationSans-Regular.ttf", import.meta.url),
  );
  return builtInFixtures.map((fixture) =>
    fixture.name === "text" ? { ...fixture, options: { fontFiles: [font] } } : fixture,
  );
};

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
