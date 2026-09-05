#!/usr/bin/env node
// Wait for the registry to serve every published package with an attestation and
// the integrity `npm pack` recorded for the frozen tarball (policy §7, §10).
//
// npm scans packages at publish time, so a successful publish is invisible to
// `npm view` for minutes. The wait is bounded rather than optimistic, and an
// integrity that disagrees with the tested bytes fails immediately instead of
// being retried. Imports no package: the job that runs it installs nothing.
//
// Ported from nanoraster's `scripts/registry-wait.mjs`.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";

const DEFAULT_INTERVAL_MS = 30_000;
const DEFAULT_MAX_INTERVAL_MS = 300_000;
const DEFAULT_TIMEOUT_MS = 30 * 60_000;

/** Read one published version's metadata, or `null` while it is not served. */
export const npmView = (name, version) => {
  try {
    return JSON.parse(execFileSync("npm", ["view", `${name}@${version}`, "--json"], { encoding: "utf8" }));
  } catch {
    return null;
  }
};

/**
 * Poll the registry until every packed package is served with matching bytes.
 *
 * @param {Record<string, any>} input - Tarball manifest plus polling bounds and injectable clock/view.
 * @returns {Promise<void>} Resolves when every package is visible.
 */
export const waitForRegistry = async ({
  intervalMs = DEFAULT_INTERVAL_MS,
  log = (message) => process.stdout.write(`${message}\n`),
  maxIntervalMs = DEFAULT_MAX_INTERVAL_MS,
  now = Date.now,
  sleep = delay,
  tarballs,
  timeoutMs = DEFAULT_TIMEOUT_MS,
  view = npmView,
}) => {
  const entries = Object.entries(tarballs.packages);
  const start = now();
  const deadline = start + timeoutMs;
  const pending = new Map(entries);
  const unavailable = new Map();

  for (let attempt = 1; ; attempt += 1) {
    for (const [name, packed] of pending) {
      const metadata = view(name, packed.version);
      const integrity = metadata?.dist?.integrity;
      if (!integrity) {
        unavailable.set(name, "not published");
        continue;
      }
      if (integrity !== packed.integrity) {
        throw new Error(
          `${name}@${packed.version}: registry integrity ${integrity} differs from the packed ${packed.integrity}`,
        );
      }
      if (!metadata.dist.attestations) {
        unavailable.set(name, "no attestations");
        continue;
      }
      unavailable.delete(name);
      pending.delete(name);
    }

    const elapsedSeconds = Math.round((now() - start) / 1000);
    log(
      `attempt ${attempt} after ${elapsedSeconds}s: ${entries.length - pending.size}/${entries.length} packages available`,
    );
    if (pending.size === 0) {
      log(`all ${entries.length} packages are visible with matching integrity`);
      return;
    }
    if (now() >= deadline) {
      const minutes = Number((timeoutMs / 60_000).toFixed(1));
      const detail = [...unavailable].map(([name, reason]) => `${name} (${reason})`).sort().join(", ");
      throw new Error(`timed out after ${minutes} minutes; unavailable: ${detail}`);
    }
    // Double the wait, cap it, and never sleep past the deadline.
    const wait = Math.min(intervalMs * 2 ** (attempt - 1), maxIntervalMs, deadline - now());
    await sleep(wait);
  }
};

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const { values } = parseArgs({
    options: {
      "interval-seconds": { default: String(DEFAULT_INTERVAL_MS / 1000), type: "string" },
      "max-interval-seconds": { default: String(DEFAULT_MAX_INTERVAL_MS / 1000), type: "string" },
      tarballs: { type: "string" },
      "timeout-minutes": { default: String(DEFAULT_TIMEOUT_MS / 60_000), type: "string" },
    },
  });
  try {
    if (!values.tarballs) throw new Error("expected --tarballs <test-tarballs.json>");
    await waitForRegistry({
      intervalMs: Number(values["interval-seconds"]) * 1000,
      maxIntervalMs: Number(values["max-interval-seconds"]) * 1000,
      tarballs: JSON.parse(readFileSync(values.tarballs, "utf8")),
      timeoutMs: Number(values["timeout-minutes"]) * 60_000,
    });
  } catch (error) {
    process.stderr.write(`::error::${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(1);
  }
}
