Fixed
- `tm doctor --fix` (and `tm hooks clean`) no longer strips the tm hook entries a managed launch wrote into a project's `.claude/settings.json`, the Architect's included. `hooks_contamination` now counts only a build-tree hook command in a file that carries tm's PM guard or `trusty-memory` entry; a file tm never provisioned is cleaned as before (#10565).
