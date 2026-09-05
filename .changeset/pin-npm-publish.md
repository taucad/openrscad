---
"openrscad-release-root": patch
---

Pin npm in the publish workflow instead of installing `npm@latest`, and read `npm pack --json` in both the npm 11 (array) and npm 12 (object) shapes. The `0.11.0-beta.3` publish failed before any package reached the registry because npm 12.0.2 became `latest` that day and changed the output the release-tree validation parses.
