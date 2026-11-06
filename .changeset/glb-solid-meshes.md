---
"openrscad-release-root": minor
---

GLB nodes now follow the generic glTF shape consumers such as GeoSpec read: every closed solid is one leaf node and mesh declaring `EXT_mesh_manifold`, its primitives split by owning module and material and tagged with their owner, while authored modules remain the node hierarchy as mesh-less containers; `asset.extras.openrscad.schemaVersion` is 2. This breaks readers of the previous layout, where a module node carried its own triangles: surfaces now sit on the solid nodes, feature lines are separate `LINES`-only meshes, and the exported triangles are unchanged. A solid that cannot be declared manifold is still exported, without the extension and with an export warning.
