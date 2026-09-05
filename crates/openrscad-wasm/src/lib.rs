//! wasm-bindgen marshalling for the browser and Node bundles.
//!
//! Every entry point here converts JS-friendly parallel arrays into an
//! [`openrscad_api::Request`], calls `openrscad-api`, and converts the result
//! back into a `#[wasm_bindgen]` struct. **No pipeline logic lives in this
//! crate** — it and the N-API addon in `openrscad-napi` are two marshalling
//! layers over one implementation, which is the only way the two builds can be
//! held to a byte-identical artifact contract.

use wasm_bindgen::prelude::*;

/// Initialize panic hook for readable errors in the browser console.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Return and clear the last profile produced by a benchmark-feature build.
#[cfg(feature = "benchmark-profile")]
#[wasm_bindgen]
pub fn take_last_benchmark_profile() -> String {
    openrscad_api::take_last_benchmark_profile()
}

/// Drop the persistent geometry cache (e.g. when loading a new document).
#[wasm_bindgen]
pub fn clear_cache() {
    openrscad_api::clear_cache();
}

/// Serialize the geometry-cache entries added at or after `since_epoch`
/// (`0` = everything) as an opaque, versioned blob for the host to persist.
#[wasm_bindgen]
pub fn cache_export(since_epoch: u32) -> Vec<u8> {
    openrscad_api::cache_export(u64::from(since_epoch))
}

/// Rehydrate entries from a `cache_export` blob of the same engine version and
/// kernel. Returns a JSON report; throws on a foreign or malformed blob.
#[wasm_bindgen]
pub fn cache_import(bytes: &[u8]) -> Result<String, JsError> {
    openrscad_api::cache_import(bytes).map_err(|error| JsError::new(&error))
}

/// Resident-cache accounting, caps and envelope as JSON.
#[wasm_bindgen]
pub fn cache_stats() -> String {
    openrscad_api::cache_stats()
}

/// Every resident structural key, ascending, as a `BigUint64Array`.
#[wasm_bindgen]
pub fn cache_keys() -> Vec<u64> {
    openrscad_api::cache_keys()
}

/// Engine version string.
#[wasm_bindgen]
pub fn version() -> String {
    openrscad_api::version()
}

/// The customizer parameter schema for a source string, as a JSON string
/// (`{"params":[…]}`). The playground renders a control panel from this.
#[wasm_bindgen]
pub fn parameters(source: &str) -> String {
    openrscad_api::parameters(source)
}

/// Build a request from the parallel arrays the JS surface speaks.
///
/// `bin_data` and `font_blobs` are base64 because raw bytes cannot cross the
/// string-typed wasm boundary; entries that fail to decode are dropped (the
/// engine then warns "can't open" for that import, and `text()` falls back to
/// Liberation). A native host passes `Buffer`s straight through instead.
#[allow(clippy::too_many_arguments)]
fn request(
    source: &str,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
) -> openrscad_api::Request {
    openrscad_api::Request {
        source: source.to_string(),
        params: names.into_iter().zip(values).collect(),
        files: file_names.into_iter().zip(file_contents).collect(),
        binary_files: bin_names
            .into_iter()
            .zip(bin_data)
            .filter_map(|(name, data)| base64_decode(&data).map(|bytes| (name, bytes)))
            .collect(),
        font_files: font_blobs
            .iter()
            .filter_map(|blob| base64_decode(blob))
            .collect(),
    }
}

/// The result of rendering a `.scad` source string.
///
/// Mesh data is a non-indexed triangle soup with flat (per-face) normals:
/// `positions` and `normals` both hold 9 floats per triangle.
#[wasm_bindgen]
pub struct RenderResult {
    positions: Vec<f32>,
    normals: Vec<f32>,
    echo: String,
    warnings: String,
    error: Option<String>,
    /// Recoverable geometry errors: newline-joined messages for CSG ops that
    /// failed and were replaced by a fallback mesh (e.g. non-manifold operands).
    /// Non-empty means the preview is degraded — a mesh is present but wrong
    /// somewhere — and the UI should alert the user. Distinct from `error`
    /// (a hard failure that yields no mesh).
    geom_errors: String,
    /// Structured diagnostics (JSON array) for inline editor squiggles.
    diagnostics: String,
    /// Preview color channel (only populated when the model uses `color`/`#`/`%`):
    /// a concatenated triangle soup plus a JSON array of per-group ranges/colors.
    preview_positions: Vec<f32>,
    preview_normals: Vec<f32>,
    groups: String,
    /// Provenance channel for editor↔preview linking (2D and 3D alike): a
    /// concatenated per-leaf triangle soup plus a JSON array of per-group
    /// `{start,count,spans}` ranges. `spans` is the outermost→innermost stack of
    /// `[start,end]` byte offsets into the source (an empty array when
    /// unattributable). Empty only for models with no geometry.
    provenance_positions: Vec<f32>,
    provenance_normals: Vec<f32>,
    provenance: String,
    /// `$vp*` viewport variables as JSON (only when the source references `$vp`).
    viewport: String,
    triangle_count: u32,
    vertex_count: u32,
    volume: f64,
    area: f64,
    is_2d: bool,
}

#[wasm_bindgen]
impl RenderResult {
    /// Triangle-soup vertex positions (9 f32 per triangle) as a `Float32Array`.
    #[wasm_bindgen(getter)]
    pub fn positions(&self) -> Vec<f32> {
        self.positions.clone()
    }

    /// Per-face normals (9 f32 per triangle) as a `Float32Array`.
    #[wasm_bindgen(getter)]
    pub fn normals(&self) -> Vec<f32> {
        self.normals.clone()
    }

    /// Newline-joined `ECHO:` output.
    #[wasm_bindgen(getter)]
    pub fn echo(&self) -> String {
        self.echo.clone()
    }

    /// Newline-joined warnings.
    #[wasm_bindgen(getter)]
    pub fn warnings(&self) -> String {
        self.warnings.clone()
    }

    /// Error message, or empty string if the render succeeded.
    #[wasm_bindgen(getter)]
    pub fn error(&self) -> String {
        self.error.clone().unwrap_or_default()
    }

    /// Whether the render succeeded (no error).
    #[wasm_bindgen(getter)]
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    /// Newline-joined recoverable geometry errors (degraded render), or empty
    /// when the geometry is exact. See the field docs on `geom_errors`.
    #[wasm_bindgen(getter)]
    pub fn geom_errors(&self) -> String {
        self.geom_errors.clone()
    }

    /// Structured diagnostics as a JSON array (`[{severity,message,start,end}]`),
    /// where start/end are byte offsets into the source, or -1 when unknown.
    #[wasm_bindgen(getter)]
    pub fn diagnostics(&self) -> String {
        self.diagnostics.clone()
    }

    /// Preview triangle soup (concatenated colored groups); empty when the model
    /// uses no color/`#`/`%` (the viewer then uses `positions`).
    #[wasm_bindgen(getter)]
    pub fn preview_positions(&self) -> Vec<f32> {
        self.preview_positions.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn preview_normals(&self) -> Vec<f32> {
        self.preview_normals.clone()
    }

    /// Per-group ranges/colors as JSON (`[{start,count,color,mode}]`); empty `[]`
    /// when the model uses no display attributes.
    #[wasm_bindgen(getter)]
    pub fn groups(&self) -> String {
        self.groups.clone()
    }

    /// Provenance triangle soup (concatenated per-statement groups); empty only
    /// for models with no geometry.
    #[wasm_bindgen(getter)]
    pub fn provenance_positions(&self) -> Vec<f32> {
        self.provenance_positions.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn provenance_normals(&self) -> Vec<f32> {
        self.provenance_normals.clone()
    }

    /// Per-group provenance ranges/span-stacks as JSON (`[{start,count,spans}]`);
    /// empty when the model has no pickable geometry.
    #[wasm_bindgen(getter)]
    pub fn provenance(&self) -> String {
        self.provenance.clone()
    }

    /// `$vp*` viewport variables as JSON, or empty when the source has no `$vp`.
    #[wasm_bindgen(getter)]
    pub fn viewport(&self) -> String {
        self.viewport.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn triangle_count(&self) -> u32 {
        self.triangle_count
    }

    #[wasm_bindgen(getter)]
    pub fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    #[wasm_bindgen(getter)]
    pub fn volume(&self) -> f64 {
        self.volume
    }

    #[wasm_bindgen(getter)]
    pub fn area(&self) -> f64 {
        self.area
    }

    /// Whether the model is a 2D object (exportable to DXF/SVG) vs a 3D solid.
    #[wasm_bindgen(getter)]
    pub fn is_2d(&self) -> bool {
        self.is_2d
    }
}

impl From<openrscad_api::Render> for RenderResult {
    fn from(value: openrscad_api::Render) -> Self {
        Self {
            positions: value.positions,
            normals: value.normals,
            echo: value.echo,
            warnings: value.warnings,
            error: value.error,
            geom_errors: value.geom_errors,
            diagnostics: value.diagnostics,
            preview_positions: value.preview_positions,
            preview_normals: value.preview_normals,
            groups: value.groups,
            provenance_positions: value.provenance_positions,
            provenance_normals: value.provenance_normals,
            provenance: value.provenance,
            viewport: value.viewport,
            triangle_count: value.triangle_count,
            vertex_count: value.vertex_count,
            volume: value.volume,
            area: value.area,
            is_2d: value.is_2d,
        }
    }
}

/// One owned native 3D artifact plus the operational metadata needed by hosts.
#[wasm_bindgen]
pub struct ExportShape3DResult {
    bytes: Vec<u8>,
    format: String,
    echo: String,
    warnings: String,
    error: Option<String>,
    geom_errors: String,
    diagnostics: String,
    viewport: String,
    triangle_count: u32,
    vertex_count: u32,
    volume: f64,
    area: f64,
    is_2d: bool,
}

#[wasm_bindgen]
impl ExportShape3DResult {
    /// Transfer artifact ownership to JavaScript. A second call returns empty.
    pub fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }

    #[wasm_bindgen(getter)]
    pub fn format(&self) -> String {
        self.format.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn ok(&self) -> bool {
        self.error.is_none()
    }

    #[wasm_bindgen(getter)]
    pub fn error(&self) -> String {
        self.error.clone().unwrap_or_default()
    }

    #[wasm_bindgen(getter)]
    pub fn echo(&self) -> String {
        self.echo.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn warnings(&self) -> String {
        self.warnings.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn geom_errors(&self) -> String {
        self.geom_errors.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn diagnostics(&self) -> String {
        self.diagnostics.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn viewport(&self) -> String {
        self.viewport.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn triangle_count(&self) -> u32 {
        self.triangle_count
    }

    #[wasm_bindgen(getter)]
    pub fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    #[wasm_bindgen(getter)]
    pub fn volume(&self) -> f64 {
        self.volume
    }

    #[wasm_bindgen(getter)]
    pub fn area(&self) -> f64 {
        self.area
    }

    #[wasm_bindgen(getter)]
    pub fn is_2d(&self) -> bool {
        self.is_2d
    }
}

impl ExportShape3DResult {
    fn from_error(format: &str, message: String, diagnostics: String) -> Self {
        openrscad_api::Artifact3d {
            format: format.to_string(),
            error: Some(message),
            diagnostics,
            ..openrscad_api::Artifact3d::default()
        }
        .into()
    }
}

impl From<openrscad_api::Artifact3d> for ExportShape3DResult {
    fn from(value: openrscad_api::Artifact3d) -> Self {
        Self {
            bytes: value.bytes,
            format: value.format,
            echo: value.echo,
            warnings: value.warnings,
            error: value.error,
            geom_errors: value.geom_errors,
            diagnostics: value.diagnostics,
            viewport: value.viewport,
            triangle_count: value.triangle_count,
            vertex_count: value.vertex_count,
            volume: value.volume,
            area: value.area,
            is_2d: value.is_2d,
        }
    }
}

/// Evaluate a 3D source and serialize it natively to one owned artifact.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn export_3d(
    source: &str,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
    format: &str,
    include_edges: bool,
    source_unit_to_meters: f64,
    coordinate_system: &str,
) -> ExportShape3DResult {
    if names.len() != values.len()
        || file_names.len() != file_contents.len()
        || bin_names.len() != bin_data.len()
    {
        return ExportShape3DResult::from_error(
            format,
            "parallel request arrays have different lengths".to_string(),
            "[]".to_string(),
        );
    }
    let request = request(
        source,
        names,
        values,
        file_names,
        file_contents,
        bin_names,
        bin_data,
        font_blobs,
    );
    openrscad_api::artifact_3d(
        &request,
        &openrscad_api::ExportOptions {
            format: format.to_string(),
            include_edges,
            source_unit_to_meters,
            coordinate_system: coordinate_system.to_string(),
            preview: false,
        },
    )
    .into()
}

/// Render a preview-semantics GLB for interactive viewers.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn render_to_glb(
    source: &str,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
    include_edges: bool,
) -> ExportShape3DResult {
    if names.len() != values.len()
        || file_names.len() != file_contents.len()
        || bin_names.len() != bin_data.len()
    {
        return ExportShape3DResult::from_error(
            "glb",
            "parallel request arrays have different lengths".to_string(),
            "[]".to_string(),
        );
    }
    let request = request(
        source,
        names,
        values,
        file_names,
        file_contents,
        bin_names,
        bin_data,
        font_blobs,
    );
    openrscad_api::artifact_3d(
        &request,
        &openrscad_api::ExportOptions {
            format: "glb".to_string(),
            include_edges,
            preview: true,
            ..openrscad_api::ExportOptions::default()
        },
    )
    .into()
}

/// Render a 2D model and serialize it to DXF or SVG text. Returns an empty
/// string if the model isn't 2D or fails to evaluate (the caller checks
/// `RenderResult.is_2d` first). `format` is "dxf" or "svg".
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn export_2d(
    source: &str,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
    format: &str,
) -> String {
    let request = request(
        source,
        names,
        values,
        file_names,
        file_contents,
        bin_names,
        bin_data,
        font_blobs,
    );
    openrscad_api::export_2d(&request, format)
}

/// Run the full pipeline on a source string.
#[wasm_bindgen]
pub fn render(source: &str) -> RenderResult {
    render_with_params(source, Vec::new(), Vec::new())
}

/// Like [`render`], but with customizer overrides supplied as parallel arrays:
/// `names[i]` is a top-level parameter and `values[i]` its new value as a
/// literal string (`"30"`, `"true"`, `"\"hi\""`, `"[1,2,3]"`).
#[wasm_bindgen]
pub fn render_with_params(source: &str, names: Vec<String>, values: Vec<String>) -> RenderResult {
    render_with_files(
        source,
        names,
        values,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
}

/// Like [`render_with_params`], but `include`/`use` resolve against an in-memory
/// set of files (`file_names[i]` → `file_contents[i]`) — the playground's other
/// files and/or a bundled library.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn render_with_files(
    source: &str,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
) -> RenderResult {
    let request = request(
        source,
        names,
        values,
        file_names,
        file_contents,
        bin_names,
        bin_data,
        font_blobs,
    );
    openrscad_api::render(&request, false).into()
}

/// Like [`render_with_files`], but renders the fast, **non-watertight** preview
/// (see `openrscad_geom::render_preview_cached_diag`): unions are concatenated rather
/// than run through the CSG kernel. Suitable for opaque on-screen display only —
/// stats and export still use the exact path. Differences/intersections/hulls
/// still resolve exactly, so holes and clips look correct.
#[wasm_bindgen]
#[allow(clippy::too_many_arguments)]
pub fn render_preview_with_files(
    source: &str,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
) -> RenderResult {
    let request = request(
        source,
        names,
        values,
        file_names,
        file_contents,
        bin_names,
        bin_data,
        font_blobs,
    );
    openrscad_api::render(&request, true).into()
}

/// Decode standard base64 (RFC 4648, with `=` padding), ignoring ASCII
/// whitespace. Returns `None` on any invalid character or malformed length.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut quad = [0u8; 4];
    let mut n = 0;
    let mut pad = 0;
    for &c in s.as_bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            quad[n] = 0;
            pad += 1;
            n += 1;
        } else {
            if pad > 0 {
                return None; // data after padding
            }
            quad[n] = val(c)?;
            n += 1;
        }
        if n == 4 {
            if pad > 2 {
                return None; // a group is at most 2 padding chars
            }
            out.push((quad[0] << 2) | (quad[1] >> 4));
            if pad < 2 {
                out.push((quad[1] << 4) | (quad[2] >> 2));
            }
            if pad < 1 {
                out.push((quad[2] << 6) | quad[3]);
            }
            n = 0;
            if pad > 0 {
                break;
            }
        }
    }
    if n != 0 {
        return None; // truncated group
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Each test runs on the host under `cargo test` (native Manifold kernel) and,
    // via `wasm-pack test`, in a real browser (the pure-Rust boolmesh kernel the
    // playground actually executes). The paired `cfg_attr`s below pick `#[test]`
    // on the host and `#[wasm_bindgen_test]` on wasm, so one source covers both.
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test;
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    fn export(source: &str, format: &str, include_edges: bool) -> ExportShape3DResult {
        export_3d(
            source,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            format,
            include_edges,
            0.001,
            "y-up",
        )
    }

    fn glb_json(bytes: &[u8]) -> serde_json::Value {
        let length = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        serde_json::from_slice(&bytes[20..20 + length]).unwrap()
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn export_3d_returns_owned_multipart_glb_and_optional_edges() {
        let source = "color(\"red\") { cube(1); translate([0,0,2]) cube(1); }";
        let mut plain = export(source, "glb", false);
        assert!(plain.ok(), "{}", plain.error());
        let bytes = plain.take_bytes();
        assert_eq!(&bytes[..4], b"glTF");
        assert!(plain.take_bytes().is_empty());
        let json = glb_json(&bytes);
        assert_eq!(json["nodes"].as_array().unwrap().len(), 2);
        assert!(json["meshes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|mesh| { mesh["primitives"].as_array().unwrap().len() == 1 }));

        let mut edged = export(source, "glb", true);
        assert!(edged.ok(), "{}", edged.error());
        let json = glb_json(&edged.take_bytes());
        assert!(json["meshes"].as_array().unwrap().iter().all(|mesh| {
            let primitives = mesh["primitives"].as_array().unwrap();
            primitives.len() == 2 && primitives[0]["mode"] == 4 && primitives[1]["mode"] == 1
        }));
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn authored_modules_emit_nested_nodes_across_files_and_collapse_loop_instances() {
        let source = r#"
            use <roof.scad>
            module greenhouse() {
                cube([4, 1, 1]);
                roof_frame();
            }
            greenhouse();
        "#;
        let library = r#"
            module arch_loop() { cube([1, 1, 1]); }
            module roof_frame() {
                for (x = [0:2]) translate([x, 0, 1]) arch_loop();
            }
        "#;
        let mut result = export_3d(
            source,
            vec![],
            vec![],
            vec!["roof.scad".to_string()],
            vec![library.to_string()],
            vec![],
            vec![],
            vec![],
            "glb",
            false,
            0.001,
            "y-up",
        );
        assert!(result.ok(), "{}", result.error());
        let document = glb_json(&result.take_bytes());

        assert_eq!(document["scenes"][0]["nodes"], serde_json::json!([0]));
        assert_eq!(document["nodes"][0]["name"], "Greenhouse");
        assert!(document["nodes"][0]["mesh"].is_number());
        assert_eq!(document["nodes"][0]["children"], serde_json::json!([1]));
        assert_eq!(document["nodes"][1]["name"], "Roof Frame");
        assert_eq!(document["nodes"][1]["children"], serde_json::json!([2]));
        assert_eq!(document["nodes"][2]["name"], "Arch Loop");
        assert_eq!(
            document["nodes"][1]["extras"]["openrscad"]["definitionSite"]["source"],
            "roof.scad"
        );
        assert_eq!(document["nodes"].as_array().unwrap().len(), 3);
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn export_3d_supports_every_3d_format() {
        for (format, magic) in [
            ("stl", &b"\0\0\0\0"[..]),
            ("off", &b"OFF\n"[..]),
            ("obj", &b"v "[..]),
            ("3mf", &b"PK\x03\x04"[..]),
            ("amf", &b"<?xml"[..]),
            ("glb", &b"glTF"[..]),
        ] {
            let mut result = export("cube(1);", format, false);
            assert!(result.ok(), "{format}: {}", result.error());
            let bytes = result.take_bytes();
            assert!(bytes.starts_with(magic), "{format}");
            assert_eq!(result.triangle_count(), 12);
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn structured_cache_reuses_geometry_not_serialized_artifacts() {
        clear_cache();
        openrscad_api::reset_structured_render_count();
        let mut plain = export("cube(1);", "glb", false);
        let plain_bytes = plain.take_bytes();
        let mut edged = export("cube(1);", "glb", true);
        let edged_bytes = edged.take_bytes();
        let mut threemf = export("cube(1);", "3mf", false);
        assert!(!threemf.take_bytes().is_empty());
        assert_eq!(openrscad_api::structured_render_count(), 1);
        assert_ne!(plain_bytes, edged_bytes);

        clear_cache();
        let _ = export("cube(1);", "glb", false);
        assert_eq!(openrscad_api::structured_render_count(), 2);
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn preview_glb_styles_highlight_and_keeps_background_out_of_export() {
        let source = "#cube(1); %translate([0,0,2]) cube(1);";
        let mut preview = render_to_glb(
            source,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            false,
        );
        assert!(preview.ok(), "{}", preview.error());
        let preview = glb_json(&preview.take_bytes());
        assert_eq!(preview["nodes"].as_array().unwrap().len(), 2);
        let modes: std::collections::BTreeSet<_> = preview["meshes"]
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
        assert_eq!(
            modes,
            std::collections::BTreeSet::from(["background", "highlight"])
        );

        let mut exported = export(source, "glb", false);
        assert!(exported.ok(), "{}", exported.error());
        let exported = glb_json(&exported.take_bytes());
        assert_eq!(exported["nodes"].as_array().unwrap().len(), 1);
        assert!(exported["meshes"][0]["primitives"][0]["extras"].is_null());
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn api_mode_owns_preview_for_render_and_every_export() {
        let source = "if ($preview) cube(1); else cube(2);";
        let mut rendered = render_to_glb(
            source,
            vec!["$preview".to_string()],
            vec!["false".to_string()],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            false,
        );
        assert!(rendered.ok(), "{}", rendered.error());
        assert!((rendered.volume() - 1.0).abs() < 1e-6);
        assert!(!rendered.take_bytes().is_empty());

        for format in ["glb", "3mf", "stl", "off", "obj", "amf"] {
            let result = export_3d(
                source,
                vec!["$preview".to_string()],
                vec!["true".to_string()],
                vec![],
                vec![],
                vec![],
                vec![],
                vec![],
                format,
                false,
                0.001,
                "y-up",
            );
            assert!(result.ok(), "{format}: {}", result.error());
            assert!((result.volume() - 8.0).abs() < 1e-6, "{format}");
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn color_populates_preview_channel() {
        // A plain model: no preview channel (viewer uses `positions`).
        let plain = render_with_files(
            "cube(2);",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
        assert!(plain.ok());
        assert_eq!(plain.groups(), "");
        assert!(plain.preview_positions().is_empty());

        // A colored model: preview soup + groups JSON populated.
        let colored = render_with_files(
            "color(\"red\") cube(2); color([0,0,1]) translate([5,0,0]) sphere(2);",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
        assert!(colored.ok());
        assert!(!colored.preview_positions().is_empty());
        let g = colored.groups();
        assert!(g.contains("\"mode\":\"solid\""), "{g}");
        assert!(g.contains("\"color\":[1,0,0,1]"), "{g}");
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn preview_render_skips_the_union() {
        // Two overlapping cubes. The exact render unions them (re-meshing the
        // seam); the preview render concatenates (12 + 12 triangles, no boolean).
        let src = "cube(2); translate([1,0,0]) cube(2);";
        let exact = render_with_files(src, vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
        let preview =
            render_preview_with_files(src, vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
        assert!(exact.ok() && preview.ok());
        assert_eq!(
            preview.triangle_count(),
            24,
            "preview should not run the union"
        );
        assert_ne!(
            preview.triangle_count(),
            exact.triangle_count(),
            "the exact union re-meshes the overlap; preview does not"
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn provenance_channel_populated_for_3d() {
        // A 3D model gets a provenance channel with per-statement spans.
        let r = render_with_files(
            "cube(2); translate([5,0,0]) sphere(2);",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
        assert!(r.ok());
        assert!(!r.provenance_positions().is_empty());
        let p = r.provenance();
        assert!(p.contains("\"spans\":[["), "{p}");
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn provenance_channel_populated_for_2d() {
        // A 2D model is pickable/highlightable just like 3D: the flat mesh gets a
        // provenance channel with per-statement spans.
        let r = render_with_files(
            "square(4);",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
        assert!(r.ok());
        assert!(r.is_2d());
        assert!(!r.provenance_positions().is_empty());
        let p = r.provenance();
        assert!(p.contains("\"spans\":[["), "{p}");
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn non_manifold_boolean_degrades_and_surfaces_geom_errors() {
        // Unioning a cube with a lone open triangle (non-manifold) can't be done
        // by the kernel. The render must not fail outright: a (degraded) mesh is
        // still returned and the failure is reported on the geom_errors channel.
        let src = "union() { cube(10); \
                   polyhedron(points=[[0,0,0],[1,0,0],[0,1,0]], faces=[[0,1,2]]); }";
        let r = render_with_files(src, vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
        assert!(
            r.ok(),
            "degraded render should still succeed: {}",
            r.error()
        );
        assert!(r.triangle_count() > 0, "expected a fallback mesh");
        assert!(
            r.geom_errors().contains("union"),
            "geom_errors: {:?}",
            r.geom_errors()
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn structured_export_diagnostics_survive_cold_and_warm_cache_calls() {
        clear_cache();
        let source =
            "union() { cube(10); polyhedron(points=[[0,0,0],[1,0,0],[0,1,0]], faces=[[0,1,2]]); }";
        let mut cold = export(source, "glb", false);
        assert!(cold.ok(), "{}", cold.error());
        assert!(!cold.take_bytes().is_empty());
        assert!(
            cold.geom_errors().contains("union"),
            "{}",
            cold.geom_errors()
        );

        let mut warm = export(source, "glb", false);
        assert!(warm.ok(), "{}", warm.error());
        assert!(!warm.take_bytes().is_empty());
        assert_eq!(warm.geom_errors(), cold.geom_errors());

        let threemf = export(source, "3mf", false);
        assert!(!threemf.ok());
        assert!(
            threemf.error().contains("not manifold"),
            "{}",
            threemf.error()
        );
        assert!(
            threemf.geom_errors().contains("union"),
            "{}",
            threemf.geom_errors()
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn diagnostics_surface_parse_eval_and_warnings() {
        // Parse error → an error diagnostic with a byte span.
        let r = render_with_files(
            "cube(",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
        assert!(!r.ok());
        assert!(
            r.diagnostics().contains("\"severity\":\"error\""),
            "{}",
            r.diagnostics()
        );

        // Eval error (assert) → an error diagnostic.
        let r = render_with_files(
            "assert(false);",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
        assert!(!r.ok());
        assert!(
            r.diagnostics().contains("\"severity\":\"error\""),
            "{}",
            r.diagnostics()
        );

        // Unknown module → a warning diagnostic; the render still succeeds.
        let r = render_with_files(
            "nope();",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
        );
        assert!(r.ok());
        let d = r.diagnostics();
        assert!(d.contains("\"severity\":\"warning\""), "{d}");
        assert!(d.contains("nope"), "{d}");
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn schema_json_shapes() {
        let src = "\
/* [Box] */
// the width
width = 10; // [1:100]
mode = 1;   // [0:Off, 1:On]
flag = true;
name = \"hi\"; // 8
v = [1, 2, 3];
";
        let json = openrscad_syntax::customizer::extract(src).to_json();
        // Spot-check the salient pieces (order preserved).
        assert!(json.contains(r#""name":"width""#));
        assert!(json.contains(r#""group":"Box""#));
        assert!(json.contains(r#""description":"the width""#));
        assert!(json.contains(r#""kind":"slider","min":1,"max":100,"step":null"#));
        assert!(json.contains(r#""kind":"dropdown","options":[{"value":0,"label":"Off"}"#));
        assert!(json.contains(
            r#""name":"flag","group":"Box","description":null,"type":"bool","value":true"#
        ));
        assert!(json.contains(r#""kind":"text","maxLength":8"#));
        assert!(json
            .contains(r#""type":"vector","value":[1,2,3],"control":{"kind":"vector","length":3}"#));
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn render_applies_overrides() {
        // width=10 default → 10*10*10; override width=4 → 4*10*10 = 400.
        let src = "width = 10;\ncube([width, 10, 10]);";
        let base = render_with_params(src, vec![], vec![]);
        assert!(base.ok());
        assert!(
            (base.volume() - 1000.0).abs() < 1e-6,
            "vol {}",
            base.volume()
        );

        let overridden = render_with_params(src, vec!["width".to_string()], vec!["4".to_string()]);
        assert!(overridden.ok());
        assert!(
            (overridden.volume() - 400.0).abs() < 1e-6,
            "vol {}",
            overridden.volume()
        );
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn render_resolves_files() {
        // `use` a helper file from the in-memory resolver.
        let main = "use <lib.scad>\ncube([side(), side(), side()]);";
        let lib = "function side() = 3;";
        let r = render_with_files(
            main,
            vec![],
            vec![],
            vec!["lib.scad".to_string()],
            vec![lib.to_string()],
            vec![],
            vec![],
            vec![],
        );
        assert!(r.ok(), "err: {}", r.error());
        assert!((r.volume() - 27.0).abs() < 1e-6, "vol {}", r.volume());
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn imports_dxf_from_a_tab() {
        // A DXF profile held in a tab is imported via load_bytes and extruded.
        let outer = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 20.0], [0.0, 20.0]];
        let dxf = openrscad_geom::export_dxf(&[outer]);
        let r = render_with_files(
            "linear_extrude(3) import(\"p.dxf\");",
            vec![],
            vec![],
            vec!["p.dxf".to_string()],
            vec![dxf],
            vec![],
            vec![],
            vec![],
        );
        assert!(r.ok(), "err: {}", r.error());
        assert!((r.volume() - 600.0).abs() < 1e-3, "vol {}", r.volume()); // 10*20*3
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn export_2d_produces_dxf_and_svg() {
        let src = "square([10, 20]);";
        let dxf = export_2d(
            src,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            "dxf",
        );
        assert!(dxf.contains("LWPOLYLINE"), "dxf: {dxf}");
        let svg = export_2d(
            src,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            "svg",
        );
        assert!(svg.contains("<svg") && svg.contains("<path"), "svg: {svg}");
        // A 3D model yields no 2D export.
        assert!(export_2d(
            "cube(1);",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            "dxf"
        )
        .is_empty());
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn export_2d_exports_bare_projection() {
        let src = "projection(cut=false) cube([10, 20, 30]);";
        let check_footprint = |contours: &[openrscad_geom::Contour]| {
            assert!(!contours.is_empty());
            let mut area = 0.0;
            let mut lo = [f64::INFINITY; 2];
            let mut hi = [f64::NEG_INFINITY; 2];
            for contour in contours {
                for i in 0..contour.len() {
                    let p = contour[i];
                    let q = contour[(i + 1) % contour.len()];
                    area += p[0] * q[1] - q[0] * p[1];
                    for axis in 0..2 {
                        lo[axis] = lo[axis].min(p[axis]);
                        hi[axis] = hi[axis].max(p[axis]);
                    }
                }
            }
            assert!((area.abs() / 2.0 - 200.0).abs() < 1e-3, "area {area}");
            assert_eq!(lo, [0.0, 0.0]);
            assert_eq!(hi, [10.0, 20.0]);
        };
        let dxf = export_2d(
            src,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            "dxf",
        );
        let contours = openrscad_geom::import_dxf(dxf.as_bytes());
        check_footprint(&contours);

        let svg = export_2d(
            src,
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            "svg",
        );
        let contours = openrscad_geom::import_svg(svg.as_bytes());
        check_footprint(&contours);
    }

    #[test]
    fn base64_decode_round_trips_rfc_vectors() {
        // RFC 4648 §10 test vectors, exercising 0/1/2 padding bytes.
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYg==").unwrap(), b"foob");
        assert_eq!(base64_decode("Zm9vYmE=").unwrap(), b"fooba");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        // Interspersed newlines (as the browser's chunked encoder may emit) are ignored.
        assert_eq!(base64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
        // Full byte range survives.
        let all: Vec<u8> = (0u8..=255).collect();
        assert_eq!(base64_decode(&b64_encode(&all)).unwrap(), all);
        // Malformed inputs are rejected, not silently truncated.
        assert!(base64_decode("Zg=").is_none()); // truncated group
        assert!(base64_decode("Zm9v====").is_none()); // over-padded
        assert!(base64_decode("Zm.9").is_none()); // invalid char
    }

    #[cfg_attr(not(target_arch = "wasm32"), test)]
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
    fn imports_binary_stl_from_base64_bin_channel() {
        // A 2×2×2 cube exported as *binary* STL, carried through the base64 binary
        // channel and resolved by `import()` — the browser path binary meshes take.
        let stl = unit_cube_mesh(2.0).to_binary_stl();
        let r = render_with_files(
            "import(\"cube.stl\");",
            vec![],
            vec![],
            vec![],
            vec![],
            vec!["cube.stl".to_string()],
            vec![b64_encode(&stl)],
            vec![],
        );
        assert!(r.ok(), "err: {}", r.error());
        assert!(r.triangle_count() >= 12, "tris {}", r.triangle_count());
        assert!((r.volume() - 8.0).abs() < 1e-6, "vol {}", r.volume());
    }

    /// Standard base64 encoder for tests (mirrors the browser's `btoa` output).
    fn b64_encode(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(T[(n >> 18 & 63) as usize] as char);
            out.push(T[(n >> 12 & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                T[(n >> 6 & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                T[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    /// An axis-aligned cube of side `s` at the origin, as a closed triangle mesh.
    fn unit_cube_mesh(s: f64) -> openrscad_geom::Mesh {
        let verts = vec![
            [0.0, 0.0, 0.0],
            [s, 0.0, 0.0],
            [s, s, 0.0],
            [0.0, s, 0.0],
            [0.0, 0.0, s],
            [s, 0.0, s],
            [s, s, s],
            [0.0, s, s],
        ];
        // Outward-facing winding for each of the 6 faces.
        let tris = vec![
            [0, 3, 2],
            [0, 2, 1], // bottom (z=0)
            [4, 5, 6],
            [4, 6, 7], // top (z=s)
            [0, 1, 5],
            [0, 5, 4], // y=0
            [2, 3, 7],
            [2, 7, 6], // y=s
            [1, 2, 6],
            [1, 6, 5], // x=s
            [0, 4, 7],
            [0, 7, 3], // x=0
        ];
        openrscad_geom::Mesh { verts, tris }
    }
}
