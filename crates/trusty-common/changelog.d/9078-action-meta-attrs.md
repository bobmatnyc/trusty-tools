Breaking
- `control_bus::ActionMeta` gains `attrs: BTreeMap<String, serde_json::Value>`, the producer's own metadata for a row (counts, durations, versions, outcome codes). On the wire it is additive: omitted when empty, and a payload without it deserializes. A struct literal of `ActionMeta` outside trusty-common must add `attrs: Default::default()` (#9078).
