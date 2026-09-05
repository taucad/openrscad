#!/usr/bin/env node
// Inspect every collected addon (napi-architecture-policy §5). `napi artifacts`
// matches binaries by filename suffix only and never opens them, so a build that
// wrote the wrong slice under the right name would ship. This reads each file
// with `llvm-readobj`/`llvm-objdump` from the toolchain `rust-toolchain.toml`
// pins and asserts format, machine, word size, endianness, dynamic dependencies,
// glibc ceiling and minimum OS against the target the filename claims — plus
// that no two targets share bytes.
//
// Ported from nanoraster's sixteen-target `scripts/inspect-native.mjs`, reduced
// to the five targets this package publishes: no Android note, no FreeBSD, no
// musl, no 32-bit or big-endian rows, no hard-float ARM flag.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

import { readNapiTargets } from "./lib/napi-targets.mjs";

const TOOL_OUTPUT_LIMIT = 64 * 1024 * 1024;
const WINDOWS_SUBSYSTEM_VERSION_FLOOR = "6.0";

// The Linux rows build natively on `ubuntu-24.04` / `ubuntu-24.04-arm`, whose
// glibc is 2.39 — this is a ceiling on what a consumer's glibc must provide, not
// the 2.17 floor a `--use-napi-cross` build would give. It exists so that moving
// the rows to a newer image cannot silently raise the requirement: raising it is
// a deliberate edit here, next to the runner labels it describes.
const GLIBC_CEILING = "2.39";

// `clang` clamps arm64 macOS to 11.0 whatever MACOSX_DEPLOYMENT_TARGET says, and
// Rust's x86_64-apple-darwin defaults to 10.12, so a pin below a slice's floor
// never lowers it.
const MACOS_SLICE_FLOOR = { "darwin-arm64": "11.0", "darwin-x64": "10.12" };

// What the toolchains actually link. A new entry is a deliberate admission
// diffed against the inventory this script prints, never a silent pass.
const DEPENDENCY_ALLOW_LIST = {
  darwin: [/^\/System\/Library\/Frameworks\//u, /^\/usr\/lib\//u],
  glibc: [
    /^ld-linux-[\w-]+\.so\.\d+$/u,
    "libc.so.6",
    "libdl.so.2",
    "libgcc_s.so.1",
    "libm.so.6",
    "libpthread.so.0",
    "librt.so.1",
  ],
  windows: [
    /^api-ms-win-[\w-]+\.dll$/u,
    "advapi32.dll",
    "bcrypt.dll",
    // std's random source (observed on the nanoraster MSVC rows, 2026-08-22).
    "bcryptprimitives.dll",
    "dbghelp.dll",
    "kernel32.dll",
    "msvcp140.dll",
    // An N-API addon imports the Node symbols it calls from the host executable.
    "node.exe",
    "ntdll.dll",
    "ole32.dll",
    "oleaut32.dll",
    "powrprof.dll",
    "synchronization.dll",
    "ucrtbase.dll",
    "user32.dll",
    "userenv.dll",
    "vcruntime140.dll",
    "vcruntime140_1.dll",
    "ws2_32.dll",
  ],
};

// `lld` marks a `/dll /noentry` image WINDOWS_GUI while the MSVC linker marks a
// Rust cdylib WINDOWS_CUI. Both are inert for a DLL, so both are admitted.
const WINDOWS_SUBSYSTEMS = ["IMAGE_SUBSYSTEM_WINDOWS_CUI", "IMAGE_SUBSYSTEM_WINDOWS_GUI"];

const ELF_MACHINE_BY_CPU = { arm64: "EM_AARCH64", x64: "EM_X86_64" };
const MACHO_CPU_TYPE_BY_CPU = { arm64: "Arm64", x64: "X86-64" };
const PE_MACHINE_BY_CPU = { arm64: "IMAGE_FILE_MACHINE_ARM64", x64: "IMAGE_FILE_MACHINE_AMD64" };

const byText = (left, right) => Number(left > right) - Number(left < right);

const expectedFormat = (target) =>
  target.os === "darwin" ? "macho" : target.os === "win32" ? "pe" : "elf";

const compareVersions = (left, right) => {
  const parse = (value) => String(value).split(".").map(Number);
  const [a, b] = [parse(left), parse(right)];
  for (let index = 0; index < Math.max(a.length, b.length); index += 1) {
    const difference = (a[index] ?? 0) - (b[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return 0;
};

const isVersion = (value) => typeof value === "string" && /^\d+(?:\.\d+)*$/u.test(value);

const admits = (entry, allowList, { caseInsensitive = false } = {}) => {
  const candidate = caseInsensitive ? entry.toLowerCase() : entry;
  return allowList.some((allowed) =>
    allowed instanceof RegExp ? allowed.test(candidate) : allowed === candidate,
  );
};

// --- Parsers: one tool invocation's text in, plain data out. ----------------

export const detectBinaryFormat = (bytes) => {
  if (bytes.length < 4) return null;
  const magic = bytes.subarray(0, 4).toString("hex");
  if (magic === "7f454c46") return "elf";
  if (["cffaedfe", "cefaedfe", "feedface", "feedfacf"].includes(magic)) return "macho";
  if (bytes.subarray(0, 2).toString("latin1") === "MZ") return "pe";
  return null;
};

export const parseElfHeader = (text) => {
  const header = text.slice(text.indexOf("ElfHeader {"));
  const read = (pattern) => pattern.exec(header)?.[1];
  const machine = read(/^ {2}Machine: (\S+)/mu);
  if (!machine) return null;
  return {
    class: read(/^ {4}Class: (\S+)/mu),
    endianness: read(/^ {4}DataEncoding: (\S+)/mu),
    machine,
  };
};

export const parseProgramHeaderTypes = (text) =>
  [...text.matchAll(/^\s*Type: (PT_\w+)/gmu)].map(([, type]) => type);

export const parseNeededLibraries = (text) => {
  const start = text.indexOf("NeededLibraries [");
  if (start === -1) return [];
  const body = text.slice(start + "NeededLibraries [".length);
  return body
    .slice(0, body.indexOf("\n]"))
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
};

export const parseMaxGlibcVersion = (text) => {
  const versions = [...text.matchAll(/\bGLIBC_(\d+(?:\.\d+)+)\b/gu)].map(([, version]) => version);
  if (versions.length === 0) return null;
  return versions.reduce((highest, version) => (compareVersions(version, highest) > 0 ? version : highest));
};

export const parseMachHeader = (text) => {
  const header = text.slice(text.indexOf("MachHeader {"));
  const cpuType = /^ {2}CpuType: (\S+)/mu.exec(header)?.[1];
  if (!cpuType) return null;
  return { class: /^ {2}Magic: Magic64/mu.test(header) ? "64-bit" : "32-bit", cpuType };
};

export const parseMachoVersionMin = (text) => {
  const command = /^ {2}Cmd: (\S+)/mu.exec(text)?.[1];
  if (!command) return null;
  return {
    command,
    platform: /^ {2}Platform: (\S+)/mu.exec(text)?.[1],
    version: /^ {2}Version: (\S+)/mu.exec(text)?.[1],
  };
};

export const parseDylibId = (text) => {
  const lines = text.split("\n").map((line) => line.trim()).filter(Boolean);
  return lines.at(-1) ?? null;
};

export const parsePeHeader = (text) => {
  const machine = /^ {2}Machine: (\S+)/mu.exec(text)?.[1];
  if (!machine) return null;
  const major = /^ {2}MajorSubsystemVersion: (\d+)/mu.exec(text)?.[1];
  const minor = /^ {2}MinorSubsystemVersion: (\d+)/mu.exec(text)?.[1];
  return {
    class: /^AddressSize: 64bit/mu.test(text) ? "64-bit" : "32-bit",
    machine,
    subsystem: /^ {2}Subsystem: (\S+)/mu.exec(text)?.[1],
    // No subsystem version at all rather than `undefined.undefined`, which would
    // compare as NaN and pass the floor check silently.
    subsystemVersion: major === undefined || minor === undefined ? null : `${major}.${minor}`,
  };
};

/**
 * Drop a Mach-O image's own install name from its needed libraries: LC_ID_DYLIB
 * is reported alongside LC_LOAD_DYLIB, and a Rust cdylib's install name is its
 * absolute build path.
 */
export const machoDependencies = (needed, installName) =>
  needed.filter((library) => library !== installName);

export const parseCoffImports = (text) => [...text.matchAll(/^ {2}Name: (\S+)/gmu)].map(([, name]) => name);

// --- Assertions. -----------------------------------------------------------

/**
 * Compare one inspected binary against the expectations its target implies.
 *
 * @param {Record<string, any>} target - Target derived from `napi.targets`.
 * @param {Record<string, any>} observed - What the tools reported.
 * @param {{ macosDeploymentTarget?: string }} [options] - Deployment pin in force.
 * @returns {string[]} One finding per violation.
 */
export const binaryFindings = (
  target,
  observed,
  { macosDeploymentTarget = process.env["MACOSX_DEPLOYMENT_TARGET"] } = {},
) => {
  const findings = [];
  const note = (message) => findings.push(`${target.suffix}: ${message}`);
  const format = expectedFormat(target);
  if (observed.format !== format) {
    return [`${target.suffix}: expected a ${format} binary, found ${observed.format}`];
  }

  const machine =
    format === "elf"
      ? ELF_MACHINE_BY_CPU[target.cpu]
      : format === "macho"
        ? MACHO_CPU_TYPE_BY_CPU[target.cpu]
        : PE_MACHINE_BY_CPU[target.cpu];
  if (observed.machine !== machine) note(`expected machine ${machine}, found ${observed.machine}`);
  if (observed.class !== "64-bit") note(`expected a 64-bit image, found ${observed.class}`);

  if (format === "elf") {
    if (observed.endianness !== "LittleEndian") {
      note(`expected data encoding LittleEndian, found ${observed.endianness}`);
    }
    if ((observed.programHeaders ?? []).includes("PT_INTERP")) {
      note("carries a PT_INTERP program header");
    }
    if (observed.glibcMax === null) {
      note("requires no versioned glibc symbol, so the runner's libc line is unproven");
    } else if (compareVersions(observed.glibcMax, GLIBC_CEILING) > 0) {
      note(`requires GLIBC_${observed.glibcMax}, above the ${GLIBC_CEILING} ceiling`);
    }
  }

  if (format === "macho") {
    if (observed.platform !== undefined && observed.platform !== "macos") {
      note(`expected the macos build platform, found ${observed.platform}`);
    }
    const floor = MACOS_SLICE_FLOOR[target.suffix];
    const expected =
      macosDeploymentTarget && compareVersions(macosDeploymentTarget, floor) > 0
        ? macosDeploymentTarget
        : floor;
    if (observed.minOs !== expected) {
      note(`expected LC_BUILD_VERSION minos ${expected}, found ${observed.minOs}`);
    }
  }

  if (format === "pe") {
    if (observed.subsystem !== undefined && !WINDOWS_SUBSYSTEMS.includes(observed.subsystem)) {
      note(`expected a DLL subsystem, found ${observed.subsystem}`);
    }
    if (!isVersion(observed.minOs)) {
      note(`expected a numeric subsystem version, found ${observed.minOs}`);
    } else if (compareVersions(observed.minOs, WINDOWS_SUBSYSTEM_VERSION_FLOOR) < 0) {
      note(`expected subsystem version ${WINDOWS_SUBSYSTEM_VERSION_FLOOR} or later, found ${observed.minOs}`);
    }
  }

  const family = format === "elf" ? "glibc" : format === "macho" ? "darwin" : "windows";
  for (const dependency of observed.needed ?? []) {
    if (!admits(dependency, DEPENDENCY_ALLOW_LIST[family], { caseInsensitive: family === "windows" })) {
      note(`unexpected dynamic dependency ${dependency}`);
    }
  }

  return findings;
};

/**
 * Assert the inspected set: one binary per configured target, nothing else, and
 * no two targets sharing bytes.
 *
 * @param {{ inventory: Record<string, any>, npmDir?: string, packages: Array<Record<string, any>>, stray: string[] }} input - Collected set.
 * @returns {string[]} One finding per violation.
 */
export const inventoryFindings = ({ inventory, npmDir = "npm", packages, stray }) => {
  const findings = [];
  for (const target of packages) {
    if (!inventory[target.suffix]) {
      findings.push(`${target.suffix}: ${npmDir}/${target.suffix}/${target.binary} is missing`);
    }
  }
  for (const path of stray) findings.push(`${path} is not a configured target binary`);

  const bySha = new Map();
  for (const [suffix, entry] of Object.entries(inventory)) {
    bySha.set(entry.sha256, [...(bySha.get(entry.sha256) ?? []), suffix]);
  }
  for (const [sha256, suffixes] of bySha) {
    if (suffixes.length > 1) {
      findings.push(`${suffixes.sort(byText).join(", ")}: share the identical binary ${sha256}`);
    }
  }
  return findings;
};

// --- Tool plumbing and CLI. ------------------------------------------------

const resolveToolDirectory = (cwd) => {
  if (process.env["LLVM_TOOLS_DIR"]) return process.env["LLVM_TOOLS_DIR"];
  // `rust-toolchain.toml` pins the compiler; its `llvm-tools` component is where
  // llvm-readobj/llvm-objdump live. `RUSTC` follows cargo's own override.
  const compiler = process.env["RUSTC"] ?? "rustc";
  const run = (args) => execFileSync(compiler, args, { cwd, encoding: "utf8" });
  const sysroot = run(["--print", "sysroot"]).trim();
  const host = run(["-vV"])
    .split("\n")
    .find((line) => line.startsWith("host: "))
    ?.slice("host: ".length)
    .trim();
  if (!host) throw new Error("rustc -vV did not report a host triple");
  return join(sysroot, "lib", "rustlib", host, "bin");
};

const readObject = (toolDirectory, file, ...args) =>
  execFileSync(join(toolDirectory, "llvm-readobj"), [...args, file], {
    encoding: "utf8",
    maxBuffer: TOOL_OUTPUT_LIMIT,
  });

const dumpObject = (toolDirectory, file, ...args) =>
  execFileSync(join(toolDirectory, "llvm-objdump"), [...args, file], {
    encoding: "utf8",
    maxBuffer: TOOL_OUTPUT_LIMIT,
  });

const inspectBinary = (toolDirectory, file) => {
  const bytes = readFileSync(file);
  const observed = {
    bytes: bytes.length,
    class: null,
    endianness: null,
    format: detectBinaryFormat(bytes),
    glibcMax: null,
    machine: null,
    minOs: null,
    needed: parseNeededLibraries(readObject(toolDirectory, file, "--needed-libs")),
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };

  if (observed.format === "elf") {
    Object.assign(observed, parseElfHeader(readObject(toolDirectory, file, "--file-headers")));
    observed.programHeaders = parseProgramHeaderTypes(readObject(toolDirectory, file, "--program-headers"));
    observed.glibcMax = parseMaxGlibcVersion(readObject(toolDirectory, file, "--version-info"));
  } else if (observed.format === "macho") {
    const header = parseMachHeader(readObject(toolDirectory, file, "--file-headers"));
    const version = parseMachoVersionMin(readObject(toolDirectory, file, "--macho-version-min"));
    const installName = parseDylibId(dumpObject(toolDirectory, file, "--macho", "--dylib-id"));
    observed.class = header?.class ?? null;
    observed.machine = header?.cpuType ?? null;
    observed.minOs = version?.version ?? null;
    observed.platform = version?.platform;
    observed.needed = machoDependencies(observed.needed, installName);
  } else if (observed.format === "pe") {
    const header = parsePeHeader(readObject(toolDirectory, file, "--file-headers"));
    observed.class = header?.class ?? null;
    observed.machine = header?.machine ?? null;
    observed.minOs = header?.subsystemVersion ?? null;
    observed.subsystem = header?.subsystem;
    observed.needed = parseCoffImports(readObject(toolDirectory, file, "--coff-imports"));
  }

  return observed;
};

const collectNativeBinaries = (npmDirectory) => {
  if (!existsSync(npmDirectory)) return [];
  const found = [];
  const walk = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.name.endsWith(".node")) found.push(path);
    }
  };
  walk(npmDirectory);
  return found;
};

/**
 * Inspect every addon under `<root>/<npmDir>`.
 *
 * @param {{ npmDir?: string, root?: string }} [options] - Prepared tree location.
 * @returns {{ findings: string[], inventory: Record<string, any> }} Findings and what was observed.
 */
export const inspectNative = ({ npmDir = "npm", root } = {}) => {
  const rootDirectory = resolve(root ?? fileURLToPath(new URL("../packages/npm", import.meta.url)));
  const npmDirectory = resolve(rootDirectory, npmDir);
  const { packages } = readNapiTargets(resolve(rootDirectory, "package.json"));
  const toolDirectory = resolveToolDirectory(rootDirectory);

  const expected = new Map(
    packages.map((target) => [resolve(npmDirectory, target.suffix, target.binary), target]),
  );
  const stray = [];
  const inventory = {};
  const findings = [];

  for (const file of collectNativeBinaries(npmDirectory).sort(byText)) {
    const target = expected.get(file);
    if (!target) {
      stray.push(relative(rootDirectory, file).replaceAll("\\", "/"));
      continue;
    }
    if (!statSync(file).isFile()) continue;
    const observed = inspectBinary(toolDirectory, file);
    findings.push(...binaryFindings(target, observed));
    inventory[target.suffix] = {
      bytes: observed.bytes,
      class: observed.class,
      endianness: observed.endianness,
      format: observed.format,
      glibcMax: observed.glibcMax,
      machine: observed.machine,
      minOs: observed.minOs,
      needed: observed.needed,
      sha256: observed.sha256,
    };
  }

  const ordered = Object.fromEntries(
    packages.filter((target) => inventory[target.suffix]).map((t) => [t.suffix, inventory[t.suffix]]),
  );
  return {
    findings: [...inventoryFindings({ inventory: ordered, npmDir, packages, stray }), ...findings],
    inventory: ordered,
  };
};

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({
    options: {
      "npm-dir": { default: "npm", type: "string" },
      root: { default: fileURLToPath(new URL("../packages/npm", import.meta.url)), type: "string" },
    },
  });
  try {
    const { findings, inventory } = inspectNative({ npmDir: values["npm-dir"], root: values.root });
    process.stdout.write(`${JSON.stringify(inventory, null, 2)}\n`);
    for (const finding of findings) process.stderr.write(`::error::${finding}\n`);
    if (findings.length > 0) {
      process.stderr.write(`${findings.length} native binary findings\n`);
      process.exit(1);
    }
    process.stderr.write(`inspected ${Object.keys(inventory).length} native binaries with no findings\n`);
  } catch (error) {
    process.stderr.write(`::error::${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  }
}
