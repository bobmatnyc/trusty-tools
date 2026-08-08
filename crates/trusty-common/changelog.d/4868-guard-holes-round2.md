Fixed

- The launchd-label drift guard no longer skips production code. Four holes, each
  proven by planting a literal that passed: `#[cfg(not(test))]` and
  `#[cfg(any(…, test))]` gate PRODUCTION code but were treated as test items, so
  the scan skipped exactly what it should read (eight such sites exist);
  `strip_comment` blanked any line starting with `*`, so a deref assignment
  `*target = "…"` was read as a comment while the identical `let` form was not;
  `#[cfg(test)] use std::fmt;` puts attribute and item on one line, so consuming
  the NEXT line ate production code; and the codesign exemption was whole-line,
  so a launchd label sharing a line with a `*_IDENTIFIER` assignment was skipped
  along with it. Polarity is now checked rather than token presence, block-comment
  state is tracked across lines, a self-contained attribute line consumes nothing
  further, and only the identifier token adjacent to the marker is exempt. A
  build file may name a legacy label on a `bootout`/`unload` line — evicting an
  old label is the migration, not the drift (#4868)
