// Unit cover for the release-assembly gates. The gates also run against real
// assembled trees in CI; these cases pin the rejections that are expensive to
// discover only there — a wrong selector, a mis-suffixed binary, two targets
// shipping the same bytes.
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, describe, it } from "node:test";

import { preparedReleaseFindings } from "./check-prepared-release.mjs";
import { binaryFindings, inventoryFindings, parsePeHeader } from "./inspect-native.mjs";
import { readNapiTargets } from "./lib/napi-targets.mjs";
import { packFindings } from "./lib/package-files.mjs";

const PACKAGE_JSON = new URL("../packages/npm/package.json", import.meta.url);
const { manifest, packages } = readNapiTargets(PACKAGE_JSON);

const temporary = [];
after(() => {
  for (const directory of temporary) rmSync(directory, { force: true, recursive: true });
});

/**
 * Build a minimal prepared tree that the gate should accept, so each case only
 * has to break one thing.
 *
 * @param {(tree: { npmDir: string, root: string }) => void} [mutate] - Optional damage.
 * @returns {string[]} The gate's findings.
 */
const findingsFor = (mutate) => {
  const root = mkdtempSync(join(tmpdir(), "openrscad-prepared-"));
  temporary.push(root);
  const npmDir = join(root, "npm");
  mkdirSync(join(root, "dist", "native"), { recursive: true });
  writeFileSync(join(root, "dist", "native", "index.js"), "// generated loader\n");
  writeFileSync(
    join(root, "package.json"),
    JSON.stringify({
      engines: manifest.engines,
      license: manifest.license,
      name: manifest.name,
      napi: manifest.napi,
      optionalDependencies: Object.fromEntries(
        packages.map((target) => [target.name, manifest.version]),
      ),
      version: manifest.version,
    }),
  );
  for (const target of packages) {
    const directory = join(npmDir, target.suffix);
    mkdirSync(directory, { recursive: true });
    writeFileSync(
      join(directory, "package.json"),
      JSON.stringify({
        cpu: [target.cpu],
        engines: manifest.engines,
        files: [target.binary],
        license: manifest.license,
        main: target.binary,
        name: target.name,
        os: [target.os],
        version: manifest.version,
        ...(target.libc ? { libc: [target.libc] } : {}),
      }),
    );
    writeFileSync(join(directory, target.binary), target.suffix);
    writeFileSync(join(directory, "LICENSE.APACHE"), "Apache-2.0\n");
    writeFileSync(join(directory, "LICENSE.MIT"), "MIT\n");
  }
  mutate?.({ npmDir, root });
  return preparedReleaseFindings({ packedFiles: ["package.json", ...REQUIRED], root });
};

const REQUIRED = [
  "LICENSE-APACHE",
  "LICENSE-MIT",
  "README.md",
  "dist/browser.js",
  "dist/core.js",
  "dist/native/index.js",
  "dist/node.js",
  "pkg/node/openrscad.js",
  "pkg/web/openrscad.js",
  "pkg/web/openrscad_bg.wasm",
];

describe("target authority", () => {
  it("should derive the five published platform packages from napi.targets", () => {
    // Checked against a real `napi create-npm-dirs --npm-dir npm` run on
    // 2026-09-05 with @napi-rs/cli 3.8.6: same directory names, same selectors.
    assert.deepEqual(
      packages.map(({ cpu, libc, name, os, suffix }) => ({ cpu, libc, name, os, suffix })),
      [
        { cpu: "arm64", libc: undefined, name: `${manifest.napi.packageName}-darwin-arm64`, os: "darwin", suffix: "darwin-arm64" },
        { cpu: "x64", libc: undefined, name: `${manifest.napi.packageName}-darwin-x64`, os: "darwin", suffix: "darwin-x64" },
        { cpu: "x64", libc: "glibc", name: `${manifest.napi.packageName}-linux-x64-gnu`, os: "linux", suffix: "linux-x64-gnu" },
        { cpu: "arm64", libc: "glibc", name: `${manifest.napi.packageName}-linux-arm64-gnu`, os: "linux", suffix: "linux-arm64-gnu" },
        { cpu: "x64", libc: undefined, name: `${manifest.napi.packageName}-win32-x64-msvc`, os: "win32", suffix: "win32-x64-msvc" },
      ],
    );
  });
});

describe("completeness gate", () => {
  it("should accept a complete prepared tree", () => {
    assert.deepEqual(findingsFor(), []);
  });

  it("should reject a missing binary", () => {
    const findings = findingsFor(({ npmDir }) =>
      rmSync(join(npmDir, "linux-x64-gnu", "openrscad.linux-x64-gnu.node")),
    );
    assert.deepEqual(findings, ["linux-x64-gnu: npm/linux-x64-gnu/openrscad.linux-x64-gnu.node is missing"]);
  });

  it("should reject a stray binary beside the right one", () => {
    const findings = findingsFor(({ npmDir }) =>
      writeFileSync(join(npmDir, "darwin-arm64", "openrscad.darwin-x64.node"), "stray"),
    );
    assert.deepEqual(findings, [
      "darwin-arm64: npm/darwin-arm64/openrscad.darwin-x64.node is not the darwin-arm64 binary",
    ]);
  });

  it("should reject a wrong cpu selector and an unknown target package", () => {
    const findings = findingsFor(({ npmDir }) => {
      const path = join(npmDir, "win32-x64-msvc", "package.json");
      const overwritten = JSON.parse(readFileSync(path, "utf8"));
      overwritten.cpu = ["ia32"];
      writeFileSync(path, JSON.stringify(overwritten));
      mkdirSync(join(npmDir, "linux-x64-musl"));
    });
    assert.deepEqual(findings, [
      "npm/linux-x64-musl is not a configured target package",
      'win32-x64-msvc: expected cpu ["x64"], found ["ia32"]',
    ]);
  });

  it("should reject a root that lost the loader or gained an optional dependency", () => {
    const findings = findingsFor(({ root }) => {
      rmSync(join(root, "dist", "native", "index.js"));
      const path = join(root, "package.json");
      const overwritten = JSON.parse(readFileSync(path, "utf8"));
      overwritten.optionalDependencies["@taulabs/openrscad-engine-linux-x64-musl"] = manifest.version;
      writeFileSync(path, JSON.stringify(overwritten));
    });
    assert.deepEqual(findings, [
      "root optionalDependencies: @taulabs/openrscad-engine-linux-x64-musl is not a configured target package",
      "dist/native/index.js is missing",
    ]);
  });
});

describe("root tarball contract", () => {
  it("should reject an addon, the loader declarations and source in the root tarball", () => {
    assert.deepEqual(
      packFindings([
        ...REQUIRED,
        "package.json",
        "openrscad.darwin-arm64.node",
        "dist/native/index.d.ts",
        "src/node.ts",
      ]),
      [
        "root pack: openrscad.darwin-arm64.node must not ship",
        "root pack: dist/native/index.d.ts must not ship",
        "root pack: src/node.ts must not ship",
      ],
    );
  });
});

describe("binary inspection", () => {
  const target = packages.find((candidate) => candidate.suffix === "linux-x64-gnu");
  const clean = {
    class: "64-bit",
    endianness: "LittleEndian",
    format: "elf",
    glibcMax: "2.17",
    machine: "EM_X86_64",
    needed: ["libc.so.6", "libgcc_s.so.1", "ld-linux-x86-64.so.2"],
    programHeaders: ["PT_LOAD", "PT_DYNAMIC"],
  };

  it("should accept a linux x64 addon built against the pinned image", () => {
    assert.deepEqual(binaryFindings(target, clean), []);
  });

  it("should reject a slice whose machine, glibc or dependencies do not match", () => {
    assert.deepEqual(
      binaryFindings(target, {
        ...clean,
        glibcMax: "2.41",
        machine: "EM_AARCH64",
        needed: [...clean.needed, "libvulkan.so.1"],
      }),
      [
        "linux-x64-gnu: expected machine EM_X86_64, found EM_AARCH64",
        "linux-x64-gnu: requires GLIBC_2.41, above the 2.39 ceiling",
        "linux-x64-gnu: unexpected dynamic dependency libvulkan.so.1",
      ],
    );
  });

  it("should reject a Mach-O slice below its deployment floor", () => {
    const darwin = packages.find((candidate) => candidate.suffix === "darwin-x64");
    assert.deepEqual(
      binaryFindings(
        darwin,
        { class: "64-bit", format: "macho", machine: "X86-64", minOs: "10.12", needed: [], platform: "macos" },
        { macosDeploymentTarget: "10.13" },
      ),
      ["darwin-x64: expected LC_BUILD_VERSION minos 10.13, found 10.12"],
    );
  });

  it("should reject two targets that ship the same bytes", () => {
    const findings = inventoryFindings({
      inventory: {
        "darwin-arm64": { sha256: "abc" },
        "darwin-x64": { sha256: "abc" },
        "linux-arm64-gnu": { sha256: "d" },
        "linux-x64-gnu": { sha256: "e" },
        "win32-x64-msvc": { sha256: "f" },
      },
      packages,
      stray: ["packages/npm/openrscad.darwin-arm64.node"],
    });
    assert.deepEqual(findings, [
      "packages/npm/openrscad.darwin-arm64.node is not a configured target binary",
      "darwin-arm64, darwin-x64: share the identical binary abc",
    ]);
  });

  it("should read no subsystem version rather than an unusable one", () => {
    assert.equal(parsePeHeader("AddressSize: 64bit\n  Machine: IMAGE_FILE_MACHINE_AMD64\n").subsystemVersion, null);
  });
});

it("singlePackedTarball accepts the npm 11 array and the npm 12 object shapes", async () => {
  const { singlePackedTarball } = await import("./lib/npm-pack.mjs");
  const tarball = { filename: "x-1.0.0.tgz", integrity: "sha512-a", name: "x", version: "1.0.0" };
  assert.deepEqual(singlePackedTarball([tarball], "dir"), tarball);
  assert.deepEqual(singlePackedTarball({ x: tarball }, "dir"), tarball);
  for (const bad of [[], [tarball, tarball], {}, { x: tarball, y: tarball }, null, "x"]) {
    assert.throws(() => singlePackedTarball(bad, "dir"), /exactly one tarball in dir/);
  }
});
