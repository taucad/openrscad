#!/usr/bin/env node
// Pack the root package and every generated platform package into one directory
// and record the integrity npm computed for each. The smoke rows install these
// exact bytes, and `registry-wait.mjs` later proves the registry serves the same
// ones, so the manifest travels inside the frozen tree too.
//
// Ported unchanged in shape from nanoraster's `scripts/pack-test-tarballs.mjs`.
import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { readNapiTargets } from "./lib/napi-targets.mjs";

const MANIFEST = "test-tarballs.json";

/** Pack one package directory into `destination` and report what npm wrote. */
export const npmPack = (directory, destination) => {
  const packed = JSON.parse(
    execFileSync("npm", ["pack", "--json", "--ignore-scripts", "--pack-destination", destination], {
      cwd: directory,
      encoding: "utf8",
    }),
  );
  if (!Array.isArray(packed) || packed.length !== 1) {
    throw new Error(`npm pack must describe exactly one tarball in ${directory}`);
  }
  const { filename, integrity, name, version } = packed[0];
  return { filename, integrity, name, version };
};

/**
 * Pack the frozen test tarballs.
 *
 * @param {{ npmDir?: string, out: string, pack?: typeof npmPack, root: string }} input - What to pack and where.
 * @returns {{ packages: Record<string, { filename: string, integrity: string, version: string }>, version: string }} The manifest written.
 */
export const packTestTarballs = ({ npmDir = "npm", out, pack = npmPack, root }) => {
  const rootDirectory = resolve(root);
  const outDirectory = resolve(out);
  const { manifest, packages } = readNapiTargets(join(rootDirectory, "package.json"));
  mkdirSync(outDirectory, { recursive: true });

  const sources = [
    { directory: rootDirectory, name: manifest.name },
    ...packages.map((target) => ({
      directory: join(rootDirectory, npmDir, target.suffix),
      name: target.name,
    })),
  ];

  const packed = sources.map(({ directory, name }) => {
    const entry = pack(directory, outDirectory);
    if (entry.name !== name) throw new Error(`${directory} packed ${entry.name}, expected ${name}`);
    if (entry.version !== manifest.version) {
      throw new Error(`${entry.name} packed version ${entry.version}, expected ${manifest.version}`);
    }
    return entry;
  });

  const output = {
    packages: Object.fromEntries(
      packed
        .sort((left, right) => (left.name < right.name ? -1 : 1))
        .map(({ filename, integrity, name, version }) => [name, { filename, integrity, version }]),
    ),
    version: manifest.version,
  };

  const json = `${JSON.stringify(output, null, 2)}\n`;
  writeFileSync(join(outDirectory, MANIFEST), json);
  writeFileSync(join(rootDirectory, MANIFEST), json);
  return output;
};

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({
    options: {
      "npm-dir": { default: "npm", type: "string" },
      out: { type: "string" },
      root: { default: fileURLToPath(new URL("../packages/npm", import.meta.url)), type: "string" },
    },
  });
  try {
    if (!values.out) throw new Error("expected --out <directory>");
    const output = packTestTarballs({ npmDir: values["npm-dir"], out: values.out, root: values.root });
    process.stdout.write(`packed ${Object.keys(output.packages).length} tarballs at ${output.version}\n`);
  } catch (error) {
    process.stderr.write(`::error::${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  }
}
