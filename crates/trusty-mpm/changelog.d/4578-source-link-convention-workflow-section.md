Added

- New bundled `## Source Citations` section in
  `sections/workflow.md`: source citations in docs and reports link to a
  GitHub blob permalink pinned to a commit SHA (never `blob/main`), with
  `path:line` as the link text and the line number verified before linking.
  This is framework doctrine, not project-specific, so it ships in the
  bundled instructions rather than a project-level `.trusty-mpm/` override
  (see [#4578](https://github.com/bobmatnyc/trusty-tools/issues/4578)).
