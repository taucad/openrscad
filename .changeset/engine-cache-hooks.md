---
"openrscad-release-root": minor
---

Expose the geometry cache as `exportCache` / `importCache` / `cacheStats` / `cacheKeys`, with keys that are stable across targets.

The persistent `GeomCache` can now be serialized to an opaque, versioned blob and rehydrated into a fresh engine, so a host can carry a warm cache across a page refresh or a worker restart instead of re-rendering every subtree cold. The hooks live on the target-agnostic `openrscad-api` crate and are marshalled 1:1 by both the wasm-bindgen surface and the N-API addon, so the two builds are one producer: the structural key is computed by a `StableHasher` that canonicalises every integer write to fixed-width little-endian (std's hasher writes `usize` at the target's width, so wasm32 and 64-bit native keys could never agree), and the algorithm is named by `HASH_ALGO` and pinned by a known-answer test that runs on native and on `wasm32-wasip1`.

Every blob carries an envelope — format version, hash algorithm, engine version and the id of the CSG kernel (`Kernel::id()`) that rendered the meshes — checked before a single entry is parsed, so a stale or foreign blob is refused atomically and can only ever cost a miss, never serve a wrong mesh. The entry cap no longer resets the whole cache: `GeomCache::trim_to` evicts least-recently-used subtrees under an entry and a byte budget (8192 entries, 256 MiB), and export writes least-recently-used first so an importing engine evicts exactly what the exporter would.
