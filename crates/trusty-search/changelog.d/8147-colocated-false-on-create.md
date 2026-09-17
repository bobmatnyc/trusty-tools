Added

- `POST /indexes` accepts `colocated: false`, registering an index against the data-dir corpus store instead of creating `<root>/.trusty-search/` — a read-only or root-owned root can now be adopted without a 500 `corpus open failed`. Omitted or `true` is unchanged. A root that already carries colocated storage is refused with 400 rather than split between the two layouts ([#8147](https://github.com/bobmatnyc/trusty-tools/issues/8147))
