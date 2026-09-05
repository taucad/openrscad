//! Cache persistence hooks: a target-independent structural hasher, a
//! versioned little-endian bulk export/import of [`GeomCache`] entries, and
//! least-recently-used trimming.
//!
//! Hosts treat the exported blob as opaque bytes. Its validity domain is the
//! envelope written in its header — format version, hash algorithm, engine
//! version and the id of the CSG kernel that produced the meshes — and
//! [`GeomCache::import_bytes`] refuses anything else (atomically: a rejected blob
//! changes nothing), so a stale or foreign blob can only ever cost a miss, never
//! serve a wrong mesh.

use std::collections::HashMap;
use std::hash::Hasher;

use crate::{CachedNode, GeomCache, Mesh};

const MAGIC: &[u8; 4] = b"ORSC";
/// Bump when the blob layout changes.
pub const CACHE_FORMAT_VERSION: u32 = 1;
/// Identifies the structural-hash algorithm. Bump when `hash_all` or
/// [`StableHasher`] change, so blobs keyed by the old algorithm are refused.
pub const HASH_ALGO: &str = "sip13-le-v1";

/// SipHash-1-3 with fixed keys (std's `DefaultHasher::new()`), with every
/// integer write canonicalised to little-endian fixed width.
///
/// std's `Hasher` hashes slice lengths and enum discriminants as `usize`/`isize`
/// in native byte order and width, so the same tree hashes differently on
/// wasm32 (4-byte) and 64-bit native (8-byte). Routing those writes through
/// fixed-width little-endian bytes makes the key identical on every target.
/// The `stable_hash_known_answer` test pins the algorithm: if a toolchain ever
/// changes `DefaultHasher`, the test fails and `HASH_ALGO` must be bumped.
pub(crate) struct StableHasher(std::collections::hash_map::DefaultHasher);

impl StableHasher {
    pub(crate) fn new() -> Self {
        Self(std::collections::hash_map::DefaultHasher::new())
    }
}

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0.finish()
    }
    fn write(&mut self, bytes: &[u8]) {
        self.0.write(bytes)
    }
    fn write_u8(&mut self, i: u8) {
        self.0.write(&[i])
    }
    fn write_u16(&mut self, i: u16) {
        self.0.write(&i.to_le_bytes())
    }
    fn write_u32(&mut self, i: u32) {
        self.0.write(&i.to_le_bytes())
    }
    fn write_u64(&mut self, i: u64) {
        self.0.write(&i.to_le_bytes())
    }
    fn write_u128(&mut self, i: u128) {
        self.0.write(&i.to_le_bytes())
    }
    fn write_usize(&mut self, i: usize) {
        self.0.write(&(i as u64).to_le_bytes())
    }
    fn write_i8(&mut self, i: i8) {
        self.write_u8(i as u8)
    }
    fn write_i16(&mut self, i: i16) {
        self.write_u16(i as u16)
    }
    fn write_i32(&mut self, i: i32) {
        self.write_u32(i as u32)
    }
    fn write_i64(&mut self, i: i64) {
        self.write_u64(i as u64)
    }
    fn write_i128(&mut self, i: i128) {
        self.write_u128(i as u128)
    }
    fn write_isize(&mut self, i: isize) {
        self.write_u64(i as i64 as u64)
    }
}

/// What a blob is valid for. Two engines with different envelopes never
/// exchange entries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheEnvelope {
    /// The engine version that rendered the meshes (`CARGO_PKG_VERSION`).
    pub engine_version: String,
    /// [`crate::Kernel::id`] of the CSG kernel that rendered the meshes.
    pub kernel_id: String,
}

/// Resident-cache accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    /// Mesh payload bytes (vertices + triangles), excluding diagnostics/overhead.
    pub bytes: usize,
    /// Current export epoch (see [`GeomCache::export_since`]).
    pub epoch: u64,
}

/// Outcome of a successful import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheImportReport {
    /// Entries added.
    pub imported: usize,
    /// Entries whose key was already resident (kept as-is).
    pub skipped: usize,
    /// Resident entries after the import.
    pub entries: usize,
    /// Resident payload bytes after the import.
    pub bytes: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheImportError {
    #[error("not an OpenRSCAD cache blob")]
    BadMagic,
    #[error("unsupported cache blob format {0} (this engine reads {CACHE_FORMAT_VERSION})")]
    Format(u32),
    #[error("cache blob {field} is {blob:?}; this engine is {engine:?}")]
    Envelope {
        field: &'static str,
        blob: String,
        engine: String,
    },
    #[error("truncated cache blob")]
    Truncated,
}

pub(crate) fn mesh_bytes(mesh: &Mesh) -> usize {
    mesh.verts.len() * std::mem::size_of::<[f64; 3]>()
        + mesh.tris.len() * std::mem::size_of::<[u32; 3]>()
}

struct Writer(Vec<u8>);

impl Writer {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str16(&mut self, s: &str) {
        self.0.extend_from_slice(&(s.len() as u16).to_le_bytes());
        self.0.extend_from_slice(s.as_bytes());
    }
    fn str32(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], CacheImportError> {
        let end = self.at.checked_add(n).ok_or(CacheImportError::Truncated)?;
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or(CacheImportError::Truncated)?;
        self.at = end;
        Ok(slice)
    }
    fn u16(&mut self) -> Result<u16, CacheImportError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, CacheImportError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, CacheImportError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64, CacheImportError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn str16(&mut self) -> Result<String, CacheImportError> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn str32(&mut self) -> Result<String, CacheImportError> {
        let n = self.u32()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

type Entry = (u64, Mesh, Vec<String>, Vec<String>);

/// Parse a whole blob, checking the envelope first. Allocation is bounded by
/// the bytes actually present, so a garbage header cannot reserve memory it
/// never fills.
fn decode(bytes: &[u8], envelope: &CacheEnvelope) -> Result<Vec<Entry>, CacheImportError> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(4)? != MAGIC {
        return Err(CacheImportError::BadMagic);
    }
    let format = r.u32()?;
    if format != CACHE_FORMAT_VERSION {
        return Err(CacheImportError::Format(format));
    }
    let check = |field: &'static str, blob: String, engine: &str| {
        if blob == engine {
            Ok(())
        } else {
            Err(CacheImportError::Envelope {
                field,
                blob,
                engine: engine.to_string(),
            })
        }
    };
    check("hash algorithm", r.str16()?, HASH_ALGO)?;
    check("engine version", r.str16()?, &envelope.engine_version)?;
    check("kernel", r.str16()?, &envelope.kernel_id)?;
    let _blob_epoch = r.u64()?;
    let count = r.u32()? as usize;
    let mut entries = Vec::with_capacity(count.min(r.remaining() / 24));
    for _ in 0..count {
        let key = r.u64()?;
        let nverts = r.u32()? as usize;
        let ntris = r.u32()? as usize;
        let nwarn = r.u32()? as usize;
        let nerr = r.u32()? as usize;
        let mut verts = Vec::with_capacity(nverts.min(r.remaining() / 24));
        for _ in 0..nverts {
            verts.push([r.f64()?, r.f64()?, r.f64()?]);
        }
        let mut tris = Vec::with_capacity(ntris.min(r.remaining() / 12));
        for _ in 0..ntris {
            tris.push([r.u32()?, r.u32()?, r.u32()?]);
        }
        let mut warnings = Vec::with_capacity(nwarn.min(r.remaining() / 4));
        for _ in 0..nwarn {
            warnings.push(r.str32()?);
        }
        let mut errors = Vec::with_capacity(nerr.min(r.remaining() / 4));
        for _ in 0..nerr {
            errors.push(r.str32()?);
        }
        entries.push((key, Mesh { verts, tris }, warnings, errors));
    }
    Ok(entries)
}

impl GeomCache {
    /// Serialize every entry inserted at or after `since_epoch` (`0` = every
    /// entry, imported ones included) and advance the epoch, so a host that
    /// persists incrementally calls this after each render with the epoch the
    /// previous call reported (see [`CacheStats::epoch`]) and receives exactly the
    /// entries it has not seen. Entries are written least-recently-used first:
    /// an import stamps recency in file order, so an imported cache evicts in
    /// the same order the exporter would have (see [`GeomCache::trim_to`]).
    pub fn export_since(&mut self, since_epoch: u64, envelope: &CacheEnvelope) -> Vec<u8> {
        let mut keys: Vec<(u64, u64)> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.epoch >= since_epoch)
            .map(|(key, node)| (node.last_used, *key))
            .collect();
        keys.sort_unstable();
        self.epoch += 1;
        let mut w = Writer(Vec::new());
        w.0.extend_from_slice(MAGIC);
        w.u32(CACHE_FORMAT_VERSION);
        w.str16(HASH_ALGO);
        w.str16(&envelope.engine_version);
        w.str16(&envelope.kernel_id);
        w.u64(self.epoch);
        w.u32(keys.len() as u32);
        for (_, key) in keys {
            let node = &self.nodes[&key];
            w.u64(key);
            w.u32(node.mesh.verts.len() as u32);
            w.u32(node.mesh.tris.len() as u32);
            w.u32(node.warnings.len() as u32);
            w.u32(node.errors.len() as u32);
            for v in &node.mesh.verts {
                for c in v {
                    w.f64(*c);
                }
            }
            for t in &node.mesh.tris {
                for i in t {
                    w.u32(*i);
                }
            }
            for s in node.warnings.iter().chain(node.errors.iter()) {
                w.str32(s);
            }
        }
        w.0
    }

    /// Add the entries of a blob produced by [`GeomCache::export_since`] on an
    /// engine with the same envelope. The import is atomic: a foreign, malformed
    /// or truncated blob changes nothing. Resident keys win (the blob never
    /// overwrites); imported entries carry epoch 0 so incremental exports never
    /// re-emit them.
    pub fn import_bytes(
        &mut self,
        bytes: &[u8],
        envelope: &CacheEnvelope,
    ) -> Result<CacheImportReport, CacheImportError> {
        let entries = decode(bytes, envelope)?;
        let mut report = CacheImportReport::default();
        for (key, mesh, warnings, errors) in entries {
            if self.nodes.contains_key(&key) {
                report.skipped += 1;
                continue;
            }
            self.bytes += mesh_bytes(&mesh);
            self.tick += 1;
            self.nodes.insert(
                key,
                CachedNode {
                    mesh,
                    warnings,
                    errors,
                    epoch: 0,
                    last_used: self.tick,
                },
            );
            report.imported += 1;
        }
        report.entries = self.nodes.len();
        report.bytes = self.bytes;
        Ok(report)
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.nodes.len(),
            bytes: self.bytes,
            epoch: self.epoch,
        }
    }

    /// Every resident key, ascending.
    pub fn keys(&self) -> Vec<u64> {
        let mut keys: Vec<u64> = self.nodes.keys().copied().collect();
        keys.sort_unstable();
        keys
    }

    /// Evict least-recently-used entries until both budgets hold. Replaces the
    /// wholesale reset hosts used to do at the entry cap, which would have
    /// dropped a freshly imported cache on the very next render.
    pub fn trim_to(&mut self, max_entries: usize, max_bytes: usize) {
        if self.nodes.len() <= max_entries && self.bytes <= max_bytes {
            return;
        }
        let mut order: Vec<(u64, u64)> = self
            .nodes
            .iter()
            .map(|(key, node)| (node.last_used, *key))
            .collect();
        order.sort_unstable();
        for (_, key) in order {
            if self.nodes.len() <= max_entries && self.bytes <= max_bytes {
                break;
            }
            if let Some(node) = self.nodes.remove(&key) {
                self.bytes -= mesh_bytes(&node.mesh);
            }
        }
    }

    pub(crate) fn touch(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }
}

impl Default for GeomCache {
    fn default() -> Self {
        GeomCache {
            nodes: HashMap::new(),
            // Starts at 1 so imported entries (epoch 0) are excluded from every
            // incremental export and included only in a full (`since 0`) dump.
            epoch: 1,
            tick: 0,
            bytes: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render_cached, Kernel, RustManifoldKernel};
    use openrscad_ir::{FragmentSpec, Node};

    fn model(r: f64) -> Node {
        let frags = FragmentSpec {
            fn_: 24.0,
            fa: 12.0,
            fs: 2.0,
        };
        Node::Difference(vec![
            Node::Union(vec![
                Node::Cube {
                    size: [20.0, 20.0, 20.0],
                    center: true,
                },
                Node::Translate {
                    v: [5.0, 0.0, 0.0],
                    child: Box::new(Node::Cylinder {
                        h: 30.0,
                        r1: 4.0,
                        r2: 4.0,
                        center: true,
                        frags,
                    }),
                },
            ]),
            Node::Sphere { r, frags },
        ])
    }

    fn envelope() -> CacheEnvelope {
        CacheEnvelope {
            engine_version: "test".into(),
            kernel_id: RustManifoldKernel::new().id().into(),
        }
    }

    /// Pins the hash algorithm: this value must be identical on every target and
    /// toolchain. A change means `HASH_ALGO` must be bumped.
    #[test]
    fn stable_hash_known_answer() {
        let mut hashes = HashMap::new();
        let hash = crate::hash_all(&model(8.0), &mut hashes);
        assert_eq!(
            hash, 0x6b1a_17a0_6865_36f6,
            "structural hash drifted: {hash:#x}"
        );
    }

    #[test]
    fn export_import_round_trip_is_exact_and_hits() {
        let kernel = RustManifoldKernel::new();
        let mut warm = GeomCache::new();
        let cold = render_cached(&model(8.0), &kernel, &mut warm).unwrap();
        let blob = warm.export_since(0, &envelope());

        let mut fresh = GeomCache::new();
        let report = fresh.import_bytes(&blob, &envelope()).unwrap();
        assert_eq!(report.imported, warm.len());
        assert_eq!(report.skipped, 0);
        assert_eq!(fresh.keys(), warm.keys());
        assert_eq!(fresh.stats().bytes, warm.stats().bytes);

        // Rehydrated render: no new entries (every node hits) and a bit-exact mesh.
        let before = fresh.len();
        let rehydrated = render_cached(&model(8.0), &kernel, &mut fresh).unwrap();
        assert_eq!(fresh.len(), before, "rehydrated render must be all hits");
        assert_eq!(rehydrated, cold, "rehydrated mesh must be bit-identical");

        // A late edit reuses the heavy union: exactly sphere + difference are new.
        render_cached(&model(7.0), &kernel, &mut fresh).unwrap();
        assert_eq!(fresh.len(), before + 2);

        // Incremental export protocol: a host passes back the epoch `stats()`
        // reported before the render and receives exactly the entries rendered
        // since (imported ones carry epoch 0 and are never re-emitted).
        let delta = fresh.export_since(fresh.stats().epoch, &envelope());
        let mut third = GeomCache::new();
        assert_eq!(third.import_bytes(&delta, &envelope()).unwrap().imported, 2);
        // Nothing rendered since: the next incremental export is empty.
        let empty = fresh.export_since(fresh.stats().epoch, &envelope());
        assert_eq!(
            GeomCache::new()
                .import_bytes(&empty, &envelope())
                .unwrap()
                .imported,
            0
        );
        // A full dump (`since 0`) still carries everything, imported entries included.
        let full = fresh.export_since(0, &envelope());
        assert_eq!(
            GeomCache::new()
                .import_bytes(&full, &envelope())
                .unwrap()
                .imported,
            fresh.len()
        );

        // Importing the same blob twice keeps the resident entries.
        let again = fresh.import_bytes(&blob, &envelope()).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped, warm.len());
    }

    #[test]
    fn import_refuses_foreign_envelopes_and_garbage() {
        let mut cache = GeomCache::new();
        render_cached(&model(8.0), &RustManifoldKernel::new(), &mut cache).unwrap();
        let blob = cache.export_since(0, &envelope());
        let mut other = GeomCache::new();
        let foreign = CacheEnvelope {
            engine_version: "other".into(),
            ..envelope()
        };
        assert!(matches!(
            other.import_bytes(&blob, &foreign),
            Err(CacheImportError::Envelope {
                field: "engine version",
                ..
            })
        ));
        assert!(matches!(
            other.import_bytes(b"nope", &envelope()),
            Err(CacheImportError::BadMagic)
        ));
        assert!(matches!(
            other.import_bytes(&blob[..blob.len() / 2], &envelope()),
            Err(CacheImportError::Truncated)
        ));
        assert!(other.is_empty());
    }

    /// The exporter's recency order survives the round trip: trimming an imported
    /// cache evicts exactly what trimming the exporter would, and leaves under a
    /// resident parent go before the parent that covers them.
    #[test]
    fn import_preserves_recency_order() {
        let kernel = RustManifoldKernel::new();
        let mut warm = GeomCache::new();
        render_cached(&model(8.0), &kernel, &mut warm).unwrap();
        render_cached(&model(7.0), &kernel, &mut warm).unwrap();
        let n = warm.len();
        let blob = warm.export_since(0, &envelope());
        let mut fresh = GeomCache::new();
        assert_eq!(fresh.import_bytes(&blob, &envelope()).unwrap().imported, n);

        warm.trim_to(n - 2, usize::MAX);
        fresh.trim_to(n - 2, usize::MAX);
        assert_eq!(
            fresh.keys(),
            warm.keys(),
            "importer must evict what the exporter evicts"
        );

        // The two oldest entries are the union's leaves (cube, cylinder): rendered
        // first and never visited again because the union hit skipped them.
        let leaf = |node: Node| crate::hash_all(&node, &mut HashMap::new());
        let cube = leaf(Node::Cube {
            size: [20.0, 20.0, 20.0],
            center: true,
        });
        let cylinder = leaf(Node::Cylinder {
            h: 30.0,
            r1: 4.0,
            r2: 4.0,
            center: true,
            frags: FragmentSpec {
                fn_: 24.0,
                fa: 12.0,
                fs: 2.0,
            },
        });
        assert!(!fresh.nodes.contains_key(&cube) && !fresh.nodes.contains_key(&cylinder));
        for root in [leaf(model(8.0)), leaf(model(7.0))] {
            assert!(
                fresh.nodes.contains_key(&root),
                "roots are the most valuable entries"
            );
        }
    }

    #[test]
    fn trim_evicts_least_recently_used() {
        let kernel = RustManifoldKernel::new();
        let mut cache = GeomCache::new();
        render_cached(&model(8.0), &kernel, &mut cache).unwrap();
        let n = cache.len();
        // Touch the root again so the heavy union subtree is the most recent.
        render_cached(&model(8.0), &kernel, &mut cache).unwrap();
        render_cached(&model(7.0), &kernel, &mut cache).unwrap();
        let bytes = cache.stats().bytes;
        cache.trim_to(n, usize::MAX);
        assert_eq!(cache.len(), n);
        assert!(cache.stats().bytes < bytes);
        cache.trim_to(usize::MAX, 1);
        assert!(cache.is_empty());
        assert_eq!(cache.stats().bytes, 0);
    }
}
