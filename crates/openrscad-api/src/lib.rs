//! The engine pipeline, expressed once in plain Rust.
//!
//! `parse → evaluate → geometry → serialize` plus the two caches that make warm
//! edits cheap, taking and returning ordinary Rust types. Every host — the
//! wasm-bindgen surface in `openrscad-wasm`, the N-API addon in
//! `openrscad-napi` — is marshalling over this crate and nothing else, so the
//! two builds cannot drift into different geometry.
//!
//! Nothing here touches the filesystem: callers hand in the `include`/`use`
//! sources, the binary `import()` assets and the font files as maps, exactly as
//! the browser does. A native host therefore inherits the same virtual
//! filesystem the browser has, and `text()` sees the same fonts on both.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
#[cfg(feature = "benchmark-profile")]
use web_time::Instant;

thread_local! {
    /// Persistent geometry cache across renders — makes warm edits incremental
    /// (only subtrees whose structure changed are re-rendered). A host worker is
    /// single-threaded, so a thread-local is the whole story.
    static CACHE: RefCell<openrscad_geom::GeomCache> = RefCell::new(openrscad_geom::GeomCache::new());
    /// One complete attributed result, shared by GLB and 3MF serialization.
    /// Serialized artifacts and request-only edge pairs are never retained.
    static STRUCTURED_CACHE: RefCell<Option<(StructuredCacheKey, openrscad_geom::StructuredMesh, openrscad_geom::RenderDiagnostics)>> = const { RefCell::new(None) };
    /// How many times the structured render actually ran (i.e. missed the cache).
    /// Always compiled: one `Cell` increment per render is free, and hosts test
    /// their cache behaviour through it.
    static STRUCTURED_RENDER_COUNT: Cell<usize> = const { Cell::new(0) };
    #[cfg(feature = "benchmark-profile")]
    static LAST_BENCHMARK_PROFILE: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Bound on cached subtrees; past this the cache is reset to cap memory.
const CACHE_CAP: usize = 8192;

/// Engine version string.
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Drop the persistent geometry and structured caches (e.g. on a new document).
pub fn clear_cache() {
    CACHE.with(|cache| cache.borrow_mut().clear());
    STRUCTURED_CACHE.with(|cache| *cache.borrow_mut() = None);
}

/// The customizer parameter schema for a source string, as a JSON string
/// (`{"params":[…]}`).
pub fn parameters(source: &str) -> String {
    openrscad_syntax::customizer::extract(source).to_json()
}

/// Number of structured renders that missed the cache since the last reset.
pub fn structured_render_count() -> usize {
    STRUCTURED_RENDER_COUNT.with(Cell::get)
}

/// Reset [`structured_render_count`].
pub fn reset_structured_render_count() {
    STRUCTURED_RENDER_COUNT.with(|count| count.set(0));
}

/// Register font files (`.ttf`/`.otf`/`.ttc` bytes) into the engine's shared
/// font database so `text(font="…")` can use them. Identical files are deduped
/// engine-side, so re-sending a model's fonts every render is cheap.
pub fn register_font_files(fonts: &[Vec<u8>]) {
    for bytes in fonts {
        openrscad_eval::register_font_data(bytes.clone());
    }
}

/// Return and clear the last profile produced by a benchmark-feature build.
#[cfg(feature = "benchmark-profile")]
pub fn take_last_benchmark_profile() -> String {
    LAST_BENCHMARK_PROFILE.with(|last| std::mem::take(&mut *last.borrow_mut()))
}

#[cfg(feature = "benchmark-profile")]
fn store_benchmark_profile(profile: serde_json::Value) {
    LAST_BENCHMARK_PROFILE.with(|last| *last.borrow_mut() = profile.to_string());
}

#[cfg(all(feature = "benchmark-profile", target_arch = "wasm32"))]
fn wasm_memory_bytes() -> u32 {
    (core::arch::wasm32::memory_size(0) as u32).saturating_mul(65_536)
}

#[cfg(all(feature = "benchmark-profile", not(target_arch = "wasm32")))]
fn wasm_memory_bytes() -> u32 {
    0
}

/// One request's inputs. `params`, `files` and `binary_files` are association
/// lists rather than maps so a host never has to reproduce a map's iteration
/// order to get the same cache key — see [`StructuredCacheKey`].
#[derive(Default)]
pub struct Request {
    pub source: String,
    /// Customizer overrides: `(name, literal)`, applied in order.
    pub params: Vec<(String, String)>,
    /// `include`/`use` sources and text `import()` assets: `(path, contents)`.
    pub files: Vec<(String, String)>,
    /// Binary `import()` assets (binary STL, 3MF): `(path, bytes)`.
    pub binary_files: Vec<(String, Vec<u8>)>,
    /// Font files to register before evaluating `text()`.
    pub font_files: Vec<Vec<u8>>,
}

/// Serialization options for [`artifact_3d`].
pub struct ExportOptions {
    /// `stl` | `off` | `obj` | `3mf` | `amf` | `glb`.
    pub format: String,
    pub include_edges: bool,
    pub source_unit_to_meters: f64,
    /// `y-up` | `z-up`.
    pub coordinate_system: String,
    /// Preview semantics (the interactive `renderToGlb` path) rather than export.
    pub preview: bool,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: "glb".to_string(),
            include_edges: false,
            source_unit_to_meters: 0.001,
            coordinate_system: "y-up".to_string(),
            preview: false,
        }
    }
}

/// One owned 3D artifact plus the operational metadata hosts report.
#[derive(Default)]
pub struct Artifact3d {
    pub bytes: Vec<u8>,
    pub format: String,
    pub echo: String,
    pub warnings: String,
    pub error: Option<String>,
    pub geom_errors: String,
    pub diagnostics: String,
    pub viewport: String,
    pub triangle_count: u32,
    pub vertex_count: u32,
    pub volume: f64,
    pub area: f64,
    pub is_2d: bool,
}

impl Artifact3d {
    fn from_error(format: &str, message: String, diagnostics: String) -> Self {
        Self {
            format: format.to_string(),
            error: Some(message),
            diagnostics,
            ..Self::default()
        }
    }
}

/// A rendered mesh plus the preview/provenance channels the editor links against.
///
/// Mesh data is a non-indexed triangle soup with flat (per-face) normals:
/// `positions` and `normals` both hold 9 floats per triangle.
#[derive(Default)]
pub struct Render {
    pub positions: Vec<f32>,
    pub normals: Vec<f32>,
    pub echo: String,
    pub warnings: String,
    pub error: Option<String>,
    /// Recoverable geometry errors: newline-joined messages for CSG ops that
    /// failed and were replaced by a fallback mesh (e.g. non-manifold operands).
    pub geom_errors: String,
    /// Structured diagnostics (JSON array) for inline editor squiggles.
    pub diagnostics: String,
    /// Preview color channel (only populated when the model uses `color`/`#`/`%`).
    pub preview_positions: Vec<f32>,
    pub preview_normals: Vec<f32>,
    pub groups: String,
    /// Provenance channel for editor↔preview linking (2D and 3D alike).
    pub provenance_positions: Vec<f32>,
    pub provenance_normals: Vec<f32>,
    pub provenance: String,
    /// `$vp*` viewport variables as JSON (only when the source references `$vp`).
    pub viewport: String,
    pub triangle_count: u32,
    pub vertex_count: u32,
    pub volume: f64,
    pub area: f64,
    pub is_2d: bool,
}

impl Render {
    fn from_error(message: String, echo: String, warnings: String, diagnostics: String) -> Self {
        Self {
            error: Some(message),
            echo,
            warnings,
            diagnostics,
            ..Self::default()
        }
    }
}

/// Identity of a structured render. Every field is stored sorted so two hosts
/// that were handed the same inputs in a different order still hit the cache —
/// and, more importantly, produce the same key.
#[derive(Clone, PartialEq, Eq)]
struct StructuredCacheKey {
    source: String,
    params: Vec<(String, String)>,
    files: Vec<(String, String)>,
    binary_files: Vec<(String, Vec<u8>)>,
    font_files: Vec<Vec<u8>>,
    preview: bool,
}

impl StructuredCacheKey {
    fn new(request: &Request, preview: bool) -> Self {
        let sorted = |mut entries: Vec<(String, Vec<u8>)>| {
            entries.sort();
            entries
        };
        let mut params = request.params.clone();
        params.sort();
        let mut files = request.files.clone();
        files.sort();
        let mut font_files = request.font_files.clone();
        font_files.sort();
        Self {
            source: request.source.clone(),
            params,
            files,
            binary_files: sorted(request.binary_files.clone()),
            font_files,
            preview,
        }
    }
}

fn export_format(format: &str) -> Option<openrscad_geom::ExportFormat3D> {
    match format {
        "stl" => Some(openrscad_geom::ExportFormat3D::Stl),
        "off" => Some(openrscad_geom::ExportFormat3D::Off),
        "obj" => Some(openrscad_geom::ExportFormat3D::Obj),
        "3mf" => Some(openrscad_geom::ExportFormat3D::ThreeMf),
        "amf" => Some(openrscad_geom::ExportFormat3D::Amf),
        "glb" => Some(openrscad_geom::ExportFormat3D::Glb),
        _ => None,
    }
}

/// A `FileResolver` over in-memory maps: `path -> source` for `include`/`use`
/// (and text `import()` of DXF/SVG), plus `path -> bytes` for `import()` of
/// binary assets (binary STL, 3MF).
struct MapResolver {
    files: HashMap<String, String>,
    bins: HashMap<String, Vec<u8>>,
}

impl MapResolver {
    fn new(request: &Request) -> Self {
        Self {
            files: request.files.iter().cloned().collect(),
            bins: request.binary_files.iter().cloned().collect(),
        }
    }

    /// Resolve a path against a map's keys: as written, then normalized against
    /// the including dir. Shared by the source and binary maps.
    fn resolve_in<T>(map: &HashMap<String, T>, path: &str, from_dir: &str) -> Option<String> {
        if map.contains_key(path) {
            return Some(path.to_string());
        }
        let joined = if from_dir.is_empty() || from_dir == "." {
            path.to_string()
        } else {
            format!("{from_dir}/{path}")
        };
        map.contains_key(&joined).then_some(joined)
    }
}

impl openrscad_eval::FileResolver for MapResolver {
    fn load(&self, path: &str, from_dir: &str) -> Option<openrscad_eval::LoadedFile> {
        let key = Self::resolve_in(&self.files, path, from_dir)?;
        let source = self.files.get(&key)?.clone();
        let dir = key
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default();
        Some(openrscad_eval::LoadedFile {
            key: key.clone(),
            source,
            dir,
        })
    }

    /// Bytes for `import()`. Binary assets (STL/3MF) carried in the `bins` map
    /// win; otherwise fall back to a text file's bytes (a DXF/SVG profile).
    fn load_bytes(&self, path: &str, from_dir: &str) -> Option<Vec<u8>> {
        if let Some(key) = Self::resolve_in(&self.bins, path, from_dir) {
            return self.bins.get(&key).cloned();
        }
        let key = Self::resolve_in(&self.files, path, from_dir)?;
        self.files
            .get(&key)
            .map(|source| source.clone().into_bytes())
    }
}

fn overrides(request: &Request) -> Vec<(String, openrscad_eval::Value)> {
    request
        .params
        .iter()
        .filter_map(|(name, value)| {
            openrscad_syntax::customizer::parse_value(value)
                .map(|value| (name.clone(), openrscad_eval::value_from_param(&value)))
        })
        .collect()
}

/// Evaluate a 3D source and serialize it natively to one owned artifact.
pub fn artifact_3d(request: &Request, options: &ExportOptions) -> Artifact3d {
    #[cfg(feature = "benchmark-profile")]
    openrscad_geom::reset_benchmark_profile();
    #[cfg(feature = "benchmark-profile")]
    let profile_started = Instant::now();
    #[cfg(feature = "benchmark-profile")]
    let memory_before = wasm_memory_bytes();
    let format = options.format.as_str();
    let preview = options.preview;
    let Some(export_format) = export_format(format) else {
        return Artifact3d::from_error(
            format,
            format!("unsupported 3D export format: {format}"),
            "[]".to_string(),
        );
    };
    let coordinate_system = match options.coordinate_system.as_str() {
        "y-up" => openrscad_geom::CoordinateSystem::YUp,
        "z-up" => openrscad_geom::CoordinateSystem::ZUp,
        other => {
            return Artifact3d::from_error(
                format,
                format!("unsupported coordinate system: {other}"),
                "[]".to_string(),
            );
        }
    };
    let structured_key = StructuredCacheKey::new(request, preview);
    register_font_files(&request.font_files);
    #[cfg(feature = "benchmark-profile")]
    let parse_started = Instant::now();
    let program = match openrscad_syntax::parse(&request.source) {
        Ok(program) => program,
        Err(error) => {
            let message = format!("parse error: {}", error.message);
            let diagnostic = openrscad_eval::parse_error_diagnostic(message.clone(), error.span);
            return Artifact3d::from_error(
                format,
                message,
                openrscad_eval::diagnostics_json(Some(&diagnostic), &[]),
            );
        }
    };
    #[cfg(feature = "benchmark-profile")]
    let parse_ms = parse_started.elapsed().as_secs_f64() * 1_000.0;
    let overrides = overrides(request);
    let resolver = MapResolver::new(request);
    #[cfg(feature = "benchmark-profile")]
    let evaluate_started = Instant::now();
    let structured_format = matches!(
        export_format,
        openrscad_geom::ExportFormat3D::Glb | openrscad_geom::ExportFormat3D::ThreeMf
    );
    let eval = match if preview {
        openrscad_eval::eval_program_with_params_detailed(&program, &resolver, ".", &overrides)
    } else if structured_format {
        openrscad_eval::eval_program_with_params_detailed_export(
            &program, &resolver, ".", &overrides,
        )
    } else {
        openrscad_eval::eval_program_with_params_export(&program, &resolver, ".", &overrides)
    } {
        Ok(output) => output,
        Err(error) => {
            let diagnostic = openrscad_eval::eval_error_diagnostic(&error);
            return Artifact3d::from_error(
                format,
                format!("evaluation error: {}", error.message),
                openrscad_eval::diagnostics_json(Some(&diagnostic), &[]),
            );
        }
    };
    #[cfg(feature = "benchmark-profile")]
    let evaluate_ms = evaluate_started.elapsed().as_secs_f64() * 1_000.0;
    let diagnostics = openrscad_eval::diagnostics_json(None, &eval.warnings);
    let mut warnings = eval
        .warnings
        .iter()
        .map(|warning| warning.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let echo = eval.echoes.join("\n");
    let viewport = if request.source.contains("$vp") {
        openrscad_eval::viewport_json(&eval.viewport)
    } else {
        String::new()
    };
    if openrscad_geom::is_2d(&eval.node) {
        let mut result = Artifact3d::from_error(
            format,
            "3D export requested for a 2D model".to_string(),
            diagnostics,
        );
        result.echo = echo;
        result.warnings = warnings;
        result.viewport = viewport;
        result.is_2d = true;
        return result;
    }

    let geom_options = openrscad_geom::Export3DOptions {
        include_edges: options.include_edges,
        source_unit_to_meters: options.source_unit_to_meters,
        coordinate_system,
        source_keys: eval.source_keys,
    };
    let kernel = openrscad_geom::RustManifoldKernel::new();
    #[cfg(feature = "benchmark-profile")]
    let structured_started = Instant::now();
    #[cfg(feature = "benchmark-profile")]
    let mut structured_cache_hit = false;
    #[cfg(feature = "benchmark-profile")]
    let mut serialization_ms = 0.0;
    let artifact = match export_format {
        openrscad_geom::ExportFormat3D::Glb | openrscad_geom::ExportFormat3D::ThreeMf => {
            STRUCTURED_CACHE.with(|structured_cache| {
                let mut structured_cache = structured_cache.borrow_mut();
                if structured_cache
                    .as_ref()
                    .is_none_or(|(key, _, _)| key != &structured_key)
                {
                    let (structured, geometry_diagnostics) = CACHE.with(|cache| {
                        let mut cache = cache.borrow_mut();
                        if cache.len() > CACHE_CAP {
                            cache.clear();
                        }
                        openrscad_geom::render_structured_cached_diag(
                            &eval.node, &kernel, &mut cache, preview,
                        )
                    })?;
                    STRUCTURED_RENDER_COUNT.with(|count| count.set(count.get() + 1));
                    *structured_cache = Some((structured_key, structured, geometry_diagnostics));
                } else {
                    #[cfg(feature = "benchmark-profile")]
                    {
                        structured_cache_hit = true;
                    }
                }
                let structured = &structured_cache.as_ref().unwrap().1;
                let geometry_diagnostics = structured_cache.as_ref().unwrap().2.clone();
                #[cfg(feature = "benchmark-profile")]
                let serialization_started = Instant::now();
                let result = openrscad_geom::export_3d(structured, export_format, &geom_options);
                #[cfg(feature = "benchmark-profile")]
                {
                    serialization_ms = serialization_started.elapsed().as_secs_f64() * 1_000.0;
                }
                Ok((result, geometry_diagnostics))
            })
        }
        _ => CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            if cache.len() > CACHE_CAP {
                cache.clear();
            }
            let (mesh, geometry_diagnostics) =
                openrscad_geom::render_cached_diag(&eval.node, &kernel, &mut cache)?;
            #[cfg(feature = "benchmark-profile")]
            let serialization_started = Instant::now();
            let bytes = match export_format {
                openrscad_geom::ExportFormat3D::Stl => mesh.to_binary_stl(),
                openrscad_geom::ExportFormat3D::Off => mesh.to_off().into_bytes(),
                openrscad_geom::ExportFormat3D::Obj => mesh.to_obj().into_bytes(),
                openrscad_geom::ExportFormat3D::Amf => mesh.to_amf().into_bytes(),
                _ => unreachable!("structured formats are handled above"),
            };
            let result = Ok(openrscad_geom::Export3DArtifact {
                bytes,
                triangle_count: mesh.tris.len(),
                vertex_count: mesh.verts.len(),
                volume: mesh.volume(),
                surface_area: mesh.surface_area(),
            });
            #[cfg(feature = "benchmark-profile")]
            {
                serialization_ms = serialization_started.elapsed().as_secs_f64() * 1_000.0;
            }
            Ok((result, geometry_diagnostics))
        }),
    };
    let (artifact, geometry_diagnostics) =
        artifact.unwrap_or_else(|error| (Err(error), openrscad_geom::RenderDiagnostics::default()));
    #[cfg(feature = "benchmark-profile")]
    {
        let geom = openrscad_geom::take_benchmark_profile();
        let payload_bytes = artifact.as_ref().map_or(0, |artifact| artifact.bytes.len());
        let cache_entries = CACHE.with(|cache| cache.borrow().len());
        store_benchmark_profile(serde_json::json!({
            "path": if preview { "renderToGlb" } else { "exportShape3D" },
            "format": format,
            "includeEdges": options.include_edges,
            "parseMs": parse_ms,
            "evaluateMs": evaluate_ms,
            "structuredTotalMs": structured_started.elapsed().as_secs_f64() * 1_000.0,
            "structuredGeometryMs":
                (structured_started.elapsed().as_secs_f64() * 1_000.0 - serialization_ms).max(0.0),
            "attributedRenderMs": geom.attributed_render_ms,
            "booleanMs": geom.boolean_ms,
            "attributedTopologyMs": (geom.attributed_render_ms - geom.boolean_ms).max(0.0),
            "partitionMs": geom.partition_ms,
            "edgeDerivationMs": geom.edge_derivation_ms,
            "serializationMs": serialization_ms,
            "rustTotalMs": profile_started.elapsed().as_secs_f64() * 1_000.0,
            "featureLineCount": geom.feature_line_count,
            "payloadBytes": payload_bytes,
            "structuredCacheHit": structured_cache_hit,
            "geometryCacheEntries": cache_entries,
            "wasmMemoryBeforeBytes": memory_before,
            "wasmMemoryAfterBytes": wasm_memory_bytes(),
        }));
    }
    for warning in &geometry_diagnostics.warnings {
        if !warnings.is_empty() {
            warnings.push('\n');
        }
        warnings.push_str(warning);
    }
    let geom_errors = geometry_diagnostics.errors.join("\n");
    match artifact {
        Ok(artifact) => Artifact3d {
            bytes: artifact.bytes,
            format: format.to_string(),
            echo,
            warnings,
            error: None,
            geom_errors,
            diagnostics,
            viewport,
            triangle_count: artifact.triangle_count as u32,
            vertex_count: artifact.vertex_count as u32,
            volume: artifact.volume,
            area: artifact.surface_area,
            is_2d: false,
        },
        Err(error) => {
            let diagnostic = openrscad_eval::eval_error_diagnostic(
                &openrscad_eval::EvalError::new(format!("geometry error: {error}")),
            );
            let mut result = Artifact3d::from_error(
                format,
                format!("geometry error: {error}"),
                openrscad_eval::diagnostics_json(Some(&diagnostic), &eval.warnings),
            );
            result.echo = echo;
            result.warnings = warnings;
            result.geom_errors = geom_errors;
            result.viewport = viewport;
            result
        }
    }
}

/// Run the full pipeline and return a triangle soup plus the editor channels.
///
/// `preview` selects the fast, **non-watertight** path (see
/// `openrscad_geom::render_preview_cached_diag`): unions are concatenated rather
/// than run through the CSG kernel. Suitable for opaque on-screen display only —
/// stats and export still use the exact path. Differences/intersections/hulls
/// still resolve exactly, so holes and clips look correct.
pub fn render(request: &Request, preview: bool) -> Render {
    #[cfg(feature = "benchmark-profile")]
    openrscad_geom::reset_benchmark_profile();
    #[cfg(feature = "benchmark-profile")]
    let profile_started = Instant::now();
    #[cfg(feature = "benchmark-profile")]
    let memory_before = wasm_memory_bytes();
    // Register any host-supplied system fonts before evaluating `text()`.
    register_font_files(&request.font_files);

    #[cfg(feature = "benchmark-profile")]
    let parse_started = Instant::now();
    let program = match openrscad_syntax::parse(&request.source) {
        Ok(program) => program,
        Err(error) => {
            let message = format!("parse error: {}", error.message);
            let diagnostic = openrscad_eval::parse_error_diagnostic(message.clone(), error.span);
            return Render::from_error(
                message,
                String::new(),
                String::new(),
                openrscad_eval::diagnostics_json(Some(&diagnostic), &[]),
            );
        }
    };
    #[cfg(feature = "benchmark-profile")]
    let parse_ms = parse_started.elapsed().as_secs_f64() * 1_000.0;

    let overrides = overrides(request);
    let resolver = MapResolver::new(request);

    #[cfg(feature = "benchmark-profile")]
    let evaluate_started = Instant::now();
    let eval = match openrscad_eval::eval_program_with_params(&program, &resolver, ".", &overrides)
    {
        Ok(output) => output,
        Err(error) => {
            let diagnostic = openrscad_eval::eval_error_diagnostic(&error);
            return Render::from_error(
                format!("evaluation error: {}", error.message),
                String::new(),
                String::new(),
                openrscad_eval::diagnostics_json(Some(&diagnostic), &[]),
            );
        }
    };
    #[cfg(feature = "benchmark-profile")]
    let evaluate_ms = evaluate_started.elapsed().as_secs_f64() * 1_000.0;
    let echo = eval.echoes.join("\n");
    let mut warnings = eval
        .warnings
        .iter()
        .map(|warning| warning.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let diagnostics = openrscad_eval::diagnostics_json(None, &eval.warnings);

    // Render geometry with the pure-Rust Manifold kernel, reusing the persistent
    // cache so unchanged subtrees survive across edits.
    let kernel = openrscad_geom::RustManifoldKernel::new();
    #[cfg(feature = "benchmark-profile")]
    let exact_render_started = Instant::now();
    let rendered = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > CACHE_CAP {
            cache.clear();
        }
        if preview {
            openrscad_geom::render_preview_cached_diag(&eval.node, &kernel, &mut cache)
        } else {
            openrscad_geom::render_cached_diag(&eval.node, &kernel, &mut cache)
        }
    });
    #[cfg(feature = "benchmark-profile")]
    let exact_render_ms = exact_render_started.elapsed().as_secs_f64() * 1_000.0;
    let (mesh, geometry_diagnostics) = match rendered {
        Ok(value) => value,
        Err(error) => {
            let geom_error = openrscad_eval::EvalError::new(format!("geometry error: {error}"));
            let diagnostic = openrscad_eval::eval_error_diagnostic(&geom_error);
            return Render::from_error(
                format!("geometry error: {error}"),
                echo,
                warnings,
                openrscad_eval::diagnostics_json(Some(&diagnostic), &eval.warnings),
            );
        }
    };
    // Fold non-fatal geometry warnings (e.g. non-convex minkowski) into the
    // console warnings stream.
    for warning in geometry_diagnostics.warnings {
        if !warnings.is_empty() {
            warnings.push('\n');
        }
        warnings.push_str(&warning);
    }
    // Recoverable geometry errors (a CSG op failed and the mesh is a fallback)
    // go on their own channel so the UI can raise a distinct, non-blocking alert
    // while still showing the degraded model.
    let geom_errors = geometry_diagnostics.errors.join("\n");

    #[cfg(feature = "benchmark-profile")]
    let mesh_encoding_started = Instant::now();
    let (positions, normals) = mesh.to_triangle_soup_f32();
    #[cfg(feature = "benchmark-profile")]
    let mesh_encoding_ms = mesh_encoding_started.elapsed().as_secs_f64() * 1_000.0;

    // Preview color channel — only for models that actually use color/`#`/`%`, so
    // plain models keep the fast single-mesh path (and the warm-edit budget).
    #[cfg(feature = "benchmark-profile")]
    let preview_channel_started = Instant::now();
    let (preview_positions, preview_normals, groups) =
        if openrscad_geom::has_display_attrs(&eval.node) {
            let groups = CACHE.with(|cache| {
                let mut cache = cache.borrow_mut();
                openrscad_geom::render_groups_cached(&eval.node, &kernel, &mut cache)
            });
            match groups {
                Ok(groups) => openrscad_geom::preview_channel(&groups),
                Err(_) => (Vec::new(), Vec::new(), String::new()),
            }
        } else {
            (Vec::new(), Vec::new(), String::new())
        };
    #[cfg(feature = "benchmark-profile")]
    let preview_channel_ms = preview_channel_started.elapsed().as_secs_f64() * 1_000.0;

    // Provenance channel for editor↔preview linking — any model with geometry
    // (2D flat meshes and 3D solids alike). Shares the cache with the fused
    // render above, so opaque leaf meshes aren't recomputed just to tag them
    // with a span.
    #[cfg(feature = "benchmark-profile")]
    let provenance_channel_started = Instant::now();
    let (provenance_positions, provenance_normals, provenance) = if mesh.tris.is_empty() {
        (Vec::new(), Vec::new(), String::new())
    } else {
        let groups = CACHE.with(|cache| {
            let mut cache = cache.borrow_mut();
            openrscad_geom::render_provenance_cached(&eval.node, &kernel, &mut cache)
        });
        match groups {
            Ok(groups) => openrscad_geom::provenance_channel(&groups),
            Err(_) => (Vec::new(), Vec::new(), String::new()),
        }
    };
    #[cfg(feature = "benchmark-profile")]
    let provenance_channel_ms = provenance_channel_started.elapsed().as_secs_f64() * 1_000.0;

    // Viewport channel only for models that reference `$vp` (drives the camera).
    let viewport = if request.source.contains("$vp") {
        openrscad_eval::viewport_json(&eval.viewport)
    } else {
        String::new()
    };

    #[cfg(feature = "benchmark-profile")]
    {
        let payload_bytes = (positions.len()
            + normals.len()
            + preview_positions.len()
            + preview_normals.len()
            + provenance_positions.len()
            + provenance_normals.len())
            * std::mem::size_of::<f32>()
            + groups.len()
            + provenance.len();
        let cache_entries = CACHE.with(|cache| cache.borrow().len());
        store_benchmark_profile(serde_json::json!({
            "path": "render",
            "parseMs": parse_ms,
            "evaluateMs": evaluate_ms,
            "exactRenderMs": exact_render_ms,
            "previewChannelMs": preview_channel_ms,
            "provenanceChannelMs": provenance_channel_ms,
            "meshEncodingMs": mesh_encoding_ms,
            "rustTotalMs": profile_started.elapsed().as_secs_f64() * 1_000.0,
            "payloadBytes": payload_bytes,
            "geometryCacheEntries": cache_entries,
            "wasmMemoryBeforeBytes": memory_before,
            "wasmMemoryAfterBytes": wasm_memory_bytes(),
        }));
    }

    Render {
        triangle_count: mesh.tris.len() as u32,
        vertex_count: mesh.verts.len() as u32,
        volume: mesh.volume(),
        area: mesh.surface_area(),
        is_2d: openrscad_geom::is_2d(&eval.node),
        positions,
        normals,
        echo,
        warnings,
        error: None,
        geom_errors,
        diagnostics,
        preview_positions,
        preview_normals,
        groups,
        provenance_positions,
        provenance_normals,
        provenance,
        viewport,
    }
}

/// Render a 2D model and serialize it to DXF or SVG text. Returns an empty
/// string if the model isn't 2D or fails to evaluate (the caller checks
/// [`Render::is_2d`] first). `format` is `dxf` or `svg`.
pub fn export_2d(request: &Request, format: &str) -> String {
    register_font_files(&request.font_files);
    let Ok(program) = openrscad_syntax::parse(&request.source) else {
        return String::new();
    };
    let overrides = overrides(request);
    let resolver = MapResolver::new(request);
    let Ok(eval) =
        openrscad_eval::eval_program_with_params_export(&program, &resolver, ".", &overrides)
    else {
        return String::new();
    };
    let kernel = openrscad_geom::RustManifoldKernel::new();
    match openrscad_geom::render_contours_with(&eval.node, &kernel) {
        Ok(Some(contours)) if format == "dxf" => openrscad_geom::export_dxf(&contours),
        Ok(Some(contours)) if format == "svg" => openrscad_geom::export_svg(&contours),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_insensitive_to_input_order() {
        let a = Request {
            source: "cube(1);".to_string(),
            params: vec![("x".into(), "1".into()), ("y".into(), "2".into())],
            files: vec![
                ("a.scad".into(), "//a".into()),
                ("b.scad".into(), "//b".into()),
            ],
            binary_files: vec![("a.stl".into(), vec![1]), ("b.stl".into(), vec![2])],
            font_files: vec![vec![3], vec![4]],
        };
        let b = Request {
            params: a.params.iter().rev().cloned().collect(),
            files: a.files.iter().rev().cloned().collect(),
            binary_files: a.binary_files.iter().rev().cloned().collect(),
            font_files: a.font_files.iter().rev().cloned().collect(),
            source: a.source.clone(),
        };
        assert!(StructuredCacheKey::new(&a, false) == StructuredCacheKey::new(&b, false));
        assert!(StructuredCacheKey::new(&a, false) != StructuredCacheKey::new(&a, true));
    }
}
