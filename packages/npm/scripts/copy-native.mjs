// Copy the NAPI-RS build output into `dist/native/`, which is what the compiled
// `dist/node.js` imports. `tsc` only emits `.ts`, and the loader and the `.node`
// are build artifacts, not source — so they are copied, never hand-written and
// never checked in.
//
// The declarations are a build input and are not shipped; the `.node` is copied
// so the in-repo parity gate loads the real addon, and `package.json`'s `files`
// excludes it from the tarball because the addon belongs to the generated
// platform packages.
import { cpSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const from = join(root, "src", "native");
const to = join(root, "dist", "native");
if (!existsSync(from)) {
  throw new Error(`missing ${from} — run \`npm run build:addon\` first`);
}
mkdirSync(to, { recursive: true });
for (const entry of readdirSync(from)) {
  if (entry.endsWith(".d.ts")) continue; // a build input, not shipped
  cpSync(join(from, entry), join(to, entry));
}
