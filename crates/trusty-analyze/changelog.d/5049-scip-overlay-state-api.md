Changed

- **Breaking (library API), part of [#5049](https://github.com/bobmatnyc/trusty-tools/issues/5049):** `AnalyzerAppState::scip_overlays` changed type from `Arc<RwLock<HashMap<String, KgGraph>>>` to the new `core::ScipOverlayStore`, and `AnalyzerAppState::new` / `AnalyzerAppState::with_registry` take it as a required argument. It is a constructor parameter rather than a `with_*` override so no caller can end up with a non-durable overlay store by omission — that omission was the bug.
