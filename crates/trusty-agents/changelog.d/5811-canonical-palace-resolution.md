Fixed

- The workstream endpoints resolve their palace through `trusty_common::palace_resolve` instead of trusty-memory's `project_slug_at` helpers, which are pin-then-basename and consult no git identity. In an unpinned repo the GET handler listed workstreams from `<dirname>` while the daemon wrote them to `<owner>-<repo>`, so the list came back empty; `create_tagged_drawer_at` wrote drawers to that same wrong palace and no longer creates a pin file as a side effect of a chat turn ([#5811](https://github.com/bobmatnyc/trusty-tools/issues/5811))
