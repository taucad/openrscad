// Read `npm pack --json` output as one tarball description.
//
// npm 11 prints an array with one entry per packed spec; npm 12.0.0 changed it
// to an object keyed by package name. The publish job installs npm explicitly
// (Trusted Publishing needs ≥ 11.5.1), so the shape depends on that pin, and the
// 0.11.0-beta.3 publish failed the moment `npm@latest` became 12.0.2. Both shapes
// are accepted here; "exactly one tarball" is still asserted.
import { execFileSync } from "node:child_process";

/**
 * Normalise `npm pack --json` output to the single tarball it describes.
 *
 * @param {unknown} parsed - The parsed JSON npm printed.
 * @param {string} directory - Where the pack ran, for the error message.
 * @returns {Record<string, unknown>} The one tarball description.
 */
export const singlePackedTarball = (parsed, directory) => {
  const entries = Array.isArray(parsed)
    ? parsed
    : parsed !== null && typeof parsed === "object"
      ? Object.values(parsed)
      : [];
  if (entries.length !== 1 || entries[0] === null || typeof entries[0] !== "object") {
    throw new Error(`npm pack must describe exactly one tarball in ${directory}`);
  }
  return entries[0];
};

/**
 * Run `npm pack --json --ignore-scripts` in `directory` and return the one
 * tarball it describes.
 *
 * @param {string} directory - The package directory to pack.
 * @param {readonly string[]} [extraArguments] - Extra npm arguments (`--dry-run`, `--pack-destination`).
 * @returns {Record<string, unknown>} The tarball description.
 */
export const npmPackJson = (directory, extraArguments = []) =>
  singlePackedTarball(
    JSON.parse(
      execFileSync("npm", ["pack", "--json", "--ignore-scripts", ...extraArguments], {
        cwd: directory,
        encoding: "utf8",
      }),
    ),
    directory,
  );
