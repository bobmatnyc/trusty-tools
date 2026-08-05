Changed

- the `list_drawers_creates_desc_paginates` fixture builds its drawers with `Drawer::new` plus field assignment instead of a struct literal (refs [#4884](https://github.com/bobmatnyc/trusty-tools/issues/4884)). The literal named all twelve `Drawer` fields while the test only cares about `importance` and `created_at`, so every field added to `Drawer` in trusty-common broke this crate's build for no behavioural reason — `fact_key` was the third such break. No production code or behaviour changed.
