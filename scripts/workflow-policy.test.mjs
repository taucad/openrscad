// Structural assertions on the release workflows. Conventions rot; these are the
// ones whose breakage is expensive and invisible until a release is already
// half-published (napi-architecture-policy §5, §6, §10).
import assert from "node:assert/strict";
import { builtinModules } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { describe, it } from "node:test";
import { fileURLToPath } from "node:url";

import { readNapiTargets } from "./lib/napi-targets.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const read = (relative) => readFileSync(new URL(relative, import.meta.url), "utf8");

const native = read("../.github/workflows/native.yml");
const publish = read("../.github/workflows/publish-npm.yml");
const ci = read("../.github/workflows/ci.yml");
const action = read("../.github/actions/download-verified-artifact/action.yml");
const verifyScript = read("../.github/actions/download-verified-artifact/verify-artifact.sh");
const { packages } = readNapiTargets(new URL("../packages/npm/package.json", import.meta.url));

/**
 * Split one workflow into its top-level job blocks. Job identifiers are the only
 * keys indented by exactly two spaces after the `jobs:` mapping, so a line scan
 * isolates each without a YAML dependency this repository does not ship.
 *
 * @param {string} workflow - Workflow source.
 * @returns {Map<string, string>} Job name to body.
 */
const jobsOf = (workflow) => {
  const lines = workflow.split("\n");
  const start = lines.indexOf("jobs:");
  assert.notEqual(start, -1, "the workflow must declare a jobs mapping");
  const blocks = new Map();
  let current;
  for (const line of lines.slice(start + 1)) {
    const header = /^ {2}([A-Za-z0-9_-]+):\s*$/u.exec(line);
    if (header) {
      current = [];
      blocks.set(header[1], current);
      continue;
    }
    current?.push(line);
  }
  return new Map([...blocks].map(([name, body]) => [name, body.join("\n")]));
};

const nativeJobs = jobsOf(native);
const publishJobs = jobsOf(publish);

const job = (jobs, name) => {
  const body = jobs.get(name);
  assert(body, `the workflow must declare a ${name} job`);
  return body;
};

const occurrences = (haystack, needle) => haystack.split(needle).length - 1;

/** A registry credential actually referenced, as opposed to named in prose. */
const REGISTRY_SECRET = /\$\{\{\s*secrets\.(?:NPM_TOKEN|NODE_AUTH_TOKEN)/u;

/** How many jobs in a workflow declare an `id-token` permission, prose aside. */
const oidcGrants = (workflow) => [...workflow.matchAll(/^\s+id-token:/gmu)].length;

const builtins = new Set(builtinModules);

/** Every `node <path>.mjs` a job body runs, deduplicated. */
const scriptsRunBy = (body) => [
  ...new Set([...body.matchAll(/\bnode ([\w./-]+\.mjs)\b/gu)].map((match) => String(match[1]))),
];

/** Package specifiers reachable from one script through its relative imports. */
const packagesReachedBy = (script) => {
  const seen = new Set();
  const found = new Set();
  const pending = [resolve(root, script)];
  while (pending.length > 0) {
    const file = pending.pop();
    if (seen.has(file)) continue;
    seen.add(file);
    const source = readFileSync(file, "utf8");
    const specifiers = [
      ...source.matchAll(/(?:^|\n)\s*(?:import|export)[^;\n]*?from\s+["']([^"']+)["']/gu),
      ...source.matchAll(/\bimport\(\s*["']([^"']+)["']\s*\)/gu),
    ].map((match) => match[1]);
    for (const specifier of specifiers) {
      if (specifier.startsWith("node:") || builtins.has(specifier)) continue;
      if (specifier.startsWith(".")) pending.push(resolve(dirname(file), specifier));
      else found.add(specifier);
    }
  }
  return [...found].sort();
};

describe("native workflow", () => {
  it("should build exactly the configured napi targets, one row each", () => {
    const rows = [...job(nativeJobs, "build").matchAll(/^ {10}- target: (\S+)$/gmu)].map((m) => m[1]);
    assert.deepEqual(
      [...rows].sort(),
      packages.map((target) => target.triple).sort(),
      "the build matrix and package.json.napi.targets must name the same targets",
    );
  });

  it("should build every row from the single recipe in package.json", () => {
    const body = job(nativeJobs, "build");
    assert(body.includes("npm run build:addon -- --target ${{ matrix.target }}"));
    assert(body.includes("name: bindings-${{ matrix.target }}"));
    assert(body.includes("path: packages/npm/src/native/openrscad.*.node"));
    assert(body.includes("if-no-files-found: error"));
  });

  it("should assemble, inspect, reconcile, pack and freeze in that order", () => {
    const body = job(nativeJobs, "assemble");
    const order = [
      "npx napi create-npm-dirs --npm-dir npm",
      "npx napi artifacts --output-dir artifacts --npm-dir npm",
      "node scripts/inspect-native.mjs",
      "npx napi pre-publish --skip-optional-publish -t npm --no-gh-release",
      "node scripts/check-prepared-release.mjs",
      "node scripts/pack-test-tarballs.mjs --out tarballs",
      "node scripts/validate-pack.mjs",
      'sha256sum "$archive" > "$archive.sha256"',
    ];
    let previous = -1;
    for (const command of order) {
      const index = body.indexOf(command);
      assert(index !== -1, `assemble must run: ${command}`);
      assert(index > previous, `${command} runs out of order`);
      previous = index;
    }
  });

  it("should run every package-manager script before napi rewrites the manifest", () => {
    // `napi pre-publish` materializes optionalDependencies in the checkout's
    // package.json; npm's pre-run dependency check then rejects the now-stale
    // lockfile, so everything after it must be plain node or shell.
    const body = job(nativeJobs, "assemble");
    const reconciled = body.indexOf("npx napi pre-publish");
    assert.notEqual(reconciled, -1);
    const before = body.slice(0, reconciled);
    const after = body.slice(reconciled + "npx napi pre-publish".length);
    assert(before.includes("npm run build"), "the builds must precede the reconcile");
    assert(!/npm (ci|install|run) /u.test(after), "no npm script may follow the manifest reconcile");
    assert(!/npx /u.test(after), "no npx invocation may follow the manifest reconcile");
  });

  it("should smoke every configured target from the frozen tarballs", () => {
    const body = job(nativeJobs, "smoke");
    const suffixes = [...body.matchAll(/^ {10}- suffix: (\S+)$/gmu)].map((m) => m[1]);
    assert.deepEqual(
      [...suffixes].sort(),
      packages.map((target) => target.suffix).sort(),
      "every published target needs runtime evidence",
    );
    assert(body.includes("node packages/npm/test/smoke-installed.mjs"));
    assert(body.includes("OPENRSCAD_TARBALL_DIR: tarballs"));
    assert.equal(occurrences(body, "fallback: '1'"), 1, "one row proves the wasm fallback");
    assert(body.includes("fail-fast: false"));
  });

  it("should never publish or hold OIDC", () => {
    assert.equal(oidcGrants(native), 0, "the evidence workflow must hold no OIDC token");
    assert(!native.includes("npm publish"));
    assert(!REGISTRY_SECRET.test(native));
  });
});

describe("continuous integration", () => {
  it("should run the native matrix as evidence and publish nothing", () => {
    const body = job(jobsOf(ci), "native");
    assert(body.includes("uses: ./.github/workflows/native.yml"));
    assert(!ci.includes("npm publish"), "ci.yml must not publish");
    assert.equal(oidcGrants(ci), 0, "no ci.yml job may hold OIDC");
  });
});

describe("publication", () => {
  it("should grant OIDC only to the publish job", () => {
    assert.equal(oidcGrants(publish), 1);
    assert(job(publishJobs, "publish").includes("id-token: write"));
    assert(!REGISTRY_SECRET.test(publish), "publication is OIDC-only");
  });

  it("should refuse a tag that is not reachable from the default branch", () => {
    // npm's trusted-publisher match carries no ref, so a dispatch from a stray
    // tag would otherwise publish.
    const body = job(publishJobs, "release");
    assert(body.includes("git merge-base --is-ancestor"));
    assert(body.includes("origin/main"));
  });

  it("should route a prerelease away from latest, for platforms too", () => {
    const resolveTag = job(publishJobs, "release");
    assert(resolveTag.includes('*-*) echo "npmtag=next"'));
    const body = job(publishJobs, "publish");
    assert(body.includes("NPM_CONFIG_PROVENANCE: 'true'"));
    assert(
      body.includes("NPM_CONFIG_TAG: ${{ needs.release.outputs.npmtag }}"),
      "napi pre-publish spawns a bare npm publish per platform; only configuration reaches it",
    );
    assert(job(publishJobs, "registry-verify").includes("latest points at the prerelease"));
  });

  it("should publish every platform package before the root package", () => {
    const body = job(publishJobs, "publish");
    const platforms = body.indexOf("npx napi pre-publish --cwd");
    const rootPackage = body.indexOf("npm publish ./release");
    assert.notEqual(platforms, -1, "platforms publish through napi pre-publish");
    assert.notEqual(rootPackage, -1, "the root publishes explicitly");
    assert(
      platforms < rootPackage,
      "a root whose optional dependencies do not exist yet is uninstallable",
    );
    assert.equal(occurrences(publish, "npm publish ./release"), 1);
    assert(body.includes("cannot publish over the previously published versions"));
    assert(body.includes("node scripts/validate-pack.mjs --root release"));
  });

  it("should verify the frozen archive digest before extracting it", () => {
    const body = job(publishJobs, "publish");
    const check = body.indexOf('sha256sum --check "$ARCHIVE.sha256"');
    const extract = body.indexOf('tar -xzf "prepared-release/$ARCHIVE"');
    assert(check !== -1 && extract !== -1 && check < extract);
    assert(!body.includes("napi build"), "the publish job must never rebuild the frozen tree");
  });

  it("should verify the registry with bounded backoff before touching the release", () => {
    const verify = job(publishJobs, "registry-verify");
    assert(verify.includes("node scripts/registry-wait.mjs --tarballs tarballs/test-tarballs.json"));
    assert(verify.includes("npm audit signatures --include-attestations"));
    assert(verify.includes("npm install --force --ignore-scripts"));
    const assets = job(publishJobs, "release-assets");
    assert(assets.includes("needs: [release, native, registry-verify]"));
    assert.equal(occurrences(publish, "contents: write"), 1);
    assert(assets.includes("contents: write"));
    assert.equal(occurrences(publish, "gh release upload"), 1);
  });
});

describe("artifact transfer", () => {
  it("should download every artifact through the verified wrapper", () => {
    // A silent, empty `actions/download-artifact` reports success and writes
    // nothing; the failure then surfaces far downstream against a path nobody
    // chose. The wrapper is the only place a landing is proved.
    for (const [name, workflow] of [
      ["native.yml", native],
      ["publish-npm.yml", publish],
      ["ci.yml", ci],
    ]) {
      assert.equal(
        occurrences(workflow, "uses: actions/download-artifact@"),
        0,
        `${name} must not call actions/download-artifact directly`,
      );
    }
    assert.equal(occurrences(action, "uses: actions/download-artifact@"), 2, "one bounded retry");
  });

  it("should verify, retry once, and verify the retry", () => {
    const attempts = [...action.matchAll(/uses: actions\/download-artifact@\S+/gu)].map((m) => m.index);
    const checks = [...action.matchAll(/verify-artifact\.sh/gu)].map((m) => m.index);
    assert.equal(checks.length, 2, "each download attempt must be verified");
    assert(attempts[0] < checks[0] && checks[0] < attempts[1] && attempts[1] < checks[1]);
    assert(action.includes("if: steps.verify.outputs.complete == 'false'"));
    assert(!/while|until|for attempt/u.test(action), "one explicit attempt, not a loop");
    assert(verifyScript.startsWith("#!/usr/bin/env bash\n"));
    assert(verifyScript.includes("set -euo pipefail"));
    assert(/ls -l/u.test(verifyScript), "the failure must print what did land");
  });

  it("should name the files each frozen artifact must land", () => {
    assert(
      job(nativeJobs, "assemble").includes("expect: ${{ steps.expected.outputs.files }}"),
      "the bindings download must demand the addons napi.targets derives",
    );
    assert.equal(occurrences(native + publish, "expect: test-tarballs.json"), 3);
    assert(
      job(publishJobs, "publish").includes(
        "expect: ${{ needs.native.outputs.archive }} ${{ needs.native.outputs.archive }}.sha256",
      ),
    );
  });

  it("should pin every action the way this repository already pins them", () => {
    const uses = [native, publish, action]
      .flatMap((source) => [...source.matchAll(/uses: (\S+)/gu)].map((match) => match[1]))
      .filter((reference) => !reference.startsWith("./"));
    assert(uses.length > 0);
    for (const reference of uses) {
      assert(/@(?:v\d+|stable)$/u.test(reference), `${reference} must carry a pinned major or channel`);
    }
  });
});

describe("dependency-free jobs", () => {
  it("should run only import-free scripts in the jobs that install nothing", () => {
    // A job that renders from a published package or verifies registry state
    // installs no repository dependency on purpose: it proves what a consumer
    // gets, not what this checkout builds. `npm ci` is the marker, because
    // registry-verify's `npm install` runs inside a throwaway directory.
    const all = [...nativeJobs, ...publishJobs];
    const dependencyFree = all
      .filter(([, body]) => !body.includes("npm ci"))
      .map(([name, body]) => ({ name, scripts: scriptsRunBy(body) }))
      .filter(({ scripts }) => scripts.length > 0);

    assert.deepEqual(
      Object.fromEntries(dependencyFree.map(({ name, scripts }) => [name, scripts])),
      {
        "registry-verify": ["scripts/registry-wait.mjs"],
        smoke: ["packages/npm/test/smoke-installed.mjs"],
      },
      "the set of clean-room jobs changed",
    );

    for (const { name, scripts } of dependencyFree) {
      for (const script of scripts) {
        assert.deepEqual(
          packagesReachedBy(script),
          [],
          `${name} installs nothing, so ${script} must import no package`,
        );
      }
    }
  });
});
