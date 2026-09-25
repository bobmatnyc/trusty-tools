Removed
- The `commands::kuzu_migrate` redb reader (`KuzuEntity`, `KuzuRelation`, `entity_uuid`, `entity_to_drawer`, `relation_to_triple`, `discover_schema`, `read_entities`, `read_relations`, `ENTITIES_TABLE`, `RELATIONS_TABLE`). It read a `store.redb` layout real kuzu-memory stores never had; `commands::kuzu_import` replaces it.
