// The geometry cache is one producer across both bindings of this package.
//
// `../dist/node.js` is the colocated entry — it binds the N-API addon on a
// covered host — and the Wasm side is rebuilt in-process from `./core`'s
// `makeApi()` over the raw `./node` glue, exactly as `parity.test.mjs` does. The
// two engines keep separate thread-local caches in one process, which is what
// makes the cross-import assertions meaningful: a blob one engine wrote must be
// accepted whole by the other, and the rehydrated render must be byte-identical
// to the cold one. Requires a local `npm run build` (wasm + addon + facade).
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { makeApi } from "../dist/core.js";
import * as nativeApi from "../dist/node.js";
import * as glue from "../pkg/node/openrscad.js";

const wasmApi = makeApi(glue, () => Promise.resolve());

const model = await readFile(
  new URL("../../../corpus/geom/bool_nested.scad", import.meta.url),
  "utf8",
);

const coldGlb = async (api) => {
  await api.clearCache();
  const output = await api.renderToGlb(model);
  assert.ok(output.ok, output.error);
  return Buffer.from(output.bytes);
};

test("the Node entry binds the native addon on a covered host", () => {
  assert.equal(
    nativeApi.backend,
    "native",
    `the entry fell back to wasm, so this file would compare wasm to itself: ${nativeApi.backendCause}`,
  );
});

test("both backends key the same model identically and agree on the envelope", async () => {
  await coldGlb(nativeApi);
  await coldGlb(wasmApi);

  const [nativeKeys, wasmKeys] = await Promise.all([nativeApi.cacheKeys(), wasmApi.cacheKeys()]);
  assert.ok(nativeKeys.length > 0, "the render cached nothing");
  assert.deepEqual([...nativeKeys], [...wasmKeys], "the two backends keyed the model differently");

  const [nativeStats, wasmStats] = await Promise.all([nativeApi.cacheStats(), wasmApi.cacheStats()]);
  assert.equal(nativeStats.engineVersion, wasmStats.engineVersion);
  assert.equal(nativeStats.kernelId, wasmStats.kernelId);
  assert.equal(nativeStats.hashAlgo, wasmStats.hashAlgo);
  assert.equal(nativeStats.formatVersion, wasmStats.formatVersion);
  assert.equal(nativeStats.bytes, wasmStats.bytes, "payload accounting must agree to the byte");
  assert.equal(nativeStats.entries, nativeKeys.length);
});

test("a blob crosses between the backends and rehydrates a byte-identical render", async () => {
  const nativeCold = await coldGlb(nativeApi);
  const wasmCold = await coldGlb(wasmApi);
  const nativeBlob = await nativeApi.exportCache();
  const wasmBlob = await wasmApi.exportCache();
  assert.ok(nativeBlob.length > 0 && wasmBlob.length > 0);

  for (const [name, api, foreignBlob, cold] of [
    ["native", nativeApi, wasmBlob, nativeCold],
    ["wasm", wasmApi, nativeBlob, wasmCold],
  ]) {
    await api.clearCache();
    const report = await api.importCache(foreignBlob);
    assert.equal(report.skipped, 0, `${name}: skipped an entry of a foreign-backend blob`);
    assert.equal(report.imported, report.entries, `${name}: import was partial`);

    const rehydrated = await api.renderToGlb(model);
    assert.ok(rehydrated.ok, rehydrated.error);
    assert.deepEqual(
      Buffer.from(rehydrated.bytes),
      cold,
      `${name}: the rehydrated render is not byte-identical to cold`,
    );
    const after = await api.cacheStats();
    assert.equal(after.entries, report.entries, `${name}: the rehydrated render missed the cache`);
  }
});

test("a foreign or malformed blob is refused and leaves the cache unchanged", async () => {
  await coldGlb(nativeApi);
  const before = await nativeApi.cacheStats();
  const blob = await nativeApi.exportCache();

  await assert.rejects(() => nativeApi.importCache(Buffer.from("not a cache blob")));
  await assert.rejects(() => nativeApi.importCache(blob.subarray(0, blob.length >> 1)));

  const after = await nativeApi.cacheStats();
  assert.equal(after.entries, before.entries);
  assert.equal(after.bytes, before.bytes);
});
