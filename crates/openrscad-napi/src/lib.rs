//! N-API marshalling for Node hosts (CLI, daemon, CI, desktop utility process).
//!
//! The exported surface is deliberately **the same shape as the wasm-bindgen
//! surface** — same function names, same parallel-array arguments, same result
//! classes with `take_bytes()`/`free()` — so `packages/npm-native` can feed it
//! straight into `makeApi()`, the one JS facade both engines share. No pipeline
//! logic lives here: everything below is a conversion into
//! [`openrscad_api::Request`] and back.
//!
//! Two properties this addon must keep, because they are what makes it a byte
//! parity lane rather than a second engine:
//!
//! * it links `manifold-rust` only (the `rust-relation` feature), never the C++
//!   Manifold kernel or TBB — see the `Cargo.toml` note;
//! * it never enables `fontdb`'s filesystem features, so `text()` sees exactly
//!   the fonts a browser would.

use napi::bindgen_prelude::*;
use napi_derive::napi;

/// Decode standard base64 (RFC 4648, with `=` padding), ignoring ASCII
/// whitespace. Returns `None` on any invalid character or malformed length.
///
/// Only the inbound binary-asset and font lanes are base64: the shared JS
/// facade encodes them because raw bytes cannot cross the string-typed wasm
/// boundary. The artifact comes back as a `Buffer` that adopts the Rust `Vec`,
/// so the large direction is never copied or re-encoded.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut quad = [0u8; 4];
    let mut filled = 0;
    let mut padding = 0;
    for &byte in input.as_bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            quad[filled] = 0;
            padding += 1;
            filled += 1;
        } else {
            if padding > 0 {
                return None; // data after padding
            }
            quad[filled] = value(byte)?;
            filled += 1;
        }
        if filled == 4 {
            if padding > 2 {
                return None; // a group is at most 2 padding chars
            }
            out.push((quad[0] << 2) | (quad[1] >> 4));
            if padding < 2 {
                out.push((quad[1] << 4) | (quad[2] >> 2));
            }
            if padding < 1 {
                out.push((quad[2] << 6) | quad[3]);
            }
            filled = 0;
            if padding > 0 {
                break;
            }
        }
    }
    if filled != 0 {
        return None; // truncated group
    }
    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn request(
    source: String,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
) -> openrscad_api::Request {
    openrscad_api::Request {
        source,
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

/// A rendered mesh plus the editor channels. Mirrors the wasm `RenderResult`.
#[napi]
pub struct RenderResult {
    inner: openrscad_api::Render,
}

#[napi]
impl RenderResult {
    #[napi(getter)]
    pub fn positions(&self) -> Float32Array {
        Float32Array::new(self.inner.positions.clone())
    }

    #[napi(getter)]
    pub fn normals(&self) -> Float32Array {
        Float32Array::new(self.inner.normals.clone())
    }

    #[napi(getter)]
    pub fn echo(&self) -> String {
        self.inner.echo.clone()
    }

    #[napi(getter)]
    pub fn warnings(&self) -> String {
        self.inner.warnings.clone()
    }

    #[napi(getter)]
    pub fn error(&self) -> String {
        self.inner.error.clone().unwrap_or_default()
    }

    #[napi(getter)]
    pub fn ok(&self) -> bool {
        self.inner.error.is_none()
    }

    #[napi(getter, js_name = "geom_errors")]
    pub fn geom_errors(&self) -> String {
        self.inner.geom_errors.clone()
    }

    #[napi(getter)]
    pub fn diagnostics(&self) -> String {
        self.inner.diagnostics.clone()
    }

    #[napi(getter, js_name = "preview_positions")]
    pub fn preview_positions(&self) -> Float32Array {
        Float32Array::new(self.inner.preview_positions.clone())
    }

    #[napi(getter, js_name = "preview_normals")]
    pub fn preview_normals(&self) -> Float32Array {
        Float32Array::new(self.inner.preview_normals.clone())
    }

    #[napi(getter)]
    pub fn groups(&self) -> String {
        self.inner.groups.clone()
    }

    #[napi(getter, js_name = "provenance_positions")]
    pub fn provenance_positions(&self) -> Float32Array {
        Float32Array::new(self.inner.provenance_positions.clone())
    }

    #[napi(getter, js_name = "provenance_normals")]
    pub fn provenance_normals(&self) -> Float32Array {
        Float32Array::new(self.inner.provenance_normals.clone())
    }

    #[napi(getter)]
    pub fn provenance(&self) -> String {
        self.inner.provenance.clone()
    }

    #[napi(getter)]
    pub fn viewport(&self) -> String {
        self.inner.viewport.clone()
    }

    #[napi(getter, js_name = "triangle_count")]
    pub fn triangle_count(&self) -> u32 {
        self.inner.triangle_count
    }

    #[napi(getter, js_name = "vertex_count")]
    pub fn vertex_count(&self) -> u32 {
        self.inner.vertex_count
    }

    #[napi(getter)]
    pub fn volume(&self) -> f64 {
        self.inner.volume
    }

    #[napi(getter)]
    pub fn area(&self) -> f64 {
        self.inner.area
    }

    #[napi(getter, js_name = "is_2d")]
    pub fn is_2d(&self) -> bool {
        self.inner.is_2d
    }

    /// No-op: the addon owns no linear memory, so there is nothing to release.
    /// It exists because the shared facade frees every wasm-owned result in a
    /// `finally`, and one facade is better than two.
    #[napi]
    pub fn free(&mut self) {}
}

/// One owned 3D artifact. Mirrors the wasm `ExportShape3DResult`.
#[napi]
pub struct ExportShape3DResult {
    inner: openrscad_api::Artifact3d,
}

#[napi]
impl ExportShape3DResult {
    /// Transfer artifact ownership to JavaScript. A second call returns empty.
    /// `Buffer::from(Vec<u8>)` adopts the allocation rather than copying it.
    #[napi(js_name = "take_bytes")]
    pub fn take_bytes(&mut self) -> Buffer {
        Buffer::from(std::mem::take(&mut self.inner.bytes))
    }

    #[napi(getter)]
    pub fn format(&self) -> String {
        self.inner.format.clone()
    }

    #[napi(getter)]
    pub fn ok(&self) -> bool {
        self.inner.error.is_none()
    }

    #[napi(getter)]
    pub fn error(&self) -> String {
        self.inner.error.clone().unwrap_or_default()
    }

    #[napi(getter)]
    pub fn echo(&self) -> String {
        self.inner.echo.clone()
    }

    #[napi(getter)]
    pub fn warnings(&self) -> String {
        self.inner.warnings.clone()
    }

    #[napi(getter, js_name = "geom_errors")]
    pub fn geom_errors(&self) -> String {
        self.inner.geom_errors.clone()
    }

    #[napi(getter)]
    pub fn diagnostics(&self) -> String {
        self.inner.diagnostics.clone()
    }

    #[napi(getter)]
    pub fn viewport(&self) -> String {
        self.inner.viewport.clone()
    }

    #[napi(getter, js_name = "triangle_count")]
    pub fn triangle_count(&self) -> u32 {
        self.inner.triangle_count
    }

    #[napi(getter, js_name = "vertex_count")]
    pub fn vertex_count(&self) -> u32 {
        self.inner.vertex_count
    }

    #[napi(getter)]
    pub fn volume(&self) -> f64 {
        self.inner.volume
    }

    #[napi(getter)]
    pub fn area(&self) -> f64 {
        self.inner.area
    }

    #[napi(getter, js_name = "is_2d")]
    pub fn is_2d(&self) -> bool {
        self.inner.is_2d
    }

    /// No-op; see [`RenderResult::free`].
    #[napi]
    pub fn free(&mut self) {}
}

fn mismatched(format: &str) -> ExportShape3DResult {
    ExportShape3DResult {
        inner: openrscad_api::Artifact3d {
            format: format.to_string(),
            error: Some("parallel request arrays have different lengths".to_string()),
            diagnostics: "[]".to_string(),
            ..openrscad_api::Artifact3d::default()
        },
    }
}

fn ragged(
    names: &[String],
    values: &[String],
    file_names: &[String],
    file_contents: &[String],
    bin_names: &[String],
    bin_data: &[String],
) -> bool {
    names.len() != values.len()
        || file_names.len() != file_contents.len()
        || bin_names.len() != bin_data.len()
}

/// Evaluate a 3D source and serialize it natively to one owned artifact.
#[napi(js_name = "export_3d")]
#[allow(clippy::too_many_arguments)]
pub fn export_3d(
    source: String,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
    format: String,
    include_edges: bool,
    source_unit_to_meters: f64,
    coordinate_system: String,
) -> ExportShape3DResult {
    if ragged(
        &names,
        &values,
        &file_names,
        &file_contents,
        &bin_names,
        &bin_data,
    ) {
        return mismatched(&format);
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
    ExportShape3DResult {
        inner: openrscad_api::artifact_3d(
            &request,
            &openrscad_api::ExportOptions {
                format,
                include_edges,
                source_unit_to_meters,
                coordinate_system,
                preview: false,
            },
        ),
    }
}

/// Render a preview-semantics GLB for interactive viewers.
#[napi(js_name = "render_to_glb")]
#[allow(clippy::too_many_arguments)]
pub fn render_to_glb(
    source: String,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
    include_edges: bool,
) -> ExportShape3DResult {
    if ragged(
        &names,
        &values,
        &file_names,
        &file_contents,
        &bin_names,
        &bin_data,
    ) {
        return mismatched("glb");
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
    ExportShape3DResult {
        inner: openrscad_api::artifact_3d(
            &request,
            &openrscad_api::ExportOptions {
                format: "glb".to_string(),
                include_edges,
                preview: true,
                ..openrscad_api::ExportOptions::default()
            },
        ),
    }
}

/// Render a 2D model to DXF or SVG text (empty when the model isn't 2D).
#[napi(js_name = "export_2d")]
#[allow(clippy::too_many_arguments)]
pub fn export_2d(
    source: String,
    names: Vec<String>,
    values: Vec<String>,
    file_names: Vec<String>,
    file_contents: Vec<String>,
    bin_names: Vec<String>,
    bin_data: Vec<String>,
    font_blobs: Vec<String>,
    format: String,
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
    openrscad_api::export_2d(&request, &format)
}

/// Run the full pipeline, resolving `include`/`use` against the supplied maps.
#[napi(js_name = "render_with_files")]
#[allow(clippy::too_many_arguments)]
pub fn render_with_files(
    source: String,
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
    RenderResult {
        inner: openrscad_api::render(&request, false),
    }
}

/// The customizer parameter schema for a source string, as a JSON string.
#[napi]
pub fn parameters(source: String) -> String {
    openrscad_api::parameters(&source)
}

/// Engine version string.
#[napi]
pub fn version() -> String {
    openrscad_api::version()
}

/// Drop the persistent geometry cache (e.g. when loading a new document).
#[napi(js_name = "clear_cache")]
pub fn clear_cache() {
    openrscad_api::clear_cache();
}
