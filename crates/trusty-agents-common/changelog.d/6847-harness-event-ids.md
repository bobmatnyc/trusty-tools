Added

- `events::make_event` (used by both `publish` and `emit`) now stamps every `HarnessEvent` with a fresh `EventId` and `parent_id: None`; `events::EventId` is re-exported from `trusty_common::control_bus`. Threading real causal context (a non-`None` `parent_id`) through this crate's emit sites is future work (DOC-73 §9 Phase 3) — every event this crate emits today is a root (#6847).
