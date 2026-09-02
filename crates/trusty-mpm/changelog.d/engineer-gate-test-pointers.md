Fixed

- The PM's mandatory engineer-brief tail (`core.md`) now names this repo's
  doc gates — `check_test_pointers.sh`, `check_line_cap.sh`,
  `check_changelog_fragment.sh` — so a dispatched engineer runs them before
  returning, instead of a required CI job catching a missing gate after the
  fact (#6656, #6659, #6670).
