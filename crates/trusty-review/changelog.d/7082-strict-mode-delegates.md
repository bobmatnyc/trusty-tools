Changed

- `llm::enforce_strict_mode` delegates to `trusty_common::inference::strict_json_schema` instead of carrying its own recursive walk. Behavior is unchanged for every schema this crate sends, and the function, its signature, and `ResponseSchema::new`'s use of it all stay put; what moves is the implementation, so the same defect cannot be fixed here and stay broken in another crate — which is what #7082 was. The shared version also descends into `$defs`, `definitions`, and `anyOf`/`oneOf`/`allOf`, which the local one did not. Refs #7082
