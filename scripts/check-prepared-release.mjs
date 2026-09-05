#!/usr/bin/env node
// The completeness gate (napi-architecture-policy §5). It runs after `napi
// pre-publish --skip-optional-publish` has reconciled the prepared tree and
// before any job holds `id-token: write`, and it rejects every failure mode the
// policy names: a missing, duplicate or unknown target package; a wrong name,
// version, `os`/`cpu`/`libc` selector; a missing or mis-suffixed binary; a
// missing licence, entry point, declared file or engine range; root optional
// dependencies that differ from the generated target set; a missing loader; and
// a root tarball carrying an addon.
//
// Ported from nanoraster's `scripts/check-prepared-release.mjs`. Two departures:
// the licence is this fork's dual `LICENSE.APACHE`/`LICENSE.MIT` pair (npm
// force-packs `LICENSE.*`, but not `LICENSE-*`, whatever `files` says), and the
// root file contract is rule-based (see `lib/package-files.mjs`).
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { readNapiTargets } from "./lib/napi-targets.mjs";
import { packFindings } from "./lib/package-files.mjs";
import { packedFiles } from "./validate-pack.mjs";

const LOADER = "dist/native/index.js";
const LOADER_DECLARATIONS = "dist/native/index.d.ts";
/** Copied into every generated platform package during assembly. */
export const LICENSE_FILES = ["LICENSE.APACHE", "LICENSE.MIT"];

const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

const same = (left, right) => JSON.stringify(left) === JSON.stringify(right);

const platformFindings = ({ directory, npmDir, rootManifest, target }) => {
  const relative = `${npmDir}/${target.suffix}`;
  const manifestPath = join(directory, "package.json");
  if (!existsSync(manifestPath)) return [`${target.suffix}: ${relative}/package.json is missing`];

  const findings = [];
  const note = (message) => findings.push(`${target.suffix}: ${message}`);
  const manifest = readJson(manifestPath);
  if (manifest.name !== target.name) note(`expected name ${target.name}, found ${manifest.name}`);
  if (manifest.version !== rootManifest.version) {
    note(`expected version ${rootManifest.version}, found ${manifest.version}`);
  }
  if (!same(manifest.os, [target.os])) {
    note(`expected os ${JSON.stringify([target.os])}, found ${JSON.stringify(manifest.os)}`);
  }
  if (!same(manifest.cpu, [target.cpu])) {
    note(`expected cpu ${JSON.stringify([target.cpu])}, found ${JSON.stringify(manifest.cpu)}`);
  }
  // Generation writes `libc` only for a literal `gnu` or `musl` ABI.
  if (target.libc && !same(manifest.libc, [target.libc])) {
    note(`expected libc ${JSON.stringify([target.libc])}, found ${JSON.stringify(manifest.libc)}`);
  }
  if (!target.libc && manifest.libc !== undefined) {
    note(`expected no libc selector, found ${JSON.stringify(manifest.libc)}`);
  }
  if (!same(manifest.engines, rootManifest.engines)) {
    note(`expected engines ${JSON.stringify(rootManifest.engines)}, found ${JSON.stringify(manifest.engines)}`);
  }
  if (manifest.main !== target.binary) note(`expected main ${target.binary}, found ${manifest.main}`);
  if (!same(manifest.files, [target.binary])) {
    note(`expected files ${JSON.stringify([target.binary])}, found ${JSON.stringify(manifest.files)}`);
  }
  if (manifest.license !== rootManifest.license) {
    note(`expected license ${rootManifest.license}, found ${manifest.license}`);
  }

  const entries = readdirSync(directory);
  if (!entries.includes(target.binary)) note(`${relative}/${target.binary} is missing`);
  for (const entry of entries.filter((name) => name.endsWith(".node") && name !== target.binary)) {
    note(`${relative}/${entry} is not the ${target.suffix} binary`);
  }
  for (const license of LICENSE_FILES) {
    if (!entries.includes(license)) note(`${relative}/${license} is missing`);
  }
  return findings;
};

const optionalDependencyFindings = (packages, declared, rootVersion) => {
  const findings = [];
  const expected = new Set(packages.map((target) => target.name));
  for (const name of expected) {
    if (!(name in declared)) findings.push(`root optionalDependencies: ${name} is missing`);
  }
  for (const [name, version] of Object.entries(declared)) {
    if (!expected.has(name)) {
      findings.push(`root optionalDependencies: ${name} is not a configured target package`);
    } else if (version !== rootVersion) {
      findings.push(`root optionalDependencies: expected ${name}@${rootVersion}, found ${String(version)}`);
    }
  }
  return findings;
};

/**
 * Assert one assembled release tree is complete.
 *
 * @param {{ npmDir?: string, packedFiles: string[], root: string }} input - Tree under test.
 * @returns {string[]} One finding per violation, empty when the tree is releasable.
 */
export const preparedReleaseFindings = ({ npmDir = "npm", packedFiles: packed, root }) => {
  const rootDirectory = resolve(root);
  const { manifest, packages } = readNapiTargets(join(rootDirectory, "package.json"));
  const npmDirectory = join(rootDirectory, npmDir);
  const findings = [];

  const configured = new Set(packages.map((target) => target.suffix));
  const present = existsSync(npmDirectory) ? readdirSync(npmDirectory).sort() : [];
  for (const entry of present.filter((name) => !configured.has(name))) {
    findings.push(`${npmDir}/${entry} is not a configured target package`);
  }

  for (const target of [...packages].sort((a, b) => (a.suffix < b.suffix ? -1 : 1))) {
    findings.push(
      ...platformFindings({
        directory: join(npmDirectory, target.suffix),
        npmDir,
        rootManifest: manifest,
        target,
      }),
    );
  }

  findings.push(...optionalDependencyFindings(packages, manifest.optionalDependencies ?? {}, manifest.version));

  if (!existsSync(join(rootDirectory, LOADER))) findings.push(`${LOADER} is missing`);
  if (existsSync(join(rootDirectory, LOADER_DECLARATIONS))) {
    findings.push(`${LOADER_DECLARATIONS} is a build input and must not ship`);
  }

  findings.push(...packFindings(packed));
  return findings;
};

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({
    options: {
      "npm-dir": { default: "npm", type: "string" },
      root: { default: fileURLToPath(new URL("../packages/npm", import.meta.url)), type: "string" },
    },
  });
  try {
    const root = resolve(values.root);
    const findings = preparedReleaseFindings({
      npmDir: values["npm-dir"],
      packedFiles: packedFiles(root),
      root,
    });
    for (const finding of findings) process.stderr.write(`::error::${finding}\n`);
    if (findings.length > 0) {
      process.stderr.write(`${findings.length} prepared release findings\n`);
      process.exit(1);
    }
    process.stdout.write("prepared release tree is complete\n");
  } catch (error) {
    process.stderr.write(`::error::${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  }
}
