Fixed
- A declared `[[stores]].index` keeps the default `vector_search` slot after knowledge provisioning instead of being shadowed by the protected OKG index (#7902). The protected index stays queryable by id, and a declared store that names the protected index id is refused with an error.
