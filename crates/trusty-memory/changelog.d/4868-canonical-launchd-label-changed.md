Changed

- `LAUNCHD_LABEL` is read from `trusty_common::launchd_labels::MEMORY` rather
  than restated. The value is unchanged; a correct-but-duplicated literal is the
  state trusty-search's label was in before it drifted (#4868)
