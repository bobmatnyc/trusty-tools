Added

- `POST /indexes` accepts `colocated: false`, registering an index against the data-dir corpus store instead of creating `<root>/.trusty-search/` — a read-only or root-owned root can now be adopted without a 500 `corpus open failed`. Omitted or `true` is unchanged. A root that already holds a `.trusty-search/` directory, even one the daemon cannot write, registers with `colocated: false`; that directory is left alone. The field is carried by `POST /indexes` and the `search.index.create` socket method only ([#8147](https://github.com/bobmatnyc/trusty-tools/issues/8147))
