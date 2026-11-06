//! Deterministic native 3D serialization over exact attributed geometry.

use super::mesh::{cross, normalize, package_3mf, sub};
use super::structured::{
    AttributedMesh, AttributionStatus, MeshSelection, StructuredMesh, SurfaceAttribution,
};
use super::{DisplayMode, GeomError, Mesh};
use openrscad_ir::{ProvenanceFrame, SourceId, SourceSpan};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
#[cfg(feature = "benchmark-profile")]
use web_time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat3D {
    Stl,
    Off,
    Obj,
    ThreeMf,
    Amf,
    Glb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinateSystem {
    YUp,
    ZUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Export3DOptions {
    pub include_edges: bool,
    pub source_unit_to_meters: f64,
    pub coordinate_system: CoordinateSystem,
    pub source_keys: Vec<String>,
}

impl Default for Export3DOptions {
    fn default() -> Self {
        Self {
            include_edges: false,
            source_unit_to_meters: 0.001,
            coordinate_system: CoordinateSystem::YUp,
            source_keys: vec!["<main>".to_string()],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Export3DArtifact {
    pub bytes: Vec<u8>,
    pub triangle_count: usize,
    pub vertex_count: usize,
    pub volume: f64,
    pub surface_area: f64,
    /// Export diagnostics: the artifact is complete, but a consumer may want
    /// to know, for example, that a GLB solid could not declare
    /// `EXT_mesh_manifold`.
    pub warnings: Vec<String>,
}

pub fn export_3d(
    structured: &StructuredMesh,
    format: ExportFormat3D,
    options: &Export3DOptions,
) -> Result<Export3DArtifact, GeomError> {
    let mesh = &structured.aggregate;
    let (bytes, warnings) = match format {
        ExportFormat3D::Glb => serialize_glb_with_diagnostics(structured, options)?,
        ExportFormat3D::ThreeMf => (serialize_3mf(structured, options)?, Vec::new()),
        _ => {
            return Err(GeomError::Invariant(
                "authored-scene export only supports GLB and 3MF".to_string(),
            ));
        }
    };
    Ok(Export3DArtifact {
        bytes,
        triangle_count: mesh.tris.len(),
        vertex_count: mesh.verts.len(),
        volume: mesh.volume(),
        surface_area: mesh.surface_area(),
        warnings,
    })
}

fn rgba_bytes(rgba: [f32; 4]) -> [u8; 4] {
    rgba.map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
}

#[derive(Clone, Copy)]
struct MeshSource<'a> {
    exact: &'a AttributedMesh,
    attributions: &'a [SurfaceAttribution],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MaterialKey {
    mode: u8,
    rgba: [u8; 4],
}

impl MaterialKey {
    fn from_surface(surface: &SurfaceAttribution) -> Self {
        Self {
            mode: match surface.mode {
                DisplayMode::Solid => 0,
                DisplayMode::Highlight => 1,
                DisplayMode::Background => 2,
            },
            rgba: rgba_bytes(surface.rgba),
        }
    }

    fn display_mode(self) -> &'static str {
        match self.mode {
            1 => "highlight",
            2 => "background",
            _ => "solid",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SemanticFrameKey {
    call_source: SourceId,
    call_start: u32,
    call_end: u32,
    definition: Option<(SourceId, u32, u32)>,
    module_name: String,
}

impl From<&ProvenanceFrame> for SemanticFrameKey {
    fn from(frame: &ProvenanceFrame) -> Self {
        Self {
            call_source: frame.call_site.source_id,
            call_start: frame.call_site.start,
            call_end: frame.call_site.end,
            definition: frame
                .definition_site
                .as_ref()
                .map(|span| (span.source_id, span.start, span.end)),
            module_name: frame
                .module_name
                .clone()
                .expect("semantic frames have authored module names"),
        }
    }
}

struct ComponentBuilder {
    key: SemanticFrameKey,
    path: Vec<SemanticFrameKey>,
    frame: ProvenanceFrame,
    parent: Option<usize>,
    children: Vec<usize>,
    /// Every triangle this module owns directly, across solids; only feature
    /// edges are derived from it; surfaces belong to the solid meshes.
    triangles: Vec<u32>,
    attribution: AttributionStatus,
    contributors: Vec<Vec<ProvenanceFrame>>,
}

struct GlbEdgeGroup<'a> {
    source: MeshSource<'a>,
    selection: MeshSelection,
}

/// An authored module: a node with no surface mesh of its own. It carries
/// `extras.openrscad`, its child modules, the solids anchored on it, and a
/// `LINES`-only mesh when feature edges are requested.
struct GlbComponent<'a> {
    frame: ProvenanceFrame,
    children: Vec<usize>,
    solids: Vec<usize>,
    name: String,
    edge_groups: Vec<GlbEdgeGroup<'a>>,
    attribution: AttributionStatus,
    contributors: Vec<Vec<ProvenanceFrame>>,
}

/// One primitive of a solid mesh: the solid's triangles that one module (or
/// no module) owns in one material.
struct GlbPrimitive {
    owner: Option<usize>,
    material: MaterialKey,
    selection: MeshSelection,
}

/// One closed solid (a `StructuredMesh::solid_components` entry): one leaf node
/// and one mesh, hung from the deepest authored module common to its faces.
struct GlbSolid<'a> {
    source: MeshSource<'a>,
    component: &'a MeshSelection,
    name: String,
    owners: Vec<usize>,
    primitives: Vec<GlbPrimitive>,
    /// Feature edges of the triangles no module owns; they have no module node
    /// to sit on, so they go on a `LINES`-only child of the solid's node.
    unowned_edges: Option<GlbEdgeGroup<'a>>,
}

enum GlbRoot {
    Component(usize),
    Solid(usize),
}

struct CompiledGlb<'a> {
    components: Vec<GlbComponent<'a>>,
    solids: Vec<GlbSolid<'a>>,
    roots: Vec<GlbRoot>,
}

fn material_rgba_from_key(material: MaterialKey) -> [u8; 4] {
    material.rgba
}

fn humanize_identifier(identifier: &str) -> String {
    identifier
        .split(['_', '-'])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            let Some(first) = characters.next() else {
                return String::new();
            };
            first.to_uppercase().chain(characters).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn semantic_sibling_names(builders: &[ComponentBuilder], roots: &[usize]) -> Vec<String> {
    let mut names = vec![String::new(); builders.len()];
    let sibling_groups =
        std::iter::once(roots).chain(builders.iter().map(|builder| builder.children.as_slice()));
    for siblings in sibling_groups {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for sibling in siblings {
            let base = humanize_identifier(&builders[*sibling].key.module_name);
            *counts.entry(base).or_default() += 1;
        }
        let mut occurrences: BTreeMap<String, usize> = BTreeMap::new();
        for sibling in siblings {
            let base = humanize_identifier(&builders[*sibling].key.module_name);
            if counts[&base] == 1 {
                names[*sibling] = base;
                continue;
            }
            let occurrence = occurrences.entry(base.clone()).or_default();
            *occurrence += 1;
            names[*sibling] = format!("{base} {occurrence}");
        }
    }
    names
}

fn visit_semantic(index: usize, builders: &[ComponentBuilder], order: &mut Vec<usize>) {
    order.push(index);
    for child in &builders[index].children {
        visit_semantic(*child, builders, order);
    }
}

fn compile_glb<'a>(structured: &'a StructuredMesh) -> CompiledGlb<'a> {
    let source = MeshSource {
        exact: &structured.exact,
        attributions: &structured.attributions,
    };

    let mut builders: Vec<ComponentBuilder> = Vec::new();
    let mut by_path: BTreeMap<Vec<SemanticFrameKey>, usize> = BTreeMap::new();
    let mut triangle_owner: Vec<Option<usize>> = vec![None; source.exact.surface_ids.len()];

    for (triangle, surface_id) in source.exact.surface_ids.iter().enumerate() {
        let surface = &source.attributions[surface_id.0 as usize];
        let path = super::structured::authored_path(&surface.provenance);
        if path.is_empty() {
            continue;
        }

        let mut semantic_path = Vec::with_capacity(path.len());
        let mut parent = None;
        for frame in path {
            semantic_path.push(SemanticFrameKey::from(&frame));
            let index = if let Some(index) = by_path.get(&semantic_path) {
                *index
            } else {
                let index = builders.len();
                builders.push(ComponentBuilder {
                    key: semantic_path.last().unwrap().clone(),
                    path: semantic_path.clone(),
                    frame,
                    parent,
                    children: Vec::new(),
                    triangles: Vec::new(),
                    attribution: AttributionStatus::Exact,
                    contributors: Vec::new(),
                });
                by_path.insert(semantic_path.clone(), index);
                if let Some(parent) = parent {
                    builders[parent].children.push(index);
                }
                index
            };
            parent = Some(index);
        }

        let owner = parent.expect("non-empty semantic path has an owner");
        triangle_owner[triangle] = Some(owner);
        builders[owner].triangles.push(triangle as u32);
        if surface.status == AttributionStatus::Ambiguous {
            builders[owner].attribution = AttributionStatus::Ambiguous;
        }
        builders[owner]
            .contributors
            .extend(surface.contributors.iter().cloned());
    }

    let builder_keys: Vec<_> = builders.iter().map(|builder| builder.key.clone()).collect();
    for builder in &mut builders {
        builder
            .children
            .sort_by(|left, right| builder_keys[*left].cmp(&builder_keys[*right]));
        builder
            .contributors
            .sort_by(|left, right| super::structured::compare_provenance_paths(left, right));
        builder.contributors.dedup();
    }
    let mut semantic_roots: Vec<_> = builders
        .iter()
        .enumerate()
        .filter_map(|(index, builder)| builder.parent.is_none().then_some(index))
        .collect();
    semantic_roots.sort_by(|left, right| builders[*left].key.cmp(&builders[*right].key));
    let semantic_names = semantic_sibling_names(&builders, &semantic_roots);

    let mut old_order = Vec::with_capacity(builders.len());
    for root in &semantic_roots {
        visit_semantic(*root, &builders, &mut old_order);
    }
    let old_to_new: HashMap<_, _> = old_order
        .iter()
        .enumerate()
        .map(|(new, old)| (*old, new))
        .collect();
    let mut components = Vec::with_capacity(builders.len());
    for old in &old_order {
        let builder = &builders[*old];
        components.push(GlbComponent {
            frame: builder.frame.clone(),
            children: builder
                .children
                .iter()
                .map(|child| old_to_new[child])
                .collect(),
            solids: Vec::new(),
            name: semantic_names[*old].clone(),
            edge_groups: vec![GlbEdgeGroup {
                source,
                selection: super::structured::make_selection(
                    source.exact,
                    source.attributions,
                    builder.triangles.clone(),
                ),
            }],
            attribution: builder.attribution,
            contributors: builder.contributors.clone(),
        });
    }

    let semantic_paths: Vec<_> = structured
        .solid_components
        .iter()
        .map(|component| solid_semantic_paths(structured, component))
        .collect();
    let names = solid_names(structured, &semantic_paths);
    let mut solids = Vec::with_capacity(structured.solid_components.len());
    let mut roots: Vec<_> = semantic_roots
        .iter()
        .map(|root| GlbRoot::Component(old_to_new[root]))
        .collect();
    for ((component, paths), name) in structured
        .solid_components
        .iter()
        .zip(&semantic_paths)
        .zip(names)
    {
        let mut groups: BTreeMap<(bool, Option<usize>, MaterialKey), Vec<u32>> = BTreeMap::new();
        let mut unowned = Vec::new();
        for triangle in &component.triangles {
            let surface =
                &source.attributions[source.exact.surface_ids[*triangle as usize].0 as usize];
            let owner = triangle_owner[*triangle as usize].map(|old| old_to_new[&old]);
            if owner.is_none() {
                unowned.push(*triangle);
            }
            groups
                .entry((owner.is_none(), owner, MaterialKey::from_surface(surface)))
                .or_default()
                .push(*triangle);
        }
        let mut owners: Vec<usize> = groups.keys().filter_map(|(_, owner, _)| *owner).collect();
        owners.dedup();

        // The deepest authored module common to every face's semantic path and
        // every owner, so the solid sits under (or at) each module that owns a
        // face of it. A path that names a module with no node of its own
        // (a contributor that kept no faces) falls back to its nearest
        // ancestor that has one.
        let mut anchor_paths: Vec<Vec<SemanticFrameKey>> = paths
            .iter()
            .map(|path| path.iter().map(SemanticFrameKey::from).collect())
            .collect();
        anchor_paths.extend(groups.keys().map(|(_, owner, _)| match owner {
            Some(owner) => builders[old_order[*owner]].path.clone(),
            None => Vec::new(),
        }));
        let mut prefix = anchor_paths.first().cloned().unwrap_or_default();
        for path in &anchor_paths[1..] {
            let common = prefix
                .iter()
                .zip(path)
                .take_while(|(left, right)| left == right)
                .count();
            prefix.truncate(common);
        }
        let anchor = loop {
            if prefix.is_empty() {
                break None;
            }
            if let Some(old) = by_path.get(&prefix) {
                break Some(old_to_new[old]);
            }
            prefix.pop();
        };

        let index = solids.len();
        match anchor {
            Some(anchor) => components[anchor].solids.push(index),
            None => roots.push(GlbRoot::Solid(index)),
        }
        solids.push(GlbSolid {
            source,
            component,
            name,
            owners,
            primitives: groups
                .into_iter()
                .map(|((_, owner, material), triangles)| GlbPrimitive {
                    owner,
                    material,
                    selection: super::structured::make_selection(
                        source.exact,
                        source.attributions,
                        triangles,
                    ),
                })
                .collect(),
            unowned_edges: (!unowned.is_empty()).then(|| GlbEdgeGroup {
                source,
                selection: super::structured::make_selection(
                    source.exact,
                    source.attributions,
                    unowned,
                ),
            }),
        });
    }

    CompiledGlb {
        components,
        solids,
        roots,
    }
}

fn validate_finite(structured: &StructuredMesh, scale: f64) -> Result<(), GeomError> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err(GeomError::Invariant(
            "source_unit_to_meters must be positive and finite".to_string(),
        ));
    }
    if structured
        .exact
        .mesh
        .verts
        .iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(GeomError::Invariant(
            "cannot export non-finite geometry".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn canonical_zero(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else {
        value
    }
}

fn gltf_point(point: [f64; 3], scale: f64, coordinates: CoordinateSystem) -> [f32; 3] {
    let point = point.map(|component| canonical_zero(component * scale) as f32);
    match coordinates {
        CoordinateSystem::YUp => [point[0], point[2], -point[1]],
        CoordinateSystem::ZUp => point,
    }
}

fn gltf_normal(normal: [f64; 3], coordinates: CoordinateSystem) -> [f32; 3] {
    match coordinates {
        CoordinateSystem::YUp => [normal[0] as f32, normal[2] as f32, -normal[1] as f32],
        CoordinateSystem::ZUp => normal.map(|component| component as f32),
    }
}

fn canonical_triangles(mesh: &Mesh, selection: &MeshSelection) -> Vec<(u32, [u32; 3])> {
    let mut triangles: Vec<_> = selection
        .triangles
        .iter()
        .map(|triangle_id| {
            let mut triangle = mesh.tris[*triangle_id as usize];
            let points = triangle.map(|vertex| {
                mesh.verts[vertex as usize].map(|value| canonical_zero(value).to_bits())
            });
            let first = (1..3).fold(0, |best, next| {
                if points[next] < points[best] {
                    next
                } else {
                    best
                }
            });
            triangle.rotate_left(first);
            (*triangle_id, triangle)
        })
        .collect();
    triangles.sort_by_key(|(triangle_id, triangle)| {
        (
            triangle.map(|vertex| {
                mesh.verts[vertex as usize].map(|value| canonical_zero(value).to_bits())
            }),
            *triangle_id,
        )
    });
    triangles
}

fn rgba_hex(rgba: [u8; 4]) -> String {
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        rgba[0], rgba[1], rgba[2], rgba[3]
    )
}

fn srgb_to_linear(value: u8) -> f32 {
    let value = f32::from(value) / 255.0;
    if value <= 0.04045 {
        value / 12.92
    } else {
        libm::powf((value + 0.055) / 1.055, 2.4)
    }
}

fn append_aligned(buffer: &mut Vec<u8>, bytes: &[u8]) -> (usize, usize) {
    while buffer.len() % 4 != 0 {
        buffer.push(0);
    }
    let offset = buffer.len();
    buffer.extend_from_slice(bytes);
    (offset, bytes.len())
}

fn f32_bytes(values: &[[f32; 3]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 12);
    for value in values {
        for component in value {
            bytes.extend_from_slice(&component.to_le_bytes());
        }
    }
    bytes
}

fn u32_bytes(values: &[u32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Appends one buffer view. `target` is `None` for data no vertex or index
/// fetch reads directly, such as the sparse merge lists of `EXT_mesh_manifold`.
fn add_view(
    views: &mut Vec<Value>,
    buffer: &mut Vec<u8>,
    bytes: &[u8],
    target: Option<u32>,
) -> usize {
    let (offset, length) = append_aligned(buffer, bytes);
    let index = views.len();
    let mut view = json!({
        "buffer": 0,
        "byteLength": length,
        "byteOffset": offset,
    });
    if let Some(target) = target {
        view["target"] = json!(target);
    }
    views.push(view);
    index
}

fn vec3_min_max(values: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for value in values {
        for axis in 0..3 {
            min[axis] = min[axis].min(value[axis]);
            max[axis] = max[axis].max(value[axis]);
        }
    }
    (min, max)
}

fn add_vec3_accessor(
    accessors: &mut Vec<Value>,
    view: usize,
    values: &[[f32; 3]],
    bounds: bool,
) -> usize {
    let index = accessors.len();
    let mut value = json!({
        "bufferView": view,
        "componentType": 5126,
        "count": values.len(),
        "type": "VEC3",
    });
    if bounds {
        let (min, max) = vec3_min_max(values);
        value["min"] = json!(min);
        value["max"] = json!(max);
    }
    accessors.push(value);
    index
}

/// Appends a `u32` scalar accessor over `count` elements of `view`, starting
/// `first` elements in.
fn add_index_accessor(
    accessors: &mut Vec<Value>,
    view: usize,
    first: usize,
    count: usize,
) -> usize {
    let index = accessors.len();
    let mut accessor = json!({
        "bufferView": view,
        "componentType": 5125,
        "count": count,
        "type": "SCALAR",
    });
    if first > 0 {
        accessor["byteOffset"] = json!(first * 4);
    }
    accessors.push(accessor);
    index
}

fn span_json(span: &SourceSpan, source_keys: &[String]) -> Value {
    let key = source_keys
        .get(span.source_id.0 as usize)
        .map(|value| safe_source_key(value))
        .unwrap_or_else(|| format!("source-{}", span.source_id.0));
    json!({
        "sourceId": span.source_id.0,
        "source": key,
        "start": span.start,
        "end": span.end,
    })
}

fn safe_source_key(key: &str) -> String {
    if key == "<main>" || !std::path::Path::new(key).is_absolute() {
        return key.replace('\\', "/");
    }
    std::path::Path::new(key)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source.scad")
        .to_string()
}

fn provenance_json(frames: &[ProvenanceFrame], source_keys: &[String]) -> Value {
    let mut frames = frames.to_vec();
    frames.sort_by_key(|frame| {
        (
            frame.call_site.source_id,
            frame.call_site.start,
            frame.call_site.end,
            frame.module_name.clone(),
        )
    });
    frames.dedup();
    Value::Array(
        frames
            .iter()
            .map(|frame| {
                json!({
                    "callSite": span_json(&frame.call_site, source_keys),
                    "definitionSite": frame.definition_site.as_ref().map(|span| span_json(span, source_keys)),
                    "moduleName": frame.module_name,
                })
            })
            .collect(),
    )
}

fn triangle_normal(mesh: &Mesh, triangle: [u32; 3]) -> [f64; 3] {
    let a = mesh.verts[triangle[0] as usize];
    let b = mesh.verts[triangle[1] as usize];
    let c = mesh.verts[triangle[2] as usize];
    normalize(cross(sub(b, a), sub(c, a)))
}

#[cfg(test)]
thread_local! {
    static EDGE_DERIVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_edge_derivation_count() {
    EDGE_DERIVATIONS.set(0);
}

#[cfg(test)]
pub(crate) fn edge_derivation_count() -> usize {
    EDGE_DERIVATIONS.get()
}

/// Two adjacent source patches are one surface along a shared boundary when the
/// sharpest dihedral anywhere on that *connected* boundary stays under this
/// angle. Deciding per component rather than per edge keeps a seam that is sharp
/// somewhere drawn along its whole length, including where it flattens toward
/// tangency; deciding per component rather than per patch pair keeps a pair whose
/// boundary is smooth in one place and creased in another from being judged by
/// either half alone.
///
/// [`crate::structured::hull_patch_ids`] reuses the same angle to split a hull's
/// operand patches into their smooth pieces, so the split and the rejoin agree.
///
/// Matches the threshold Tau's cross-kernel edge middleware applies to meshes
/// that arrive without authored lines.
pub(crate) const PATCH_MERGE_DEGREES: f64 = 30.0;

/// Geometry with no source topology — `import`, `surface`, and the ruled bands
/// of `hull` and `minkowski` — has no patches to pair up, so each edge is
/// judged on its own dihedral. The angle is lower than [`PATCH_MERGE_DEGREES`]
/// because such meshes are routinely coarse enough that a 30° cut would erase
/// their real creases.
const RAW_CREASE_DEGREES: f64 = 20.0;

fn dihedral_degrees(mesh: &Mesh, left: usize, right: usize) -> f64 {
    let left_normal = triangle_normal(mesh, mesh.tris[left]);
    let right_normal = triangle_normal(mesh, mesh.tris[right]);
    let cosine = left_normal
        .iter()
        .zip(right_normal)
        .map(|(left, right)| left * right)
        .sum::<f64>()
        .clamp(-1.0, 1.0);
    libm::acos(cosine).to_degrees()
}

/// Decides, for every edge that separates two distinct classified patches,
/// whether it is part of a boundary that creases.
///
/// The edges of one patch pair are grouped into connected components (joined
/// through shared vertices) and each component is judged by its own sharpest
/// dihedral. A pair can bound two surfaces in more than one place — two hulled
/// strokes sharing a cylinder touch tangentially along one arc and cross at a
/// notch elsewhere — and only per-component verdicts answer both correctly.
fn boundary_component_verdicts(
    mesh: &Mesh,
    source_face_ids: &[u64],
    adjacency: &BTreeMap<(u32, u32), Vec<u32>>,
) -> HashMap<(u32, u32), bool> {
    /// One edge of a patch-pair boundary: its two vertices and its dihedral.
    type BoundaryEdge = ((u32, u32), f64);

    let mut by_pair: BTreeMap<(u64, u64), Vec<BoundaryEdge>> = BTreeMap::new();
    for ((left_vertex, right_vertex), local) in adjacency {
        if local.len() != 2
            || mesh.verts[*left_vertex as usize] == mesh.verts[*right_vertex as usize]
        {
            continue;
        }
        let (left, right) = (local[0] as usize, local[1] as usize);
        let (left_patch, right_patch) = (source_face_ids[left], source_face_ids[right]);
        if left_patch == right_patch
            || left_patch == super::structured::UNCLASSIFIED_PATCH_ID
            || right_patch == super::structured::UNCLASSIFIED_PATCH_ID
        {
            continue;
        }
        by_pair
            .entry((left_patch.min(right_patch), left_patch.max(right_patch)))
            .or_default()
            .push((
                (*left_vertex, *right_vertex),
                dihedral_degrees(mesh, left, right),
            ));
    }

    let mut verdicts = HashMap::new();
    for edges in by_pair.into_values() {
        let mut slots: HashMap<u32, usize> = HashMap::new();
        let mut union = super::structured::UnionFind::new(edges.len() * 2);
        let mut anchors = Vec::with_capacity(edges.len());
        for ((left, right), _) in &edges {
            let next = slots.len();
            let left = *slots.entry(*left).or_insert(next);
            let next = slots.len();
            let right = *slots.entry(*right).or_insert(next);
            union.union(left, right);
            anchors.push(left);
        }
        let mut sharpest: HashMap<usize, f64> = HashMap::new();
        for (anchor, (_, angle)) in anchors.iter().zip(&edges) {
            let component = union.find(*anchor);
            let entry = sharpest.entry(component).or_insert(0.0);
            if *angle > *entry {
                *entry = *angle;
            }
        }
        for (anchor, (edge, _)) in anchors.iter().zip(&edges) {
            let component = union.find(*anchor);
            verdicts.insert(*edge, sharpest[&component] >= PATCH_MERGE_DEGREES);
        }
    }
    verdicts
}

fn feature_edges(source: MeshSource<'_>, selection: &MeshSelection) -> Vec<[u32; 2]> {
    #[cfg(test)]
    EDGE_DERIVATIONS.set(EDGE_DERIVATIONS.get() + 1);

    let mesh = &source.exact.mesh;
    let selected: BTreeSet<_> = selection.triangles.iter().copied().collect();
    let mut adjacency: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    for triangle_id in &selection.triangles {
        let triangle = mesh.tris[*triangle_id as usize];
        for edge in [
            [triangle[0], triangle[1]],
            [triangle[1], triangle[2]],
            [triangle[2], triangle[0]],
        ] {
            let key = if edge[0] < edge[1] {
                (edge[0], edge[1])
            } else {
                (edge[1], edge[0])
            };
            adjacency.entry(key).or_default().push(*triangle_id);
        }
    }
    let verdicts = boundary_component_verdicts(mesh, &source.exact.source_face_ids, &adjacency);

    let mut result = Vec::new();
    for ((a, b), local) in adjacency {
        if mesh.verts[a as usize] == mesh.verts[b as usize] {
            continue;
        }
        let include = if local.len() != 2 {
            true
        } else {
            let left = local[0] as usize;
            let right = local[1] as usize;
            let left_patch = source.exact.source_face_ids[left];
            let right_patch = source.exact.source_face_ids[right];
            if left_patch != super::structured::UNCLASSIFIED_PATCH_ID
                && right_patch != super::structured::UNCLASSIFIED_PATCH_ID
            {
                // Absent from the map means the two faces share a patch, so
                // there is no boundary here at all.
                verdicts.get(&(a, b)).copied().unwrap_or(false)
            } else {
                dihedral_degrees(mesh, left, right) >= RAW_CREASE_DEGREES
            }
        };
        if include && local.iter().all(|triangle| selected.contains(triangle)) {
            result.push([a, b]);
        }
    }
    result.sort_by_key(|edge| {
        let mut points = edge
            .map(|vertex| mesh.verts[vertex as usize].map(|value| canonical_zero(value).to_bits()));
        points.sort_unstable();
        points
    });
    result.dedup_by_key(|edge| {
        let mut points = edge
            .map(|vertex| mesh.verts[vertex as usize].map(|value| canonical_zero(value).to_bits()));
        points.sort_unstable();
        points
    });
    result
}

#[cfg(test)]
pub(crate) fn serialize_glb(
    structured: &StructuredMesh,
    options: &Export3DOptions,
) -> Result<Vec<u8>, GeomError> {
    serialize_glb_with_diagnostics(structured, options).map(|(bytes, _)| bytes)
}

/// Derives one owner's (or one solid's unowned) feature lines and writes them
/// as a `LINES` primitive, or returns `None` when there are none.
fn edge_primitive(
    edge_group: &GlbEdgeGroup<'_>,
    line_material: usize,
    options: &Export3DOptions,
    views: &mut Vec<Value>,
    buffer: &mut Vec<u8>,
    accessors: &mut Vec<Value>,
) -> Option<Value> {
    #[cfg(feature = "benchmark-profile")]
    let edge_started = Instant::now();
    let edges = feature_edges(edge_group.source, &edge_group.selection);
    #[cfg(feature = "benchmark-profile")]
    super::structured::record_edge_derivation(
        edge_started.elapsed().as_secs_f64() * 1_000.0,
        edges.len(),
    );
    if edges.is_empty() {
        return None;
    }
    let mut line_positions = Vec::with_capacity(edges.len() * 2);
    let mut line_indices = Vec::with_capacity(edges.len() * 2);
    for edge in edges {
        for vertex in edge {
            line_positions.push(gltf_point(
                edge_group.source.exact.mesh.verts[vertex as usize],
                options.source_unit_to_meters,
                options.coordinate_system,
            ));
            line_indices.push(line_indices.len() as u32);
        }
    }
    let position_view = add_view(views, buffer, &f32_bytes(&line_positions), Some(34962));
    let index_view = add_view(views, buffer, &u32_bytes(&line_indices), Some(34963));
    let position_accessor = add_vec3_accessor(accessors, position_view, &line_positions, true);
    let index_accessor = add_index_accessor(accessors, index_view, 0, line_indices.len());
    // `mode: 1` and the shared Feature Edges material already identify a
    // generated edge primitive; no consumer read the extras tag that used to
    // sit here.
    Some(json!({
        "attributes": { "POSITION": position_accessor },
        "indices": index_accessor,
        "material": line_material,
        "mode": 1,
    }))
}

/// Serializes a GLB and returns the export diagnostics alongside it: one per
/// solid whose index stream is not an oriented 2-manifold, which is then
/// written without `EXT_mesh_manifold` rather than fused or dropped.
pub(crate) fn serialize_glb_with_diagnostics(
    structured: &StructuredMesh,
    options: &Export3DOptions,
) -> Result<(Vec<u8>, Vec<String>), GeomError> {
    validate_finite(structured, options.source_unit_to_meters)?;

    let compiled = compile_glb(structured);
    let mut diagnostics = Vec::new();
    let mut colors: Vec<_> = compiled
        .solids
        .iter()
        .flat_map(|solid| solid.primitives.iter())
        .map(|primitive| material_rgba_from_key(primitive.material))
        .collect();
    colors.sort_unstable();
    colors.dedup();
    let material_by_color: BTreeMap<_, _> = colors
        .iter()
        .enumerate()
        .map(|(index, color)| (*color, index))
        .collect();
    let mut materials: Vec<Value> = colors
        .iter()
        .map(|rgba| {
            let mut material = json!({
                "name": format!("{} Material", rgba_hex(*rgba)),
                "pbrMetallicRoughness": {
                    "baseColorFactor": [
                        srgb_to_linear(rgba[0]),
                        srgb_to_linear(rgba[1]),
                        srgb_to_linear(rgba[2]),
                        f32::from(rgba[3]) / 255.0,
                    ],
                    "metallicFactor": 0.1,
                    "roughnessFactor": 0.6,
                }
            });
            material["doubleSided"] = json!(true);
            if rgba[3] < 255 {
                material["alphaMode"] = json!("BLEND");
            }
            material
        })
        .collect();
    let line_material = options.include_edges.then(|| {
        let index = materials.len();
        materials.push(json!({
            "name": "Feature Edges",
            "extensions": { "KHR_materials_unlit": {} },
            "pbrMetallicRoughness": {
                "baseColorFactor": [0.0, 0.0, 0.0, 1.0],
                "metallicFactor": 0.0,
                "roughnessFactor": 1.0,
            },
            "alphaMode": "OPAQUE",
            "doubleSided": true,
        }));
        index
    });

    let mut buffer = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    let solid_node = |solid: usize| compiled.components.len() + solid;

    // Authored modules: nodes 0..components, mesh-less unless they own lines.
    for component in &compiled.components {
        let mut primitives = Vec::new();
        if let Some(line_material) = line_material {
            for edge_group in &component.edge_groups {
                primitives.extend(edge_primitive(
                    edge_group,
                    line_material,
                    options,
                    &mut views,
                    &mut buffer,
                    &mut accessors,
                ));
            }
        }
        let mesh_index = if primitives.is_empty() {
            None
        } else {
            let mesh_index = meshes.len();
            meshes.push(json!({ "name": component.name, "primitives": primitives }));
            Some(mesh_index)
        };
        let attribution = match component.attribution {
            AttributionStatus::Exact => "exact",
            AttributionStatus::Ambiguous => "ambiguous",
        };
        let frame = &component.frame;
        let mut openrscad = json!({
            "attribution": attribution,
            "callSite": span_json(&frame.call_site, &options.source_keys),
            "definitionSite": frame.definition_site.as_ref().map(|span| span_json(span, &options.source_keys)),
            "moduleName": frame.module_name,
        });
        if !component.contributors.is_empty() {
            openrscad["contributors"] = Value::Array(
                component
                    .contributors
                    .iter()
                    .map(|path| provenance_json(path, &options.source_keys))
                    .collect(),
            );
        }
        let mut node = json!({
            "extras": { "openrscad": openrscad },
            "name": component.name,
        });
        if let Some(mesh_index) = mesh_index {
            node["mesh"] = json!(mesh_index);
        }
        let children: Vec<_> = component
            .children
            .iter()
            .copied()
            .chain(component.solids.iter().map(|solid| solid_node(*solid)))
            .collect();
        if !children.is_empty() {
            node["children"] = json!(children);
        }
        nodes.push(node);
    }

    // Closed solids: nodes components..components + solids, one mesh each.
    let mut edge_nodes = Vec::new();
    let mut manifold_used = false;
    for solid in &compiled.solids {
        let mesh = &solid.source.exact.mesh;
        let mut positions = Vec::new();
        let mut normals = Vec::new();
        let mut corner_vertices = Vec::new();
        let mut ranges = Vec::with_capacity(solid.primitives.len());
        for primitive in &solid.primitives {
            let start = positions.len();
            for (_, triangle) in canonical_triangles(mesh, &primitive.selection) {
                let normal = triangle_normal(mesh, triangle);
                // A zero-area triangle has no normal, and `normalize` says so with
                // a zero vector — which glTF rejects, because NORMAL must be unit
                // length. The direction is arbitrary for a face with no area, so
                // pick one rather than write an invalid file. Only the attribute
                // substitutes: `triangle_normal` stays honest for the dihedral
                // comparison, where a degenerate face reading as a crease is the
                // conservative answer.
                let normal = if normal == [0.0, 0.0, 0.0] {
                    [0.0, 0.0, 1.0]
                } else {
                    normal
                };
                let normal = gltf_normal(normal, options.coordinate_system);
                for vertex in triangle {
                    positions.push(gltf_point(
                        mesh.verts[vertex as usize],
                        options.source_unit_to_meters,
                        options.coordinate_system,
                    ));
                    normals.push(normal);
                    corner_vertices.push(vertex);
                }
            }
            ranges.push((start, positions.len() - start));
        }

        // Flat shading keeps one corner per triangle vertex, so the render
        // stream is the identity and every primitive is a slice of it.
        let render_indices: Vec<u32> = (0..positions.len() as u32).collect();
        let position_view = add_view(&mut views, &mut buffer, &f32_bytes(&positions), Some(34962));
        let normal_view = add_view(&mut views, &mut buffer, &f32_bytes(&normals), Some(34962));
        let index_view = add_view(
            &mut views,
            &mut buffer,
            &u32_bytes(&render_indices),
            Some(34963),
        );
        let position_accessor = add_vec3_accessor(&mut accessors, position_view, &positions, true);
        let normal_accessor = add_vec3_accessor(&mut accessors, normal_view, &normals, false);
        let mut primitives = Vec::with_capacity(solid.primitives.len());
        for (primitive, (start, count)) in solid.primitives.iter().zip(&ranges) {
            let index_accessor = add_index_accessor(&mut accessors, index_view, *start, *count);
            let mut value = json!({
                "attributes": { "NORMAL": normal_accessor, "POSITION": position_accessor },
                "indices": index_accessor,
                "material": material_by_color[&material_rgba_from_key(primitive.material)],
                "mode": 4,
            });
            let mut extras = serde_json::Map::new();
            if structured.include_display_modes {
                extras.insert(
                    "displayMode".to_string(),
                    json!(primitive.material.display_mode()),
                );
            }
            if let Some(owner) = primitive.owner {
                extras.insert("owner".to_string(), json!(owner));
            }
            if !extras.is_empty() {
                value["extras"] = json!({ "openrscad": extras });
            }
            primitives.push(value);
        }

        let mut mesh_json = json!({ "name": solid.name, "primitives": primitives });
        match validate_closed_component("GLB solid", mesh, solid.component)
            .and_then(|()| validate_vertex_links(mesh, solid.component))
        {
            Ok(()) => {
                // The exact mesh is indexed, so a corner's canonical vertex is
                // the first corner that names the same exact vertex. Merged
                // corners come from one exact vertex and so carry identical
                // positions, as `EXT_mesh_manifold` requires.
                let mut canonical: HashMap<u32, u32> = HashMap::new();
                let mut merge_indices = Vec::new();
                let mut merge_values = Vec::new();
                for (corner, vertex) in corner_vertices.iter().enumerate() {
                    let corner = corner as u32;
                    let first = *canonical.entry(*vertex).or_insert(corner);
                    if first != corner {
                        merge_indices.push(corner);
                        merge_values.push(first);
                    }
                }
                let manifold_accessor =
                    add_index_accessor(&mut accessors, index_view, 0, render_indices.len());
                let mut extension = json!({
                    "manifoldPrimitive": {
                        "attributes": { "POSITION": position_accessor },
                        "indices": manifold_accessor,
                        "mode": 4,
                    },
                });
                if !merge_indices.is_empty() {
                    let mut merge_bytes = u32_bytes(&merge_indices);
                    merge_bytes.extend_from_slice(&u32_bytes(&merge_values));
                    let merge_view = add_view(&mut views, &mut buffer, &merge_bytes, None);
                    let values_offset = merge_indices.len() * 4;
                    let merge_indices_accessor =
                        add_index_accessor(&mut accessors, merge_view, 0, merge_indices.len());
                    let merge_values_accessor = add_index_accessor(
                        &mut accessors,
                        merge_view,
                        merge_indices.len(),
                        merge_values.len(),
                    );
                    accessors[manifold_accessor]["sparse"] = json!({
                        "count": merge_indices.len(),
                        "indices": { "bufferView": merge_view, "componentType": 5125 },
                        "values": { "bufferView": merge_view, "byteOffset": values_offset },
                    });
                    extension["mergeIndices"] = json!(merge_indices_accessor);
                    extension["mergeValues"] = json!(merge_values_accessor);
                }
                mesh_json["extensions"] = json!({ "EXT_mesh_manifold": extension });
                manifold_used = true;
            }
            Err(error) => diagnostics.push(format!(
                "GLB solid \"{}\" was exported without EXT_mesh_manifold: {error}",
                solid.name
            )),
        }
        let mesh_index = meshes.len();
        meshes.push(mesh_json);

        let attribution = match solid.component.attribution {
            AttributionStatus::Exact => "exact",
            AttributionStatus::Ambiguous => "ambiguous",
        };
        let mut openrscad = json!({
            "attribution": attribution,
            "owners": solid.owners,
            "solid": true,
        });
        if solid.owners.is_empty() {
            // No module owns any face, so no module node carries its
            // provenance: the solid node keeps it, as fallback parts did.
            let mut contributors: Vec<_> = solid
                .component
                .triangles
                .iter()
                .flat_map(|triangle| {
                    let surface_id = solid.source.exact.surface_ids[*triangle as usize];
                    solid.source.attributions[surface_id.0 as usize]
                        .contributors
                        .iter()
                        .cloned()
                })
                .collect();
            contributors
                .sort_by(|left, right| super::structured::compare_provenance_paths(left, right));
            contributors.dedup();
            openrscad["fallback"] = json!(true);
            openrscad["provenance"] =
                provenance_json(&solid.component.provenance, &options.source_keys);
            if !contributors.is_empty() {
                openrscad["contributors"] = Value::Array(
                    contributors
                        .iter()
                        .map(|path| provenance_json(path, &options.source_keys))
                        .collect(),
                );
            }
        }
        nodes.push(json!({
            "extras": { "openrscad": openrscad },
            "mesh": mesh_index,
            "name": solid.name,
        }));

        if let (Some(line_material), Some(edge_group)) = (line_material, &solid.unowned_edges) {
            if let Some(primitive) = edge_primitive(
                edge_group,
                line_material,
                options,
                &mut views,
                &mut buffer,
                &mut accessors,
            ) {
                let name = format!("{} Edges", solid.name);
                let mesh_index = meshes.len();
                meshes.push(json!({ "name": name, "primitives": [primitive] }));
                edge_nodes.push((nodes.len() - 1, json!({ "mesh": mesh_index, "name": name })));
            }
        }
    }
    for (parent, node) in edge_nodes {
        nodes[parent]["children"] = json!([nodes.len()]);
        nodes.push(node);
    }
    while buffer.len() % 4 != 0 {
        buffer.push(0);
    }
    let roots: Vec<_> = compiled
        .roots
        .iter()
        .map(|root| match root {
            GlbRoot::Component(component) => *component,
            GlbRoot::Solid(solid) => solid_node(*solid),
        })
        .collect();
    let mut scene = json!({});
    if !roots.is_empty() {
        scene["nodes"] = json!(roots);
    }
    let mut document = json!({
        "asset": {
            "generator": "OpenRSCAD",
            "version": "2.0",
            "extras": {
                "openrscad": {
                    "schemaVersion": 2,
                    "status": "experimental",
                    "sources": options.source_keys.iter().enumerate().map(|(source_id, source)| {
                        json!({ "sourceId": source_id, "source": safe_source_key(source) })
                    }).collect::<Vec<_>>(),
                }
            }
        },
        "scene": 0,
        "scenes": [scene],
    });
    // glTF gives every one of these arrays `minItems: 1`, so a model that
    // renders to nothing — an empty difference, a collapsed offset — has to omit
    // them rather than write them empty. Same for the BIN chunk below: a
    // zero-length buffer is invalid, not merely wasteful.
    for (key, value) in [
        ("accessors", json!(accessors)),
        ("bufferViews", json!(views)),
        ("materials", json!(materials)),
        ("meshes", json!(meshes)),
        ("nodes", json!(nodes)),
    ] {
        if !value.as_array().is_some_and(|entries| entries.is_empty()) {
            document[key] = value;
        }
    }
    if !buffer.is_empty() {
        document["buffers"] = json!([{ "byteLength": buffer.len() }]);
    }
    let mut extensions_used = Vec::new();
    if manifold_used {
        extensions_used.push("EXT_mesh_manifold");
    }
    if options.include_edges {
        extensions_used.push("KHR_materials_unlit");
    }
    if !extensions_used.is_empty() {
        document["extensionsUsed"] = json!(extensions_used);
    }
    let mut json_bytes = serde_json::to_vec(&document)
        .map_err(|error| GeomError::Invariant(format!("GLB JSON serialization failed: {error}")))?;
    while json_bytes.len() % 4 != 0 {
        json_bytes.push(b' ');
    }
    let binary_chunk = if buffer.is_empty() {
        0
    } else {
        8 + buffer.len()
    };
    let total = 12 + 8 + json_bytes.len() + binary_chunk;
    let total =
        u32::try_from(total).map_err(|_| GeomError::Invariant("GLB exceeds 4 GiB".to_string()))?;
    let mut glb = Vec::with_capacity(total as usize);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&total.to_le_bytes());
    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json_bytes);
    if !buffer.is_empty() {
        glb.extend_from_slice(&(buffer.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&buffer);
    }
    Ok((glb, diagnostics))
}

fn xml_number(value: f64) -> Result<String, GeomError> {
    if !value.is_finite() {
        return Err(GeomError::Invariant(
            "cannot export non-finite geometry".to_string(),
        ));
    }
    Ok(canonical_zero(value).to_string())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

type ColoredTriangle = ([u32; 3], [u8; 4]);

fn compact_component(
    structured: &StructuredMesh,
    selection: &MeshSelection,
) -> (Vec<[f64; 3]>, Vec<ColoredTriangle>) {
    let triangles = canonical_triangles(&structured.exact.mesh, selection);
    let mut remap = HashMap::new();
    let mut vertices = Vec::new();
    let mut compact = Vec::with_capacity(triangles.len());
    for (triangle_id, triangle) in triangles {
        let triangle = triangle.map(|vertex| {
            *remap.entry(vertex).or_insert_with(|| {
                vertices.push(structured.exact.mesh.verts[vertex as usize]);
                vertices.len() as u32 - 1
            })
        });
        let surface =
            &structured.attributions[structured.exact.surface_ids[triangle_id as usize].0 as usize];
        compact.push((triangle, rgba_bytes(surface.rgba)));
    }
    (vertices, compact)
}

/// Requires `selection` to be an oriented 2-manifold over the exact mesh's
/// vertex indices: no collapsed triangle, and every edge shared by exactly two
/// triangles that traverse it in opposite directions. `subject` names the
/// exported object in the error.
fn validate_closed_component(
    subject: &str,
    mesh: &Mesh,
    selection: &MeshSelection,
) -> Result<(), GeomError> {
    let mut edges: BTreeMap<(u32, u32), (usize, i32)> = BTreeMap::new();
    for triangle_id in &selection.triangles {
        let triangle = mesh.tris[*triangle_id as usize];
        if triangle[0] == triangle[1] || triangle[1] == triangle[2] || triangle[2] == triangle[0] {
            return Err(GeomError::NonManifold(format!(
                "{subject} contains a degenerate triangle"
            )));
        }
        for [from, to] in [
            [triangle[0], triangle[1]],
            [triangle[1], triangle[2]],
            [triangle[2], triangle[0]],
        ] {
            let key = if from < to { (from, to) } else { (to, from) };
            let edge = edges.entry(key).or_default();
            edge.0 += 1;
            edge.1 += if from < to { 1 } else { -1 };
        }
    }
    if let Some((edge, incidence)) = edges
        .into_iter()
        .find(|(_, (count, orientation))| *count != 2 || *orientation != 0)
    {
        return Err(GeomError::NonManifold(format!(
            "{subject} edge {}-{} has {} incident triangles and orientation balance {}",
            edge.0, edge.1, incidence.0, incidence.1
        )));
    }
    Ok(())
}

/// Requires the triangles around every vertex of `selection` to form one fan,
/// so that a vertex two otherwise separate sheets merely touch (a pinch) is
/// not declared manifold. Assumes [`validate_closed_component`] passed.
fn validate_vertex_links(mesh: &Mesh, selection: &MeshSelection) -> Result<(), GeomError> {
    let mut links: BTreeMap<u32, Vec<(u32, u32)>> = BTreeMap::new();
    for triangle_id in &selection.triangles {
        let [a, b, c] = mesh.tris[*triangle_id as usize];
        links.entry(a).or_default().push((b, c));
        links.entry(b).or_default().push((c, a));
        links.entry(c).or_default().push((a, b));
    }
    for (vertex, link) in links {
        let mut slots: BTreeMap<u32, usize> = BTreeMap::new();
        for (left, right) in &link {
            let next = slots.len();
            slots.entry(*left).or_insert(next);
            let next = slots.len();
            slots.entry(*right).or_insert(next);
        }
        let mut union = super::structured::UnionFind::new(slots.len());
        for (left, right) in &link {
            union.union(slots[left], slots[right]);
        }
        let root = union.find(0);
        if (1..slots.len()).any(|slot| union.find(slot) != root) {
            return Err(GeomError::NonManifold(format!(
                "GLB solid vertex {vertex} joins separate triangle fans"
            )));
        }
    }
    Ok(())
}

fn solid_semantic_paths(
    structured: &StructuredMesh,
    selection: &MeshSelection,
) -> Vec<Vec<ProvenanceFrame>> {
    let mut paths: Vec<_> = selection
        .triangles
        .iter()
        .flat_map(|triangle| {
            let surface_id = structured.exact.surface_ids[*triangle as usize];
            let surface = &structured.attributions[surface_id.0 as usize];
            if surface.contributors.is_empty() {
                vec![super::structured::authored_path(&surface.provenance)]
            } else {
                surface.contributors.clone()
            }
        })
        .collect();
    paths.sort_by(|left, right| super::structured::compare_provenance_paths(left, right));
    paths.dedup();
    paths
}

fn solid_base_name(
    structured: &StructuredMesh,
    selection: &MeshSelection,
    paths: &[Vec<ProvenanceFrame>],
) -> String {
    if paths.len() == 1 {
        if let Some(module_name) = paths[0]
            .last()
            .and_then(|frame| frame.module_name.as_deref())
        {
            return humanize_identifier(module_name);
        }
        let triangle = selection.triangles[0] as usize;
        let surface = &structured.attributions[structured.exact.surface_ids[triangle].0 as usize];
        return format!("{} Shape", rgba_hex(rgba_bytes(surface.rgba)));
    }
    super::structured::longest_common_prefix(paths)
        .last()
        .and_then(|frame| frame.module_name.as_deref())
        .map(|module_name| format!("{} Solid", humanize_identifier(module_name)))
        .unwrap_or_else(|| "Shape".to_string())
}

fn solid_names(structured: &StructuredMesh, paths: &[Vec<Vec<ProvenanceFrame>>]) -> Vec<String> {
    let bases: Vec<_> = paths
        .iter()
        .zip(&structured.solid_components)
        .map(|(paths, selection)| solid_base_name(structured, selection, paths))
        .collect();
    let mut counts = BTreeMap::new();
    for base in &bases {
        *counts.entry(base.clone()).or_insert(0usize) += 1;
    }
    let mut occurrences = BTreeMap::new();
    bases
        .into_iter()
        .map(|base| {
            if counts[&base] == 1 {
                return base;
            }
            let occurrence = occurrences.entry(base.clone()).or_insert(0usize);
            *occurrence += 1;
            format!("{base} {occurrence}")
        })
        .collect()
}

fn ordered_provenance_json(frames: &[ProvenanceFrame], source_keys: &[String]) -> Value {
    Value::Array(
        frames
            .iter()
            .map(|frame| {
                json!({
                    "callSite": span_json(&frame.call_site, source_keys),
                    "definitionSite": frame.definition_site.as_ref().map(|span| span_json(span, source_keys)),
                    "moduleName": frame.module_name,
                })
            })
            .collect(),
    )
}

pub(crate) fn serialize_3mf(
    structured: &StructuredMesh,
    options: &Export3DOptions,
) -> Result<Vec<u8>, GeomError> {
    validate_finite(structured, 1.0)?;
    let mut colors: Vec<_> = structured
        .exact
        .surface_ids
        .iter()
        .map(|id| rgba_bytes(structured.attributions[id.0 as usize].rgba))
        .collect();
    colors.sort_unstable();
    colors.dedup();
    let material_by_color: BTreeMap<_, _> = colors
        .iter()
        .enumerate()
        .map(|(index, color)| (*color, index))
        .collect();
    let semantic_paths: Vec<_> = structured
        .solid_components
        .iter()
        .map(|component| solid_semantic_paths(structured, component))
        .collect();
    let names = solid_names(structured, &semantic_paths);
    let source_table = serde_json::to_string(
        &options
            .source_keys
            .iter()
            .enumerate()
            .map(|(source_id, source)| {
                json!({ "sourceId": source_id, "source": safe_source_key(source) })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| GeomError::Invariant(format!("3MF source serialization failed: {error}")))?;
    let mut model = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <model unit=\"millimeter\" xml:lang=\"en-US\" xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" xmlns:openrscad=\"https://openrscad.com/3mf/metadata/1\">\n\
         <metadata name=\"openrscad:schemaVersion\" preserve=\"true\">1</metadata>\n\
         <metadata name=\"openrscad:status\" preserve=\"true\">experimental</metadata>\n",
    );
    model.push_str(&format!(
        "<metadata name=\"openrscad:sourceTable\" preserve=\"true\">{}</metadata><resources>\n",
        xml_escape(&source_table)
    ));
    if !colors.is_empty() {
        model.push_str("<basematerials id=\"1\">\n");
        for color in &colors {
            model.push_str(&format!(
                "<base name=\"{}\" displaycolor=\"#{:02X}{:02X}{:02X}{:02X}\"/>\n",
                xml_escape(&rgba_hex(*color)),
                color[0],
                color[1],
                color[2],
                color[3]
            ));
        }
        model.push_str("</basematerials>\n");
    }
    for (component_index, component) in structured.solid_components.iter().enumerate() {
        validate_closed_component("3MF object", &structured.exact.mesh, component)?;
        let (vertices, triangles) = compact_component(structured, component);
        if triangles.is_empty() {
            continue;
        }
        let object_id = component_index + 2;
        let default_material = material_by_color[&triangles[0].1];
        let attribution = match component.attribution {
            AttributionStatus::Exact => "exact",
            AttributionStatus::Ambiguous => "ambiguous",
        };
        let owners = Value::Array(
            semantic_paths[component_index]
                .iter()
                .map(|path| ordered_provenance_json(path, &options.source_keys))
                .collect(),
        );
        let owners = serde_json::to_string(&owners).map_err(|error| {
            GeomError::Invariant(format!("3MF provenance serialization failed: {error}"))
        })?;
        model.push_str(&format!(
            "<object id=\"{object_id}\" type=\"model\" name=\"{}\" pid=\"1\" pindex=\"{default_material}\"><metadatagroup><metadata name=\"openrscad:semanticOwners\" preserve=\"true\">{}</metadata><metadata name=\"openrscad:attribution\" preserve=\"true\">{attribution}</metadata></metadatagroup><mesh><vertices>\n",
            xml_escape(&names[component_index]),
            xml_escape(&owners),
        ));
        for vertex in vertices {
            model.push_str(&format!(
                "<vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n",
                xml_number(vertex[0])?,
                xml_number(vertex[1])?,
                xml_number(vertex[2])?
            ));
        }
        model.push_str("</vertices><triangles>\n");
        for (triangle, color) in triangles {
            let material = material_by_color[&color];
            model.push_str(&format!(
                "<triangle v1=\"{}\" v2=\"{}\" v3=\"{}\" pid=\"1\" p1=\"{material}\" p2=\"{material}\" p3=\"{material}\"/>\n",
                triangle[0], triangle[1], triangle[2]
            ));
        }
        model.push_str("</triangles></mesh></object>\n");
    }
    model.push_str("</resources><build>\n");
    for component_index in 0..structured.solid_components.len() {
        model.push_str(&format!("<item objectid=\"{}\"/>\n", component_index + 2));
    }
    model.push_str("</build></model>\n");
    Ok(package_3mf(&model))
}

#[cfg(test)]
pub(crate) fn parse_glb_json(bytes: &[u8]) -> Value {
    assert!(bytes.len() >= 20 && &bytes[..4] == b"glTF");
    let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    serde_json::from_slice(&bytes[20..20 + length]).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{render_structured_rust_cached, GeomCache, RustManifoldKernel};
    use openrscad_ir::{FragmentSpec, Node, ProvenanceFrame, SourceId, SourceSpan};

    fn glb_json(bytes: &[u8]) -> serde_json::Value {
        let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        serde_json::from_slice(&bytes[20..20 + length]).unwrap()
    }

    fn line_segment_count(document: &Value) -> u64 {
        document["meshes"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|mesh| mesh["primitives"].as_array().into_iter().flatten())
            .filter(|primitive| primitive["mode"] == 1)
            .map(|primitive| {
                let accessor = primitive["indices"].as_u64().unwrap() as usize;
                document["accessors"][accessor]["count"].as_u64().unwrap() / 2
            })
            .sum()
    }

    fn two_cubes() -> StructuredMesh {
        render_structured_rust_cached(
            &Node::Union(vec![
                Node::Cube {
                    size: [1.0, 1.0, 1.0],
                    center: false,
                },
                Node::Translate {
                    v: [0.0, 0.0, 2.0],
                    child: Box::new(Node::Cube {
                        size: [1.0, 1.0, 1.0],
                        center: false,
                    }),
                },
            ]),
            &RustManifoldKernel::new(),
            &mut GeomCache::new(),
        )
        .unwrap()
    }

    fn authored(name: &str, call_start: u32, child: Node) -> Node {
        Node::Provenance {
            frame: ProvenanceFrame {
                call_site: SourceSpan {
                    source_id: SourceId(0),
                    start: call_start,
                    end: call_start + name.len() as u32,
                },
                definition_site: Some(SourceSpan {
                    source_id: SourceId(0),
                    start: call_start + 1_000,
                    end: call_start + 1_100,
                }),
                module_name: Some(name.to_string()),
            },
            child: Box::new(child),
        }
    }

    fn rendered(node: &Node) -> StructuredMesh {
        render_structured_rust_cached(node, &RustManifoldKernel::new(), &mut GeomCache::new())
            .unwrap()
    }

    fn surface_triangle_count(document: &Value) -> u64 {
        document["meshes"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|mesh| mesh["primitives"].as_array().unwrap())
            .filter(|primitive| primitive["mode"] == 4)
            .map(|primitive| {
                let accessor = primitive["indices"].as_u64().unwrap() as usize;
                document["accessors"][accessor]["count"].as_u64().unwrap() / 3
            })
            .sum()
    }

    fn node_surface_triangle_count(document: &Value, node_index: usize) -> u64 {
        let Some(mesh_index) = document["nodes"][node_index]["mesh"].as_u64() else {
            return 0;
        };
        document["meshes"][mesh_index as usize]["primitives"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|primitive| primitive["mode"] == 4)
            .map(|primitive| {
                let accessor = primitive["indices"].as_u64().unwrap() as usize;
                document["accessors"][accessor]["count"].as_u64().unwrap() / 3
            })
            .sum()
    }

    fn threemf_document(bytes: &[u8]) -> String {
        crate::mesh::read_3mf_model(bytes).unwrap()
    }

    fn threemf_object_names(model: &roxmltree::Document<'_>) -> Vec<String> {
        model
            .descendants()
            .filter(|node| node.has_tag_name("object"))
            .map(|node| node.attribute("name").unwrap().to_string())
            .collect()
    }

    fn open_triangle_scene() -> StructuredMesh {
        let scene = AttributedMesh {
            mesh: Mesh {
                verts: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                tris: vec![[0, 1, 2]],
            },
            surface_ids: vec![super::super::SurfaceAttributionId(0)],
            source_face_ids: vec![0],
        };
        let selection = MeshSelection {
            triangles: vec![0],
            bounds: super::super::structured::Bounds3 {
                min: [0.0, 0.0, 0.0],
                max: [1.0, 1.0, 0.0],
            },
            provenance: Vec::new(),
            attribution: AttributionStatus::Exact,
            geometry_hash: 0,
        };
        StructuredMesh {
            aggregate: scene.mesh.clone(),
            exact: scene,
            include_display_modes: false,
            solid_components: vec![selection],
            attributions: vec![SurfaceAttribution {
                rgba: [1.0, 0.0, 0.0, 1.0],
                mode: DisplayMode::Solid,
                provenance: Vec::new(),
                contributors: Vec::new(),
                contribution: super::super::ContributionKind::Boundary,
                status: AttributionStatus::Exact,
            }],
        }
    }

    #[test]
    fn threemf_rejects_open_physical_objects() {
        let error = serialize_3mf(&open_triangle_scene(), &Export3DOptions::default()).unwrap_err();
        assert!(matches!(error, GeomError::NonManifold(_)));
    }

    #[test]
    fn touching_authored_modules_emit_exact_nested_semantic_nodes() {
        let node = authored(
            "assembly",
            10,
            Node::Union(vec![
                authored(
                    "lower_frame",
                    20,
                    Node::Cube {
                        size: [10.0, 10.0, 2.0],
                        center: false,
                    },
                ),
                authored(
                    "roof_frame",
                    30,
                    Node::Translate {
                        v: [0.0, 0.0, 2.0],
                        child: Box::new(Node::Cube {
                            size: [10.0, 10.0, 2.0],
                            center: false,
                        }),
                    },
                ),
            ]),
        );
        let structured = rendered(&node);
        assert_eq!(structured.exact.mesh.tris.len(), 20);

        let bytes = serialize_glb(&structured, &Export3DOptions::default()).unwrap();
        let document = glb_json(&bytes);
        assert_eq!(surface_triangle_count(&document), 20);
        assert_eq!(document["scenes"][0]["nodes"], json!([0]));
        assert_eq!(document["nodes"][0]["name"], "Assembly");
        assert_eq!(document["nodes"][0]["children"], json!([1, 2, 3]));
        assert_eq!(document["nodes"][1]["name"], "Lower Frame");
        assert_eq!(document["nodes"][2]["name"], "Roof Frame");
        for module in 0..3 {
            assert!(document["nodes"][module].get("mesh").is_none());
        }
        // The two frames touch, so they are one solid under their common owner,
        // its faces tagged with the frame that owns them.
        let solid = &document["nodes"][3];
        assert_eq!(solid["name"], "Assembly Solid");
        assert_eq!(
            primitive_owners(&document, solid),
            [(json!(1), 10), (json!(2), 10)]
        );
        assert_eq!(
            document["nodes"][2]["extras"]["openrscad"]["moduleName"],
            "roof_frame"
        );
        assert_eq!(
            document["nodes"][2]["extras"]["openrscad"]["callSite"],
            json!({ "sourceId": 0, "source": "<main>", "start": 30, "end": 40 })
        );
    }

    #[test]
    fn overlapping_structural_siblings_keep_complete_authored_geometry() {
        let node = Node::Group(vec![
            authored(
                "left_part",
                10,
                Node::Cube {
                    size: [2.0, 2.0, 2.0],
                    center: false,
                },
            ),
            authored(
                "right_part",
                20,
                Node::Translate {
                    v: [1.0, 0.0, 0.0],
                    child: Box::new(Node::Cube {
                        size: [2.0, 2.0, 2.0],
                        center: false,
                    }),
                },
            ),
        ]);
        let structured = rendered(&node);
        let document = glb_json(&serialize_glb(&structured, &Export3DOptions::default()).unwrap());
        let names: Vec<_> = document["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap())
            .collect();

        // Unfused siblings stay two overlapping solids, one under each module.
        assert_eq!(
            names,
            ["Left Part", "Right Part", "Left Part", "Right Part"]
        );
        assert_eq!(document["nodes"][0]["children"], json!([2]));
        assert_eq!(document["nodes"][1]["children"], json!([3]));
        assert_eq!(node_surface_triangle_count(&document, 0), 0);
        assert_eq!(node_surface_triangle_count(&document, 1), 0);
        assert_eq!(node_surface_triangle_count(&document, 2), 12);
        assert_eq!(node_surface_triangle_count(&document, 3), 12);
        assert!((structured.aggregate.volume() - 12.0).abs() < 1e-6);

        let model_xml =
            threemf_document(&serialize_3mf(&structured, &Export3DOptions::default()).unwrap());
        let model = roxmltree::Document::parse(&model_xml).unwrap();
        assert_eq!(threemf_object_names(&model), ["Left Part", "Right Part"]);
    }

    #[test]
    fn smooth_sphere_has_no_tessellation_feature_lines() {
        let structured = rendered(&Node::Sphere {
            r: 10.0,
            frags: FragmentSpec {
                fn_: 32.0,
                ..FragmentSpec::default()
            },
        });
        let document = glb_json(
            &serialize_glb(
                &structured,
                &Export3DOptions {
                    include_edges: true,
                    ..Export3DOptions::default()
                },
            )
            .unwrap(),
        );
        let primitives = document["meshes"][0]["primitives"].as_array().unwrap();

        assert_eq!(primitives.len(), 1);
        assert_eq!(primitives[0]["mode"], 4);
    }

    #[test]
    fn export_omits_preview_policy_while_render_glb_keeps_factual_display_modes() {
        let node = Node::Group(vec![
            Node::Highlight(Box::new(Node::Cube {
                size: [1.0, 1.0, 1.0],
                center: false,
            })),
            Node::Background(Box::new(Node::Translate {
                v: [0.0, 0.0, 2.0],
                child: Box::new(Node::Cube {
                    size: [1.0, 1.0, 1.0],
                    center: false,
                }),
            })),
        ]);
        let export =
            glb_json(&serialize_glb(&rendered(&node), &Export3DOptions::default()).unwrap());
        let (render, _) = crate::render_structured_cached_diag(
            &node,
            &RustManifoldKernel::new(),
            &mut GeomCache::new(),
            true,
        )
        .unwrap();
        let render = glb_json(&serialize_glb(&render, &Export3DOptions::default()).unwrap());

        assert_eq!(surface_triangle_count(&export), 12);
        assert!(export["meshes"][0]["primitives"][0]["extras"].is_null());
        let modes: BTreeSet<_> = render["meshes"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|mesh| mesh["primitives"].as_array().unwrap())
            .map(|primitive| {
                primitive["extras"]["openrscad"]["displayMode"]
                    .as_str()
                    .unwrap()
            })
            .collect();
        assert_eq!(modes, BTreeSet::from(["background", "highlight"]));
    }

    #[test]
    fn glb_vendor_contract_is_declared_experimental_version_two() {
        let document = glb_json(&serialize_glb(&two_cubes(), &Export3DOptions::default()).unwrap());

        assert_eq!(document["asset"]["extras"]["openrscad"]["schemaVersion"], 2);
        assert_eq!(document["extensionsUsed"], json!(["EXT_mesh_manifold"]));
        assert_eq!(
            document["asset"]["extras"]["openrscad"]["status"],
            "experimental"
        );
        assert_eq!(
            document["materials"][0]["pbrMetallicRoughness"]["metallicFactor"],
            0.1
        );
        assert_eq!(
            document["materials"][0]["pbrMetallicRoughness"]["roughnessFactor"],
            0.6
        );
        assert_eq!(document["materials"][0]["doubleSided"], true);
        assert_eq!(document["materials"][0]["name"], "#F5A523FF Material");
    }

    /// `linear_extrude(2) hull() projection(cut=true) sphere(8, $fn=48)` — the
    /// corpus's `d2_proj_hull` — triangulates its caps with zero-area slivers,
    /// and a sliver has no normal to compute. glTF requires NORMAL to be unit
    /// length, so writing the honest zero vector produced a file every validator
    /// rejects.
    #[test]
    fn degenerate_faces_still_export_unit_normals() {
        let frags = FragmentSpec {
            fn_: 48.0,
            ..FragmentSpec::default()
        };
        let structured = rendered(&Node::LinearExtrude {
            height: 2.0,
            center: false,
            twist: 0.0,
            scale: [1.0, 1.0],
            slices: 1,
            child: Box::new(Node::Hull(vec![Node::Projection {
                cut: true,
                child: Box::new(Node::Sphere { r: 8.0, frags }),
            }])),
        });
        let bytes = export_3d(
            &structured,
            ExportFormat3D::Glb,
            &Export3DOptions::default(),
        )
        .unwrap()
        .bytes;
        let document = glb_json(&bytes);

        let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let binary = &bytes[20 + json_length + 8..];
        let mut checked = 0;
        for mesh in document["meshes"].as_array().unwrap() {
            for primitive in mesh["primitives"].as_array().unwrap() {
                let Some(accessor_index) = primitive["attributes"]["NORMAL"].as_u64() else {
                    continue;
                };
                let accessor = &document["accessors"][accessor_index as usize];
                let view =
                    &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
                let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
                for index in 0..accessor["count"].as_u64().unwrap() as usize {
                    let at = start + index * 12;
                    let component = |offset: usize| {
                        f32::from_le_bytes(binary[at + offset..at + offset + 4].try_into().unwrap())
                            as f64
                    };
                    let length =
                        (component(0).powi(2) + component(4).powi(2) + component(8).powi(2)).sqrt();
                    assert!(
                        (length - 1.0).abs() < 1e-5,
                        "normal {index} has length {length}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0);
    }

    /// Deliberately empty models are in the corpus — an empty difference, a
    /// collapsed offset — and the writer used to hand them a GLB with empty
    /// arrays and a zero-length BIN chunk, which the Khronos validator rejects
    /// outright. Nothing is a legal thing to export; an invalid file is not.
    #[test]
    fn a_model_that_renders_to_nothing_exports_a_valid_empty_document() {
        let structured = rendered(&Node::Difference(vec![
            Node::Cube {
                size: [1.0, 1.0, 1.0],
                center: true,
            },
            Node::Cube {
                size: [4.0, 4.0, 4.0],
                center: true,
            },
        ]));
        let options = Export3DOptions {
            include_edges: true,
            ..Export3DOptions::default()
        };
        let bytes = export_3d(&structured, ExportFormat3D::Glb, &options)
            .unwrap()
            .bytes;
        let document = glb_json(&bytes);

        for key in ["accessors", "bufferViews", "buffers", "meshes", "nodes"] {
            assert!(
                document.get(key).is_none(),
                "{key} must be omitted, not empty"
            );
        }
        assert!(document["scenes"][0].get("nodes").is_none());
        assert_eq!(document["scene"], 0);
        // Header plus one JSON chunk, and no BIN chunk at all.
        let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        assert_eq!(bytes.len(), 12 + 8 + json_length);
    }

    #[test]
    fn smooth_extrusions_do_not_emit_tessellation_edges() {
        let circle = || Node::Circle {
            r: 10.0,
            frags: FragmentSpec {
                fn_: 16.0,
                ..FragmentSpec::default()
            },
        };
        let linear = rendered(&Node::LinearExtrude {
            height: 10.0,
            center: false,
            twist: 0.0,
            scale: [1.0, 1.0],
            slices: 1,
            child: Box::new(circle()),
        });
        let revolved = rendered(&Node::RotateExtrude {
            angle: 360.0,
            frags: FragmentSpec {
                fn_: 16.0,
                ..FragmentSpec::default()
            },
            child: Box::new(Node::Translate {
                v: [20.0, 0.0, 0.0],
                child: Box::new(circle()),
            }),
        });
        let options = Export3DOptions {
            include_edges: true,
            ..Export3DOptions::default()
        };

        assert_eq!(
            line_segment_count(&glb_json(&serialize_glb(&linear, &options).unwrap())),
            32
        );
        assert_eq!(
            line_segment_count(&glb_json(&serialize_glb(&revolved, &options).unwrap())),
            0
        );
    }

    #[test]
    fn cylinder_edges_are_only_cap_boundaries() {
        let structured = rendered(&Node::Cylinder {
            h: 10.0,
            r1: 4.0,
            r2: 4.0,
            center: false,
            frags: FragmentSpec {
                fn_: 16.0,
                ..FragmentSpec::default()
            },
        });
        let document = glb_json(
            &serialize_glb(
                &structured,
                &Export3DOptions {
                    include_edges: true,
                    ..Export3DOptions::default()
                },
            )
            .unwrap(),
        );

        assert_eq!(line_segment_count(&document), 32);
    }

    #[cfg(feature = "cpp-relation")]
    #[test]
    fn rust_and_native_relations_preserve_the_same_boolean_feature_edges() {
        use crate::{render_structured_native_cached, ManifoldKernel};

        let node = Node::Difference(vec![
            Node::Cube {
                size: [4.0, 4.0, 4.0],
                center: false,
            },
            Node::Translate {
                v: [1.0, 1.0, 2.0],
                child: Box::new(Node::Cube {
                    size: [2.0, 2.0, 4.0],
                    center: false,
                }),
            },
        ]);
        let rust = rendered(&node);
        let native =
            render_structured_native_cached(&node, &ManifoldKernel::new(), &mut GeomCache::new())
                .unwrap();
        let options = Export3DOptions {
            include_edges: true,
            ..Export3DOptions::default()
        };
        let rust_lines = line_segment_count(&glb_json(&serialize_glb(&rust, &options).unwrap()));
        let native_lines =
            line_segment_count(&glb_json(&serialize_glb(&native, &options).unwrap()));

        assert_eq!(rust_lines, 24);
        assert_eq!(native_lines, rust_lines);
    }

    #[cfg(feature = "cpp-relation")]
    #[test]
    fn rust_and_native_relations_preserve_source_patch_boundaries() {
        use crate::{render_structured_native_cached, ManifoldKernel};

        let node = Node::Cylinder {
            h: 10.0,
            r1: 4.0,
            r2: 4.0,
            center: false,
            frags: FragmentSpec {
                fn_: 16.0,
                ..FragmentSpec::default()
            },
        };
        let native =
            render_structured_native_cached(&node, &ManifoldKernel::new(), &mut GeomCache::new())
                .unwrap();
        let options = Export3DOptions {
            include_edges: true,
            ..Export3DOptions::default()
        };

        assert_eq!(
            line_segment_count(&glb_json(&serialize_glb(&native, &options).unwrap())),
            32
        );
    }

    fn extruded(child: Node, height: f64) -> Node {
        Node::LinearExtrude {
            height,
            center: false,
            twist: 0.0,
            scale: [1.0, 1.0],
            slices: 1,
            child: Box::new(child),
        }
    }

    fn edged_line_count(node: &Node) -> u64 {
        let options = Export3DOptions {
            include_edges: true,
            ..Export3DOptions::default()
        };
        line_segment_count(&glb_json(
            &serialize_glb(&rendered(node), &options).unwrap(),
        ))
    }

    fn circle(radius: f64) -> Node {
        Node::Circle {
            r: radius,
            frags: FragmentSpec {
                fn_: 48.0,
                ..FragmentSpec::default()
            },
        }
    }

    /// A multi-loop profile does not lay its vertices out ring by ring, so
    /// inferring layers from vertex indices files the side wall into the cap
    /// patches and both rims cancel. Four complete rims is the answer.
    #[test]
    fn extruded_annulus_draws_both_rims() {
        let annulus = Node::Difference(vec![circle(10.0), circle(5.0)]);

        assert_eq!(edged_line_count(&extruded(annulus, 4.0)), 192);
    }

    /// The single-loop case must keep classifying through the ring layout,
    /// which is the only way a ruled side wall stays grouped by profile
    /// segment.
    #[test]
    fn extruded_disc_draws_only_its_rims() {
        assert_eq!(edged_line_count(&extruded(circle(10.0), 4.0)), 96);
    }

    /// A genuine polygon keeps every vertical edge: the merge must not treat a
    /// declared corner as tessellation.
    #[test]
    fn extruded_hexagon_keeps_every_vertical_edge() {
        let hexagon = Node::Circle {
            r: 10.0,
            frags: FragmentSpec {
                fn_: 6.0,
                ..FragmentSpec::default()
            },
        };

        assert_eq!(edged_line_count(&extruded(hexagon, 20.0)), 12);
    }

    /// Two patches that meet coplanar are one surface, so a flush union shows
    /// the outer box and no seam across the shared face.
    #[test]
    fn flush_union_draws_no_seam() {
        let stacked = Node::Union(vec![
            Node::Cube {
                size: [20.0, 20.0, 10.0],
                center: false,
            },
            Node::Translate {
                v: [0.0, 0.0, 10.0],
                child: Box::new(Node::Cube {
                    size: [20.0, 20.0, 10.0],
                    center: false,
                }),
            },
        ]);

        assert_eq!(edged_line_count(&stacked), 16);
    }

    /// A hull face lying wholly on one operand inherits that operand's surface,
    /// so the boundary with anything that later cuts it is a patch pair and is
    /// drawn along its whole length rather than eroding toward tangency.
    #[cfg(feature = "cpp-relation")]
    #[test]
    fn hull_faces_inherit_their_operand_surface() {
        use crate::{render_structured_native_cached, ManifoldKernel};

        let ball = |x: f64, radius: f64| Node::Translate {
            v: [x, 0.0, 0.0],
            child: Box::new(Node::Sphere {
                r: radius,
                frags: FragmentSpec {
                    fn_: 24.0,
                    ..FragmentSpec::default()
                },
            }),
        };
        let node = Node::Hull(vec![ball(-10.0, 6.0), ball(10.0, 3.0)]);
        let structured =
            render_structured_native_cached(&node, &ManifoldKernel::new(), &mut GeomCache::new())
                .unwrap();

        let patches: BTreeSet<_> = structured.exact.source_face_ids.iter().copied().collect();
        assert!(
            !patches.contains(&crate::structured::UNCLASSIFIED_PATCH_ID),
            "every hull face should resolve to an operand or a ruled band"
        );
        assert_eq!(patches.len(), 3, "two operand surfaces and one ruled band");
    }

    /// Every feature segment the exporter would write, in model coordinates,
    /// deduplicated across edge groups (a boundary edge is emitted once per
    /// adjacent group).
    fn feature_segments(structured: &StructuredMesh) -> Vec<[[f64; 3]; 2]> {
        let compiled = compile_glb(structured);
        let mut segments: BTreeMap<[[u64; 3]; 2], [[f64; 3]; 2]> = BTreeMap::new();
        let groups = compiled
            .components
            .iter()
            .flat_map(|component| component.edge_groups.iter())
            .chain(
                compiled
                    .solids
                    .iter()
                    .filter_map(|solid| solid.unowned_edges.as_ref()),
            );
        {
            for group in groups {
                for edge in feature_edges(group.source, &group.selection) {
                    let points = edge.map(|vertex| group.source.exact.mesh.verts[vertex as usize]);
                    let mut key = points.map(|point| point.map(|value| value.to_bits()));
                    key.sort_unstable();
                    segments.insert(key, points);
                }
            }
        }
        segments.into_values().collect()
    }

    fn stroke(from: [f64; 2], to: [f64; 2], diameter: f64, height: f64) -> Node {
        let end = |at: [f64; 2]| Node::Translate {
            v: [at[0], at[1], 0.0],
            child: Box::new(Node::Cylinder {
                h: height,
                r1: diameter / 2.0,
                r2: diameter / 2.0,
                center: false,
                // The plaque's `$fa = 2; $fs = 0.4;` — ~9° wall facets, well
                // under the merge threshold, so the walls must stay smooth.
                frags: FragmentSpec {
                    fa: 2.0,
                    fs: 0.4,
                    ..FragmentSpec::default()
                },
            }),
        };
        Node::Hull(vec![end(from), end(to)])
    }

    /// A letter "A" of three hulled cylinder pairs, raised on a plate: the
    /// model that exposed hull patches being classified by operand alone. Its
    /// top face is the same outline as where it meets the plate, so the rim at
    /// `z = 9` has to close and to measure the same as the loop at `z = 6`.
    #[test]
    fn hulled_letter_draws_a_closed_top_rim_and_no_wall_seams() {
        let letter = Node::Union(vec![
            stroke([-9.0, -13.0], [0.0, 11.0], 5.0, 3.1),
            stroke([9.0, -13.0], [0.0, 11.0], 5.0, 3.1),
            stroke([-5.5, -3.0], [5.5, -3.0], 4.0, 3.1),
        ]);
        let structured = rendered(&Node::Union(vec![
            Node::Translate {
                v: [-20.0, -20.0, 0.0],
                child: Box::new(Node::Cube {
                    size: [40.0, 40.0, 6.0],
                    center: false,
                }),
            },
            Node::Translate {
                v: [0.0, 0.0, 5.9],
                child: Box::new(letter),
            },
        ]));
        let segments = feature_segments(&structured);

        let on_plate_wall = |segment: &[[f64; 3]; 2]| {
            segment.iter().all(|point| {
                (point[0].abs() - 20.0).abs() < 1e-6 || (point[1].abs() - 20.0).abs() < 1e-6
            })
        };
        let in_plane = |segment: &[[f64; 3]; 2], z: f64| {
            segment.iter().all(|point| (point[2] - z).abs() < 1e-6)
        };
        let length = |segment: &[[f64; 3]; 2]| -> f64 {
            (0..3)
                .map(|axis| (segment[1][axis] - segment[0][axis]).powi(2))
                .sum::<f64>()
                .sqrt()
        };
        let total = |group: &[&[[f64; 3]; 2]]| group.iter().copied().map(length).sum::<f64>();

        let base: Vec<_> = segments
            .iter()
            .filter(|segment| in_plane(segment, 6.0) && !on_plate_wall(segment))
            .collect();
        let rim: Vec<_> = segments
            .iter()
            .filter(|segment| in_plane(segment, 9.0))
            .collect();

        // Every rim vertex meets exactly two rim segments: closed loops, and no
        // chord cutting across a rounded stroke cap (a chord would land two
        // extra segments on a pair of existing vertices).
        let mut degrees: BTreeMap<[u64; 3], usize> = BTreeMap::new();
        for segment in &rim {
            for point in segment.iter() {
                *degrees
                    .entry(point.map(|value| canonical_zero(value).to_bits()))
                    .or_default() += 1;
            }
        }
        assert!(
            degrees.values().all(|degree| *degree == 2),
            "top rim is not a set of closed loops: {:?}",
            degrees.values().filter(|degree| **degree != 2).count()
        );
        let (base_length, rim_length) = (total(&base), total(&rim));
        assert!(
            (rim_length - base_length).abs() / base_length < 0.02,
            "top rim {rim_length} mm should match the base outline {base_length} mm"
        );

        // The only creases running up a stroke wall are where the crossbar
        // meets each leg (four) and where the two legs cross at the letter's
        // inner notch (one). Tangent seams and the arc the two legs share over
        // the apex cylinder are smooth and must not be drawn.
        let mut junctions: Vec<i64> = segments
            .iter()
            .filter(|segment| {
                (segment[0][2] - segment[1][2]).abs() > 1e-6 && !on_plate_wall(segment)
            })
            .map(|segment| (segment[0][0] * 1e3).round() as i64)
            .collect();
        junctions.sort_unstable();
        assert_eq!(junctions, [-3333, -1833, 0, 1833, 3333]);
    }

    /// Feature edges have to survive whichever Manifold implementation the
    /// build links — `manifold-rust` on Wasm, `manifold-csg` natively —
    /// otherwise a model exported from Tau draws lines the native CLI never
    /// shows. The two hull *differently* (different triangle counts, and the
    /// cap disks get chopped into a different number of patch islands), so the
    /// patch census cannot match; the segments they draw must.
    #[cfg(feature = "cpp-relation")]
    #[test]
    fn both_relation_kernels_draw_the_same_hull_feature_edges() {
        use crate::{render_structured_native_cached, ManifoldKernel};

        let node = Node::Union(vec![
            stroke([-9.0, -13.0], [0.0, 11.0], 5.0, 4.0),
            stroke([9.0, -13.0], [0.0, 11.0], 5.0, 4.0),
        ]);
        // Micrometres: the two kernels agree on the hull's vertices to about a
        // nanometre, and quantising absorbs that without hiding a real
        // difference (the shortest segment here is a 0.4 mm wall facet).
        let canonical = |structured: &StructuredMesh| {
            let mut segments: Vec<[[i64; 3]; 2]> = feature_segments(structured)
                .into_iter()
                .map(|segment| {
                    let mut points =
                        segment.map(|point| point.map(|value| (value * 1e3).round() as i64));
                    points.sort_unstable();
                    points
                })
                .collect();
            segments.sort_unstable();
            segments
        };
        let native =
            render_structured_native_cached(&node, &ManifoldKernel::new(), &mut GeomCache::new())
                .unwrap();

        let (rust_edges, native_edges) = (canonical(&rendered(&node)), canonical(&native));
        assert!(!rust_edges.is_empty());
        assert_eq!(rust_edges.len(), native_edges.len());
        assert_eq!(rust_edges, native_edges);
    }

    #[test]
    fn one_authored_module_with_two_colors_emits_one_mesh_with_two_primitives() {
        let node = authored("painted_part", 10, two_colour_fused());
        let document =
            glb_json(&serialize_glb(&rendered(&node), &Export3DOptions::default()).unwrap());

        assert_eq!(document["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(document["nodes"][0]["name"], "Painted Part");
        assert!(document["nodes"][0].get("mesh").is_none());
        assert_eq!(document["nodes"][0]["children"], json!([1]));
        assert_eq!(document["nodes"][1]["name"], "Painted Part");
        assert_eq!(document["meshes"].as_array().unwrap().len(), 1);
        assert_eq!(
            primitive_owners(&document, &document["nodes"][1]),
            [(json!(0), 18), (json!(0), 18)]
        );
        let primitives = document["meshes"][0]["primitives"].as_array().unwrap();
        assert_ne!(primitives[0]["material"], primitives[1]["material"]);
    }

    #[test]
    fn authored_parent_can_own_direct_geometry_and_children_without_duplicate_triangles() {
        let node = authored(
            "assembly",
            10,
            Node::Union(vec![
                Node::Cube {
                    size: [1.0, 1.0, 1.0],
                    center: false,
                },
                authored(
                    "cap",
                    20,
                    Node::Translate {
                        v: [0.0, 0.0, 1.0],
                        child: Box::new(Node::Cube {
                            size: [1.0, 1.0, 1.0],
                            center: false,
                        }),
                    },
                ),
            ]),
        );
        let structured = rendered(&node);
        let document = glb_json(&serialize_glb(&structured, &Export3DOptions::default()).unwrap());

        assert_eq!(document["nodes"][0]["name"], "Assembly");
        assert!(document["nodes"][0].get("mesh").is_none());
        assert_eq!(document["nodes"][0]["children"], json!([1, 2]));
        assert_eq!(document["nodes"][1]["name"], "Cap");
        assert!(document["nodes"][1].get("mesh").is_none());
        let solid = &document["nodes"][2];
        assert_eq!(solid["name"], "Assembly Solid");
        assert_eq!(
            primitive_owners(&document, solid),
            [(json!(0), 10), (json!(1), 10)]
        );
        assert_eq!(
            surface_triangle_count(&document),
            structured.exact.mesh.tris.len() as u64
        );
    }

    #[test]
    fn ownerless_geometry_uses_deterministic_spatial_fallback_names() {
        let document = glb_json(&serialize_glb(&two_cubes(), &Export3DOptions::default()).unwrap());

        assert_eq!(document["scenes"][0]["nodes"], json!([0, 1]));
        assert_eq!(document["nodes"][0]["name"], "#F5A523FF Shape 1");
        assert_eq!(document["nodes"][1]["name"], "#F5A523FF Shape 2");
        assert!(document["nodes"][0]["extras"]["openrscad"]["moduleName"].is_null());
        assert_eq!(
            document["nodes"][0]["extras"]["openrscad"]["fallback"],
            true
        );
        assert_eq!(
            document["nodes"][0]["extras"]["openrscad"]["owners"],
            json!([])
        );
        // A scene with no modules and one material keeps one node and one
        // single-primitive mesh per solid.
        assert_eq!(document["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(document["meshes"].as_array().unwrap().len(), 2);
        assert_eq!(
            primitive_owners(&document, &document["nodes"][0]),
            [(Value::Null, 12)]
        );
    }

    #[test]
    fn duplicate_authored_siblings_are_source_ordered_while_same_call_site_collapses() {
        let cube = || Node::Cube {
            size: [1.0, 1.0, 1.0],
            center: false,
        };
        let node = authored(
            "assembly",
            10,
            Node::Union(vec![
                authored("bench", 30, cube()),
                authored(
                    "bench",
                    20,
                    Node::Translate {
                        v: [0.0, 0.0, 2.0],
                        child: Box::new(cube()),
                    },
                ),
                authored(
                    "arch_loop",
                    40,
                    Node::Translate {
                        v: [0.0, 0.0, 4.0],
                        child: Box::new(cube()),
                    },
                ),
                authored(
                    "arch_loop",
                    40,
                    Node::Translate {
                        v: [0.0, 0.0, 6.0],
                        child: Box::new(cube()),
                    },
                ),
            ]),
        );
        let document =
            glb_json(&serialize_glb(&rendered(&node), &Export3DOptions::default()).unwrap());
        let children = document["nodes"][0]["children"].as_array().unwrap();
        let names: Vec<_> = children
            .iter()
            .map(|child| {
                document["nodes"][child.as_u64().unwrap() as usize]["name"]
                    .as_str()
                    .unwrap()
            })
            .collect();

        assert_eq!(names, ["Bench 1", "Bench 2", "Arch Loop"]);
        assert_eq!(children.len(), 3, "the four cubes are separate solids");
        // The loop instances share one call site, so one node owns both
        // solids, each its own leaf.
        let arch = children[2].as_u64().unwrap() as usize;
        assert!(document["nodes"][arch].get("mesh").is_none());
        let solids = document["nodes"][arch]["children"].as_array().unwrap();
        assert_eq!(solids.len(), 2);
        for solid in solids {
            let solid = &document["nodes"][solid.as_u64().unwrap() as usize];
            assert_eq!(primitive_owners(&document, solid), [(json!(arch), 12)]);
        }
    }

    #[test]
    fn threemf_names_unchanged_physical_solids_from_authored_provenance() {
        let roof = authored(
            "roof_frame",
            20,
            Node::Union(vec![
                Node::Cube {
                    size: [1.0, 1.0, 1.0],
                    center: false,
                },
                Node::Translate {
                    v: [0.0, 0.0, 2.0],
                    child: Box::new(Node::Cube {
                        size: [1.0, 1.0, 1.0],
                        center: false,
                    }),
                },
            ]),
        );
        let structured = rendered(&roof);
        assert_eq!(structured.solid_components.len(), 2);
        let bytes = serialize_3mf(&structured, &Export3DOptions::default()).unwrap();
        let model_xml = threemf_document(&bytes);
        let model = roxmltree::Document::parse(&model_xml).unwrap();

        assert_eq!(
            threemf_object_names(&model),
            ["Roof Frame 1", "Roof Frame 2"]
        );
        assert_eq!(
            model.root_element().lookup_namespace_uri(Some("openrscad")),
            Some("https://openrscad.com/3mf/metadata/1")
        );
        for object in model
            .descendants()
            .filter(|node| node.has_tag_name("object"))
        {
            let metadata = object
                .descendants()
                .find(|node| {
                    node.has_tag_name("metadata")
                        && node.attribute("name") == Some("openrscad:semanticOwners")
                })
                .unwrap();
            let owners: Value = serde_json::from_str(metadata.text().unwrap()).unwrap();
            assert_eq!(owners[0][0]["moduleName"], "roof_frame");
            assert_eq!(owners[0][0]["callSite"]["start"], 20);
        }
    }

    #[test]
    fn threemf_fused_sibling_modules_use_their_common_authored_owner() {
        let node = authored(
            "assembly",
            10,
            Node::Union(vec![
                authored(
                    "lower_frame",
                    20,
                    Node::Cube {
                        size: [10.0, 10.0, 2.0],
                        center: false,
                    },
                ),
                authored(
                    "roof_frame",
                    30,
                    Node::Translate {
                        v: [0.0, 0.0, 2.0],
                        child: Box::new(Node::Cube {
                            size: [10.0, 10.0, 2.0],
                            center: false,
                        }),
                    },
                ),
            ]),
        );
        let structured = rendered(&node);
        assert_eq!(structured.solid_components.len(), 1);
        let bytes = serialize_3mf(&structured, &Export3DOptions::default()).unwrap();
        let model_xml = threemf_document(&bytes);
        let model = roxmltree::Document::parse(&model_xml).unwrap();

        assert_eq!(threemf_object_names(&model), ["Assembly Solid"]);
    }

    #[test]
    fn threemf_fused_roots_without_common_owner_use_shape_fallback() {
        let node = Node::Union(vec![
            authored(
                "left_part",
                10,
                Node::Cube {
                    size: [1.0, 1.0, 1.0],
                    center: false,
                },
            ),
            authored(
                "right_part",
                20,
                Node::Translate {
                    v: [0.0, 0.0, 1.0],
                    child: Box::new(Node::Cube {
                        size: [1.0, 1.0, 1.0],
                        center: false,
                    }),
                },
            ),
        ]);
        let structured = rendered(&node);
        assert_eq!(structured.solid_components.len(), 1);
        let model_xml =
            threemf_document(&serialize_3mf(&structured, &Export3DOptions::default()).unwrap());
        let model = roxmltree::Document::parse(&model_xml).unwrap();

        assert_eq!(threemf_object_names(&model), ["Shape"]);
        let owners = model
            .descendants()
            .find(|node| node.attribute("name") == Some("openrscad:semanticOwners"))
            .unwrap();
        let owners: Value = serde_json::from_str(owners.text().unwrap()).unwrap();
        assert_eq!(owners.as_array().unwrap().len(), 2);
    }

    #[test]
    fn structured_3mf_has_one_object_per_spatial_solid() {
        let structured = two_cubes();
        let bytes = serialize_3mf(&structured, &Export3DOptions::default()).unwrap();
        let model = crate::mesh::read_3mf_model(&bytes).unwrap();
        assert_eq!(model.matches("<object ").count(), 2);
        assert_eq!(model.matches("<item ").count(), 2);
        assert_eq!(
            serialize_3mf(&structured, &Export3DOptions::default()).unwrap(),
            bytes
        );
    }

    #[test]
    fn invalid_glb_scale_is_typed_error() {
        let options = Export3DOptions {
            source_unit_to_meters: 0.0,
            ..Export3DOptions::default()
        };
        assert!(serialize_glb(&two_cubes(), &options).is_err());
    }

    #[test]
    fn optional_edges_are_owner_local_deterministic_and_do_no_disabled_work() {
        let structured = two_cubes();
        reset_edge_derivation_count();
        let plain = serialize_glb(&structured, &Export3DOptions::default()).unwrap();
        assert_eq!(edge_derivation_count(), 0);
        let plain = glb_json(&plain);
        assert_eq!(plain["nodes"].as_array().unwrap().len(), 2);
        assert!(plain["meshes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|mesh| mesh["primitives"].as_array().unwrap().len() == 1));

        let options = Export3DOptions {
            include_edges: true,
            ..Export3DOptions::default()
        };
        let edged = serialize_glb(&structured, &options).unwrap();
        assert_eq!(edge_derivation_count(), 2);
        assert_eq!(serialize_glb(&structured, &options).unwrap(), edged);
        let edged = glb_json(&edged);
        // No module owns these solids, so each solid's lines sit on a
        // `LINES`-only child: a manifold mesh holds triangles only.
        assert_eq!(edged["scenes"][0]["nodes"], json!([0, 1]));
        assert_eq!(edged["nodes"].as_array().unwrap().len(), 4);
        for (solid, edges) in [(0, 2), (1, 3)] {
            assert_eq!(edged["nodes"][solid]["children"], json!([edges]));
            assert_eq!(
                edged["nodes"][edges]["name"],
                format!("{} Edges", edged["nodes"][solid]["name"].as_str().unwrap())
            );
            let mesh = &edged["meshes"][edged["nodes"][solid]["mesh"].as_u64().unwrap() as usize];
            let primitives = mesh["primitives"].as_array().unwrap();
            assert_eq!(primitives.len(), 1);
            assert_eq!(primitives[0]["mode"], 4);
            let mesh = &edged["meshes"][edged["nodes"][edges]["mesh"].as_u64().unwrap() as usize];
            let primitives = mesh["primitives"].as_array().unwrap();
            assert_eq!(primitives.len(), 1);
            assert_eq!(primitives[0]["mode"], 1);
            let line_accessor = primitives[0]["indices"].as_u64().unwrap() as usize;
            assert_eq!(edged["accessors"][line_accessor]["count"], 24);
        }
    }

    #[test]
    fn multicolor_owner_derives_one_edge_group_after_all_surface_primitives() {
        let node = authored("painted_part", 10, two_colour_fused());
        reset_edge_derivation_count();
        let document = glb_json(
            &serialize_glb(
                &rendered(&node),
                &Export3DOptions {
                    include_edges: true,
                    ..Export3DOptions::default()
                },
            )
            .unwrap(),
        );

        assert_eq!(edge_derivation_count(), 1);
        assert_eq!(document["nodes"].as_array().unwrap().len(), 2);
        // The owner's lines are a `LINES`-only mesh on the module node; the
        // solid's mesh keeps its two surface primitives.
        let owner = document["nodes"][0]["mesh"].as_u64().unwrap() as usize;
        let lines = document["meshes"][owner]["primitives"].as_array().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0]["mode"], 1);
        let solid = document["nodes"][1]["mesh"].as_u64().unwrap() as usize;
        let surfaces = document["meshes"][solid]["primitives"].as_array().unwrap();
        assert_eq!(surfaces.len(), 2);
        assert_eq!(surfaces[0]["mode"], 4);
        assert_eq!(surfaces[1]["mode"], 4);
    }

    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        hash
    }

    /// The export path must be self-consistent inside one process: the same
    /// scene exported twenty times in a row has to produce the same bytes.
    ///
    /// It did not before every order-affecting `std::collections::HashMap` /
    /// `HashSet` in this crate was replaced by a deterministic hasher (and the
    /// hull horizon by a `BTreeSet`): `RandomState` reseeds per map instance,
    /// so iteration order depended on how many maps the thread had already
    /// built and the twentieth export differed from the first. That broke
    /// artifact hashing, every build cache keyed on the artifact, and any
    /// native/wasm byte-parity gate — the reference was not stable against
    /// itself.
    #[test]
    fn repeated_export_in_one_process_is_byte_identical() {
        // A `tau-plaque`-shaped scene: a rounded slab built by hulling four
        // cylinders (the convex-hull horizon is the one hash container in this
        // crate whose iteration reaches the output), unioned with an extruded
        // rim and drilled by counterbored holes, under one colour so the whole
        // thing is one attributed surface.
        let frags = FragmentSpec {
            fn_: 0.0,
            fa: 12.0,
            fs: 2.0,
        };
        let pillar = |x: f64, y: f64| Node::Translate {
            v: [x, y, 0.0],
            child: Box::new(Node::Cylinder {
                h: 6.0,
                r1: 6.0,
                r2: 6.0,
                center: false,
                frags,
            }),
        };
        let hole = |x: f64, y: f64| Node::Translate {
            v: [x, y, -1.0],
            child: Box::new(Node::Cylinder {
                h: 12.0,
                r1: 2.0,
                r2: 2.0,
                center: false,
                frags,
            }),
        };
        let node = Node::Color {
            rgba: [0.184, 0.208, 0.259, 1.0],
            child: Box::new(Node::Difference(vec![
                Node::Union(vec![
                    Node::Hull(vec![
                        pillar(0.0, 0.0),
                        pillar(60.0, 0.0),
                        pillar(0.0, 30.0),
                        pillar(60.0, 30.0),
                    ]),
                    Node::Translate {
                        v: [30.0, 15.0, 5.9],
                        child: Box::new(Node::LinearExtrude {
                            height: 2.0,
                            center: false,
                            twist: 0.0,
                            scale: [1.0, 1.0],
                            slices: 1,
                            child: Box::new(Node::Circle { r: 8.0, frags }),
                        }),
                    },
                ]),
                hole(6.0, 6.0),
                hole(54.0, 6.0),
                hole(6.0, 24.0),
                hole(54.0, 24.0),
            ])),
        };
        let options = Export3DOptions::default();
        let mut hashes = std::collections::BTreeSet::new();
        for _ in 0..20 {
            let structured = render_structured_rust_cached(
                &node,
                &RustManifoldKernel::new(),
                &mut GeomCache::new(),
            )
            .unwrap();
            hashes.insert(fnv1a(&serialize_glb(&structured, &options).unwrap()));
            let structured = render_structured_rust_cached(
                &node,
                &RustManifoldKernel::new(),
                &mut GeomCache::new(),
            )
            .unwrap();
            hashes.insert(fnv1a(&serialize_3mf(&structured, &options).unwrap()));
        }
        assert_eq!(
            hashes.len(),
            2,
            "twenty in-process exports produced {} distinct artifacts (expected one GLB + one 3MF)",
            hashes.len()
        );
    }

    /// `srgb_to_linear` feeds every GLB material, so a one-ULP difference in
    /// `powf` makes 100% of GLBs differ between two builds — which is exactly
    /// what `f32::powf` did (Apple's libm natively, Rust's bundled one on
    /// wasm32). It now goes through the `libm` crate on both targets; this
    /// freezes all 256 outputs so the curve cannot drift silently.
    #[test]
    fn srgb_to_linear_curve_is_frozen() {
        let bits: Vec<u8> = (0..=255u8)
            .flat_map(|value| srgb_to_linear(value).to_bits().to_le_bytes())
            .collect();
        assert_eq!(srgb_to_linear(0), 0.0);
        assert_eq!(srgb_to_linear(255), 1.0);
        // 10 and 11 straddle the linear/gamma branch at 0.04045.
        assert_eq!(srgb_to_linear(10).to_bits(), 0x3b46_eb61);
        assert_eq!(srgb_to_linear(11).to_bits(), 0x3b5b_518d);
        assert_eq!(srgb_to_linear(128).to_bits(), 0x3e5d_0a8b);
        assert_eq!(fnv1a(&bits), 0x1a45_ac26_a93d_8502, "the sRGB curve moved");
    }

    /// Reads one accessor's elements as little-endian 32-bit words.
    fn accessor_words(document: &Value, binary: &[u8], accessor: usize, width: usize) -> Vec<u32> {
        let accessor = &document["accessors"][accessor];
        let view = &document["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
        let start = view["byteOffset"].as_u64().unwrap_or(0) as usize
            + accessor["byteOffset"].as_u64().unwrap_or(0) as usize;
        let count = accessor["count"].as_u64().unwrap() as usize * width;
        (0..count)
            .map(|index| {
                let at = start + index * 4;
                u32::from_le_bytes(binary[at..at + 4].try_into().unwrap())
            })
            .collect()
    }

    /// Every rendered triangle as its three `f32` position bit patterns,
    /// rotated to start at its smallest corner (winding kept) and sorted, so two
    /// exports compare equal exactly when they carry the same triangles,
    /// whatever their node, mesh and primitive grouping.
    fn aggregate_triangle_set(bytes: &[u8]) -> Vec<[[u32; 3]; 3]> {
        let document = glb_json(bytes);
        let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let binary = &bytes[(20 + json_length + 8).min(bytes.len())..];
        let mut triangles = Vec::new();
        for mesh in document["meshes"].as_array().into_iter().flatten() {
            for primitive in mesh["primitives"].as_array().unwrap() {
                if primitive["mode"] != 4 {
                    continue;
                }
                let positions = accessor_words(
                    &document,
                    binary,
                    primitive["attributes"]["POSITION"].as_u64().unwrap() as usize,
                    3,
                );
                let indices = accessor_words(
                    &document,
                    binary,
                    primitive["indices"].as_u64().unwrap() as usize,
                    1,
                );
                for corners in indices.chunks(3) {
                    let mut triangle = [[0u32; 3]; 3];
                    for (slot, index) in corners.iter().enumerate() {
                        let at = *index as usize * 3;
                        triangle[slot] = [positions[at], positions[at + 1], positions[at + 2]];
                    }
                    let first = (1..3).fold(0, |best, next| {
                        if triangle[next] < triangle[best] {
                            next
                        } else {
                            best
                        }
                    });
                    triangle.rotate_left(first);
                    triangles.push(triangle);
                }
            }
        }
        triangles.sort_unstable();
        triangles
    }

    fn triangle_set_hash(bytes: &[u8]) -> u64 {
        let words: Vec<u8> = aggregate_triangle_set(bytes)
            .iter()
            .flatten()
            .flatten()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        fnv1a(&words)
    }

    fn unit_cube_at(v: [f64; 3]) -> Node {
        Node::Translate {
            v,
            child: Box::new(Node::Cube {
                size: [1.0, 1.0, 1.0],
                center: false,
            }),
        }
    }

    fn painted(rgba: [f32; 4], child: Node) -> Node {
        Node::Color {
            rgba,
            child: Box::new(child),
        }
    }

    /// `modules.scad` in miniature: a module that bores a hole through a block
    /// another module builds, so one solid has two authored owners.
    fn bored_block() -> Node {
        authored(
            "chamfered_cube_cutout",
            10,
            Node::Difference(vec![
                authored(
                    "chamfered_block",
                    40,
                    Node::Cube {
                        size: [10.0, 10.0, 10.0],
                        center: true,
                    },
                ),
                Node::Cylinder {
                    h: 12.0,
                    r1: 3.0,
                    r2: 3.0,
                    center: true,
                    frags: FragmentSpec {
                        fn_: 24.0,
                        ..FragmentSpec::default()
                    },
                },
            ]),
        )
    }

    /// `union.scad`: two sibling modules fused by a top-level union.
    fn sibling_union() -> Node {
        Node::Union(vec![
            authored(
                "left_block",
                10,
                Node::Cube {
                    size: [10.0, 10.0, 10.0],
                    center: true,
                },
            ),
            authored(
                "right_block",
                30,
                Node::Translate {
                    v: [6.0, 0.0, 0.0],
                    child: Box::new(Node::Cube {
                        size: [10.0, 10.0, 10.0],
                        center: true,
                    }),
                },
            ),
        ])
    }

    /// `twosolids.scad`: one module producing two disjoint solids.
    fn pair() -> Node {
        authored(
            "pair",
            10,
            Node::Union(vec![
                unit_cube_at([0.0, 0.0, 0.0]),
                unit_cube_at([10.0, 0.0, 0.0]),
            ]),
        )
    }

    /// Two colours fused into one solid with no authored owner.
    fn two_colour_fused() -> Node {
        Node::Union(vec![
            painted([1.0, 0.0, 0.0, 1.0], unit_cube_at([0.0, 0.0, 0.0])),
            painted([0.0, 0.0, 1.0, 1.0], unit_cube_at([0.5, 0.5, 0.5])),
        ])
    }

    fn layout_scenes() -> Vec<(&'static str, Node)> {
        vec![
            (
                "two_cubes",
                Node::Union(vec![
                    unit_cube_at([0.0, 0.0, 0.0]),
                    unit_cube_at([0.0, 0.0, 2.0]),
                ]),
            ),
            ("bored_block", bored_block()),
            ("sibling_union", sibling_union()),
            ("pair", pair()),
            ("two_colour_fused", two_colour_fused()),
            (
                "painted_part",
                authored(
                    "painted_part",
                    10,
                    Node::Union(vec![
                        painted([1.0, 0.0, 0.0, 1.0], unit_cube_at([0.0, 0.0, 0.0])),
                        painted([0.0, 0.0, 1.0, 1.0], unit_cube_at([0.0, 0.0, 2.0])),
                    ]),
                ),
            ),
            (
                "assembly_with_cap",
                authored(
                    "assembly",
                    10,
                    Node::Union(vec![
                        unit_cube_at([0.0, 0.0, 0.0]),
                        authored("cap", 20, unit_cube_at([0.0, 0.0, 1.0])),
                    ]),
                ),
            ),
        ]
    }

    /// Decodes every `EXT_mesh_manifold` declaration and checks it against the
    /// extension: `TRIANGLES` only, one shared `POSITION` (and `NORMAL`), every
    /// index accessor on one buffer view, a manifold stream whose dense part is
    /// the render stream, merges (equal to the sparse substitutions) only
    /// between bitwise-identical positions, and an oriented 2-manifold result.
    /// Returns how many meshes declared it.
    fn assert_manifold_meshes(bytes: &[u8]) -> usize {
        let document = glb_json(bytes);
        let json_length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let binary = &bytes[(20 + json_length + 8).min(bytes.len())..];
        let accessor = |index: &Value| &document["accessors"][index.as_u64().unwrap() as usize];
        let words = |index: &Value, width: usize| {
            accessor_words(&document, binary, index.as_u64().unwrap() as usize, width)
        };
        let mut declared = 0;
        for mesh in document["meshes"].as_array().into_iter().flatten() {
            let Some(extension) = mesh["extensions"].get("EXT_mesh_manifold") else {
                continue;
            };
            declared += 1;
            let primitives = mesh["primitives"].as_array().unwrap();
            let attributes = &primitives[0]["attributes"];
            let index_view = &accessor(&primitives[0]["indices"])["bufferView"];
            let mut render = Vec::new();
            for primitive in primitives {
                assert_eq!(primitive["mode"], 4);
                assert_eq!(&primitive["attributes"], attributes);
                assert_eq!(&accessor(&primitive["indices"])["bufferView"], index_view);
                render.extend(words(&primitive["indices"], 1));
            }
            let manifold = &extension["manifoldPrimitive"];
            assert_eq!(manifold["mode"], 4);
            assert_eq!(
                manifold["attributes"],
                json!({ "POSITION": attributes["POSITION"] })
            );
            assert!(manifold.get("material").is_none());
            let manifold_accessor = accessor(&manifold["indices"]);
            assert_eq!(&manifold_accessor["bufferView"], index_view);
            let mut indices = words(&manifold["indices"], 1);
            assert_eq!(indices, render);
            let positions = words(&attributes["POSITION"], 3);
            if let Some(sparse) = manifold_accessor.get("sparse") {
                let merge_indices = words(&extension["mergeIndices"], 1);
                let merge_values = words(&extension["mergeValues"], 1);
                let merge_index_accessor = accessor(&extension["mergeIndices"]);
                let merge_value_accessor = accessor(&extension["mergeValues"]);
                assert_eq!(sparse["count"], json!(merge_indices.len()));
                assert_eq!(merge_values.len(), merge_indices.len());
                assert_eq!(
                    sparse["indices"]["bufferView"],
                    merge_index_accessor["bufferView"]
                );
                assert_eq!(
                    sparse["indices"]["byteOffset"].as_u64().unwrap_or(0),
                    merge_index_accessor["byteOffset"].as_u64().unwrap_or(0)
                );
                assert_eq!(
                    sparse["values"]["bufferView"],
                    merge_value_accessor["bufferView"]
                );
                assert_eq!(
                    sparse["values"]["byteOffset"].as_u64().unwrap_or(0),
                    merge_value_accessor["byteOffset"].as_u64().unwrap_or(0)
                );
                assert!(merge_indices.windows(2).all(|pair| pair[0] < pair[1]));
                for (index, value) in merge_indices.iter().zip(&merge_values) {
                    let (from, to) = (indices[*index as usize] as usize, *value as usize);
                    assert_eq!(
                        positions[from * 3..from * 3 + 3],
                        positions[to * 3..to * 3 + 3]
                    );
                    indices[*index as usize] = *value;
                }
            } else {
                assert!(extension.get("mergeIndices").is_none());
            }
            let manifold_mesh = Mesh {
                verts: positions
                    .chunks(3)
                    .map(|point| point.iter().map(|bits| f64::from(f32::from_bits(*bits))))
                    .map(|mut point| {
                        [
                            point.next().unwrap(),
                            point.next().unwrap(),
                            point.next().unwrap(),
                        ]
                    })
                    .collect(),
                tris: indices
                    .chunks(3)
                    .map(|triangle| [triangle[0], triangle[1], triangle[2]])
                    .collect(),
            };
            let everything = MeshSelection {
                triangles: (0..manifold_mesh.tris.len() as u32).collect(),
                bounds: Default::default(),
                provenance: Vec::new(),
                attribution: AttributionStatus::Exact,
                geometry_hash: 0,
            };
            validate_closed_component("manifold stream", &manifold_mesh, &everything).unwrap();
            validate_vertex_links(&manifold_mesh, &everything).unwrap();
        }
        declared
    }

    /// `(owner, triangle count)` for each primitive of a node's mesh, in order.
    fn primitive_owners(document: &Value, node: &Value) -> Vec<(Value, u64)> {
        let mesh = node["mesh"].as_u64().unwrap() as usize;
        document["meshes"][mesh]["primitives"]
            .as_array()
            .unwrap()
            .iter()
            .map(|primitive| {
                let accessor = primitive["indices"].as_u64().unwrap() as usize;
                (
                    primitive["extras"]["openrscad"]["owner"].clone(),
                    document["accessors"][accessor]["count"].as_u64().unwrap() / 3,
                )
            })
            .collect()
    }

    fn declares_manifold(document: &Value, node: &Value) -> bool {
        let mesh = node["mesh"].as_u64().unwrap() as usize;
        document["meshes"][mesh]["extensions"]
            .get("EXT_mesh_manifold")
            .is_some()
    }

    /// Only grouping, structure and metadata changed: every scene renders
    /// exactly the triangles (positions and winding, bit for bit) that the
    /// previous module-grouped exporter wrote. The hashes were taken from that
    /// exporter (`cd59bec`) with this same reader.
    #[test]
    fn glb_aggregate_triangle_set_is_identical_to_the_previous_grouping() {
        let previous: BTreeMap<&str, (u64, usize)> = BTreeMap::from([
            ("two_cubes", (0xb6ea_cdb6_c7d0_aaa9, 24)),
            ("bored_block", (0x920e_75a5_ba66_5b85, 112)),
            ("sibling_union", (0x017f_f69f_214e_39a3, 28)),
            ("pair", (0xf9bb_3acc_6d27_8ad1, 24)),
            ("two_colour_fused", (0x58fd_1688_15d2_a4dd, 36)),
            ("painted_part", (0xb6ea_cdb6_c7d0_aaa9, 24)),
            ("assembly_with_cap", (0xc7d0_ab5a_0813_6c65, 20)),
        ]);
        for (name, node) in layout_scenes() {
            let structured = rendered(&node);
            for include_edges in [false, true] {
                let options = Export3DOptions {
                    include_edges,
                    ..Export3DOptions::default()
                };
                let bytes = serialize_glb(&structured, &options).unwrap();
                assert_eq!(
                    (
                        triangle_set_hash(&bytes),
                        aggregate_triangle_set(&bytes).len()
                    ),
                    previous[name],
                    "{name} (edges: {include_edges})"
                );
            }
        }
    }

    #[test]
    fn every_solid_mesh_declares_an_oriented_two_manifold_stream() {
        for (name, node) in layout_scenes() {
            let structured = rendered(&node);
            for include_edges in [false, true] {
                let options = Export3DOptions {
                    include_edges,
                    ..Export3DOptions::default()
                };
                let (bytes, diagnostics) =
                    serialize_glb_with_diagnostics(&structured, &options).unwrap();
                assert!(diagnostics.is_empty(), "{name}: {diagnostics:?}");
                let document = glb_json(&bytes);
                assert_eq!(
                    assert_manifold_meshes(&bytes),
                    structured.solid_components.len(),
                    "{name}"
                );
                assert_eq!(document["extensionsUsed"][0], "EXT_mesh_manifold");
                // Lines never share a manifold mesh.
                for mesh in document["meshes"].as_array().unwrap() {
                    let modes: BTreeSet<_> = mesh["primitives"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|primitive| primitive["mode"].as_u64().unwrap())
                        .collect();
                    assert_eq!(modes.len(), 1, "{name}: mixed modes in {}", mesh["name"]);
                    assert_eq!(
                        mesh["extensions"].get("EXT_mesh_manifold").is_some(),
                        modes.contains(&4),
                        "{name}"
                    );
                }
            }
        }
    }

    /// `modules.scad`: the module that bores the block and the module that
    /// builds it share one solid, so the solid hangs under the outer module and
    /// its primitives say which module owns which faces.
    #[test]
    fn module_cutout_emits_one_solid_under_its_outer_module() {
        let document = glb_json(
            &serialize_glb(&rendered(&bored_block()), &Export3DOptions::default()).unwrap(),
        );

        assert_eq!(document["scenes"][0]["nodes"], json!([0]));
        assert_eq!(document["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(document["nodes"][0]["name"], "Chamfered Cube Cutout");
        assert_eq!(document["nodes"][0]["children"], json!([1, 2]));
        assert!(document["nodes"][0].get("mesh").is_none());
        assert_eq!(
            document["nodes"][0]["extras"]["openrscad"]["moduleName"],
            "chamfered_cube_cutout"
        );
        assert_eq!(document["nodes"][1]["name"], "Chamfered Block");
        assert!(document["nodes"][1].get("mesh").is_none());
        assert_eq!(
            document["nodes"][1]["extras"]["openrscad"]["moduleName"],
            "chamfered_block"
        );
        let solid = &document["nodes"][2];
        assert_eq!(solid["name"], "Chamfered Cube Cutout Solid");
        assert_eq!(document["meshes"].as_array().unwrap().len(), 1);
        assert_eq!(
            primitive_owners(&document, solid),
            [(json!(0), 48), (json!(1), 64)]
        );
        assert!(declares_manifold(&document, solid));
        assert_eq!(solid["extras"]["openrscad"]["owners"], json!([0, 1]));
    }

    /// `union.scad`: sibling modules fused at the top level have no common
    /// owner, so their one solid is a scene root named `Shape`.
    #[test]
    fn sibling_union_is_one_root_shape_with_two_owners() {
        let document = glb_json(
            &serialize_glb(&rendered(&sibling_union()), &Export3DOptions::default()).unwrap(),
        );
        let names: Vec<_> = document["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["name"].as_str().unwrap())
            .collect();

        assert_eq!(names, ["Left Block", "Right Block", "Shape"]);
        assert_eq!(document["scenes"][0]["nodes"], json!([0, 1, 2]));
        assert!(document["nodes"][0].get("mesh").is_none());
        assert!(document["nodes"][1].get("mesh").is_none());
        assert_eq!(
            primitive_owners(&document, &document["nodes"][2]),
            [(json!(0), 14), (json!(1), 14)]
        );
        assert!(declares_manifold(&document, &document["nodes"][2]));
    }

    /// `twosolids.scad`: one module that makes two disjoint solids gets one
    /// leaf node and mesh per solid.
    #[test]
    fn one_module_with_two_disjoint_solids_emits_two_leaf_solids() {
        let document =
            glb_json(&serialize_glb(&rendered(&pair()), &Export3DOptions::default()).unwrap());

        assert_eq!(document["scenes"][0]["nodes"], json!([0]));
        assert_eq!(document["nodes"][0]["name"], "Pair");
        assert!(document["nodes"][0].get("mesh").is_none());
        assert_eq!(document["nodes"][0]["children"], json!([1, 2]));
        for (index, name) in [(1, "Pair 1"), (2, "Pair 2")] {
            let solid = &document["nodes"][index];
            assert_eq!(solid["name"], name);
            assert_eq!(primitive_owners(&document, solid), [(json!(0), 12)]);
            assert!(declares_manifold(&document, solid));
        }
    }

    /// Colour does not split a solid: two colours fused with no authored owner
    /// are one fallback-named mesh with one primitive per colour.
    #[test]
    fn two_colour_fused_anonymous_solid_is_one_mesh_with_two_primitives() {
        let document = glb_json(
            &serialize_glb(&rendered(&two_colour_fused()), &Export3DOptions::default()).unwrap(),
        );

        assert_eq!(document["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(document["meshes"].as_array().unwrap().len(), 1);
        let solid = &document["nodes"][0];
        assert_eq!(solid["name"], "#FF0000FF Shape");
        assert_eq!(solid["extras"]["openrscad"]["fallback"], true);
        let primitives = document["meshes"][0]["primitives"].as_array().unwrap();
        assert_eq!(primitives.len(), 2);
        assert!(primitives
            .iter()
            .all(|primitive| primitive.get("extras").is_none()));
        assert_ne!(primitives[0]["material"], primitives[1]["material"]);
        assert!(declares_manifold(&document, solid));
    }

    /// An open sheet cannot be declared manifold. It is still exported, whole
    /// and unfused, without the extension, and the export says why.
    #[test]
    fn non_manifold_solid_is_exported_without_the_extension_and_reported() {
        let artifact = export_3d(
            &open_triangle_scene(),
            ExportFormat3D::Glb,
            &Export3DOptions::default(),
        )
        .unwrap();
        let document = glb_json(&artifact.bytes);

        assert_eq!(surface_triangle_count(&document), 1);
        assert!(document["meshes"][0].get("extensions").is_none());
        assert!(document.get("extensionsUsed").is_none());
        assert_eq!(artifact.warnings.len(), 1);
        assert!(
            artifact.warnings[0].contains("EXT_mesh_manifold"),
            "{:?}",
            artifact.warnings
        );
        assert_eq!(assert_manifold_meshes(&artifact.bytes), 0);
    }
}
