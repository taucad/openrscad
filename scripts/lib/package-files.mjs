// The root tarball's file contract. Rule-based rather than a literal inventory:
// this package ships a whole wasm build (`pkg/web`, `pkg/node`) and an examples
// tree whose file count moves with the engine, so a frozen list would be noise.
// What must never move is the boundary — the generated NAPI-RS loader ships, the
// addons and the loader's declarations do not, and the entry points the export
// map names are all present.
//
// Imports no package: the publish job validates the extracted frozen tree with
// this file and installs nothing of its own.

/** Entries the `exports` map resolves, which a tarball missing one would break. */
export const REQUIRED_FILES = [
  "LICENSE-APACHE",
  "LICENSE-MIT",
  "README.md",
  "dist/browser.js",
  "dist/core.js",
  "dist/native/index.js",
  "dist/node.js",
  "package.json",
  "pkg/node/openrscad.js",
  "pkg/web/openrscad.js",
  "pkg/web/openrscad_bg.wasm",
];

/**
 * Findings against one root tarball's packed file list.
 *
 * `napi artifacts` copies every addon into the package root as well as into its
 * platform directory (policy Known Limitations), and the loader's declarations
 * are a build input: neither may reach the root tarball, and `src/` is source.
 *
 * @param {string[]} files - Packed paths, in any separator style.
 * @returns {string[]} One finding per violation.
 */
export const packFindings = (files) => {
  const normalized = files.map((file) => file.replaceAll("\\", "/"));
  const findings = normalized
    .filter(
      (file) =>
        file.endsWith(".node") || file === "dist/native/index.d.ts" || file.startsWith("src/"),
    )
    .map((file) => `root pack: ${file} must not ship`);
  for (const required of REQUIRED_FILES) {
    if (!normalized.includes(required)) findings.push(`root pack: ${required} is missing`);
  }
  return findings;
};
