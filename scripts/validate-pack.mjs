#!/usr/bin/env node
// Assert the root tarball's file contract, from a source checkout or from an
// extracted frozen release tree (`--root release`). The publish job runs this
// against the tree it is about to publish, before it holds any registry
// credential.
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { packFindings } from "./lib/package-files.mjs";

/** Pack one directory without running scripts and return its packed paths. */
export const packedFiles = (directory) => {
  const packed = JSON.parse(
    execFileSync("npm", ["pack", "--dry-run", "--json", "--ignore-scripts"], {
      cwd: directory,
      encoding: "utf8",
    }),
  );
  if (!Array.isArray(packed) || packed.length !== 1) {
    throw new Error(`npm pack must describe exactly one tarball in ${directory}`);
  }
  return packed[0].files.map(({ path }) => path);
};

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({
    options: { root: { default: fileURLToPath(new URL("../packages/npm", import.meta.url)), type: "string" } },
  });
  try {
    const files = packedFiles(values.root);
    const findings = packFindings(files);
    for (const finding of findings) process.stderr.write(`::error::${finding}\n`);
    if (findings.length > 0) {
      process.stderr.write(`${findings.length} root pack findings\n`);
      process.exit(1);
    }
    process.stdout.write(`root tarball contains ${files.length} files and honours the contract\n`);
  } catch (error) {
    process.stderr.write(`::error::${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  }
}
