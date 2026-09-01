// Copy the NAPI-RS build output (the generated loader and any colocated addon)
// into `dist/native/`, which is what the compiled `dist/node.js` imports.
// `tsc` only emits `.ts`, and the loader and the `.node` are build artifacts,
// not source — so they are copied, never hand-written and never checked in.
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
