#!/usr/bin/env node
// Clean-room runtime evidence for one platform package, and the byte-parity gate
// (blueprint G5, G9).
//
// The frozen root tarball plus the frozen platform tarball for
// `OPENRSCAD_NATIVE_SUFFIX` are installed into a temporary directory with
// `--omit=optional` — the frozen bytes are the only ones under test, and the
// root's optional dependencies do not exist on the registry until the release
// publishes. Then a child process proves, on this host:
//
//   1. the installed package's `node` entry bound the addon (`backend === 'native'`),
//   2. every built-in fixture exports byte-identically through the addon and
//      through the wasm build the same tarball ships (`./core`'s `makeApi()` over
//      the raw `./node` glue — the same facade the entry falls back to), and
//   3. with the platform package removed, the entry still renders and says
//      `backend === 'wasm'` (one row is enough; `OPENRSCAD_SMOKE_FALLBACK=1`).
//
// Two processes, because a binding is per-process: nothing can prove the wasm
// fallback in a process that already opened the addon.
//
// Imports no package: the smoke rows install no repository dependency.
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  builtInFixtures,
  nativeVsWasmParity,
} from "../../../benchmarks/export-shape3d-benchmark.mjs";
import { readNapiTargets } from "../../../scripts/lib/napi-targets.mjs";

const PACKAGE_JSON = new URL("../package.json", import.meta.url);
const FONT = new URL("../../../crates/openrscad-eval/fonts/LiberationSans-Regular.ttf", import.meta.url);
const INSTALL_FLAGS = ["--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock", "--omit=optional"];

// npm is a `.cmd` shim on Windows, which `execFileSync` can only start through a
// shell; quoting is then ours to do.
const windows = process.platform === "win32";
const quoteForShell = (value) => (windows && /[\s&|<>^"]/u.test(value) ? `"${value}"` : value);
const runNpm = (args, cwd) =>
  execFileSync(windows ? "npm.cmd" : "npm", args.map(quoteForShell), {
    cwd,
    shell: windows,
    stdio: "inherit",
  });

/**
 * Look up the frozen root and platform tarballs recorded for one suffix.
 *
 * A silently empty artifact download leaves the directory bare, and reading
 * straight through it reports an ENOENT the operator has to trace back to the
 * download. Name the directory instead, and say what it holds.
 *
 * @param {string} directory - Directory holding the frozen tarballs.
 * @param {string} platformName - Platform package this row smokes.
 * @param {string} rootName - Root package name.
 * @returns {{ platform: string, root: string, version: string }} Absolute tarball paths and the version.
 */
export const selectTarballs = (directory, platformName, rootName) => {
  const resolved = resolve(directory);
  if (!existsSync(resolved)) throw new Error(`no tarball directory: ${resolved}`);
  const landed = readdirSync(resolved);
  const manifestPath = join(resolved, "test-tarballs.json");
  if (!existsSync(manifestPath)) {
    throw new Error(`no test-tarballs.json in ${resolved}, which holds: ${landed.join(", ") || "nothing"}`);
  }
  const { packages, version } = JSON.parse(readFileSync(manifestPath, "utf8"));
  const tarballOf = (name) => {
    const entry = packages?.[name];
    if (!entry?.filename) throw new Error(`the frozen tarball manifest has no tarball for ${name}`);
    if (entry.version !== version) {
      throw new Error(`${name} is packed at ${String(entry.version)}, expected ${version}`);
    }
    const path = join(resolved, entry.filename);
    if (!existsSync(path)) throw new Error(`frozen tarball missing: ${path}`);
    return path;
  };
  return { platform: tarballOf(platformName), root: tarballOf(rootName), version };
};

/** The `text` fixture needs a real font; the rest are pure geometry. */
const fixtures = async () => {
  const font = await readFile(FONT);
  return builtInFixtures.map((fixture) =>
    fixture.name === "text" ? { ...fixture, options: { fontFiles: [font] } } : fixture,
  );
};

/**
 * The consumer half: assert which backend bound and, on the native row, that the
 * two engines the one tarball ships agree byte for byte.
 *
 * @param {string} packageDirectory - Installed package root.
 * @param {"native" | "wasm"} expected - Backend this phase requires.
 * @returns {Promise<void>} Resolves when the phase passed.
 */
const consume = async (packageDirectory, expected) => {
  const entry = await import(new URL("dist/node.js", `file://${packageDirectory.replaceAll("\\", "/")}/`));
  if (entry.backend !== expected) {
    throw new Error(
      `expected the ${expected} backend, bound ${entry.backend}; loader cause: ${String(entry.backendCause)}`,
    );
  }

  const rendered = await entry.render("cube(2);");
  if (!rendered.ok || rendered.triangleCount !== 12) {
    throw new Error(`the ${expected} backend did not render a cube: ${rendered.error ?? "no triangles"}`);
  }
  console.log(`${expected}: rendered cube(2) as ${rendered.triangleCount} triangles`);
  if (expected === "wasm") return;

  const { makeApi } = await import(
    new URL("dist/core.js", `file://${packageDirectory.replaceAll("\\", "/")}/`)
  );
  const glue = await import(
    new URL("pkg/node/openrscad.js", `file://${packageDirectory.replaceAll("\\", "/")}/`)
  );
  const parity = await nativeVsWasmParity({
    fixtures: await fixtures(),
    nativeApi: entry,
    wasmApi: makeApi(glue, () => Promise.resolve()),
  });
  if (!parity.ok) {
    throw new Error(`native/wasm divergence: ${JSON.stringify(parity.mismatches, null, 2)}`);
  }
  console.log(`native: ${parity.total} artifacts byte-identical to the wasm build`);
};

const main = async () => {
  const suffix = process.env["OPENRSCAD_NATIVE_SUFFIX"];
  const tarballDirectory = process.env["OPENRSCAD_TARBALL_DIR"];
  if (!suffix) throw new Error("OPENRSCAD_NATIVE_SUFFIX must name the platform package to smoke");
  if (!tarballDirectory) throw new Error("OPENRSCAD_TARBALL_DIR must name the frozen tarball directory");

  // The target authority, not a host-to-package map: the loader owns selection.
  const { manifest, packages } = readNapiTargets(PACKAGE_JSON);
  const target = packages.find((candidate) => candidate.suffix === suffix);
  if (!target) throw new Error(`napi.targets configures no ${manifest.napi.packageName}-${suffix}`);

  const selected = selectTarballs(tarballDirectory, target.name, manifest.name);
  const work = mkdtempSync(join(tmpdir(), "openrscad-smoke-"));
  try {
    writeFileSync(join(work, "package.json"), '{"private":true,"type":"module"}\n');
    runNpm(["install", ...INSTALL_FLAGS, selected.root, selected.platform], work);

    const installedRoot = join(work, "node_modules", ...manifest.name.split("/"));
    const installedVersion = JSON.parse(readFileSync(join(installedRoot, "package.json"), "utf8")).version;
    if (installedVersion !== selected.version) {
      throw new Error(`installed ${manifest.name}@${installedVersion}, expected ${selected.version}`);
    }
    const platformDirectory = join(work, "node_modules", ...target.name.split("/"));
    if (!existsSync(join(platformDirectory, target.binary))) {
      throw new Error(`${target.name} installed without ${target.binary}`);
    }

    const phase = (expected) =>
      execFileSync(process.execPath, [fileURLToPath(import.meta.url), "--consume", installedRoot, expected], {
        stdio: "inherit",
      });
    phase("native");

    // The other direction of G9, on the one row the workflow asks for it: with
    // no platform package installed the entry must keep the engine, through the
    // wasm build the same tarball ships.
    if (process.env["OPENRSCAD_SMOKE_FALLBACK"] === "1") {
      rmSync(platformDirectory, { force: true, recursive: true });
      phase("wasm");
    }

    console.log(
      `clean-room smoke passed: ${manifest.name}@${selected.version} through ${target.name} on ${process.platform}-${process.arch}`,
    );
  } finally {
    rmSync(work, { force: true, recursive: true });
  }
};

if (process.argv[2] === "--consume") {
  await consume(process.argv[3], process.argv[4]);
} else if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await main();
}
