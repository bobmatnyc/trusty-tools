Fixed

- `palace_create`'s format gate reads `trusty_common::palace_id::palace_id_is_valid` instead of restating the slug shape. The two statements had drifted — the deriving side had no length cap — so a derived id could be one this gate refused. `project_slug_from_basename` clamps to `PALACE_ID_MAX_LEN` for the same reason: it is the name `validate_palace_name` tells a caller to use, and an over-long basename named a palace the format gate then rejected ([#2443](https://github.com/bobmatnyc/trusty-tools/issues/2443))
