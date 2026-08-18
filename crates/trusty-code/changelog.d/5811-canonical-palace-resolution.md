Fixed

- `derive_palace_id_for_project` routes through `trusty_common::palace_resolve` instead of probing the git remote itself and calling the pure three-level core, so a session's turns land in a project's PINNED palace rather than the derived one ([#5811](https://github.com/bobmatnyc/trusty-tools/issues/5811))
