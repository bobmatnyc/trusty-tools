# Google OKF sample bundles at `ad30107c`

The `.md` files under `bundles/` are copied byte for byte from
<https://github.com/GoogleCloudPlatform/open-knowledge-format> at commit
`ad30107c31c06aec8a7d5636e0d1058118604e6f`, directory `bundles/`. They are
the four bundles SPEC-OKF-01 criteria 1 and 2 name. Each bundle's
`viz.html` and `acme_retail/attesters/sql_equality.py` are left out: they
are not concept files.

The source repository is licensed under the Apache License 2.0; its
`LICENSE.md` is copied beside this file. Do not edit the bundle files. To
refresh them, copy them again from a named commit and update this file.

Used by `crates/trusty-okf/tests/format_criteria.rs` (#10746, slice S2).
