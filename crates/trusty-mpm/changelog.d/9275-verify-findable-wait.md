Breaking
- `core::memory_import::refresh::verify_findable` takes a fourth argument, `wait: &mut WarmupWait` (#9275): the budget for asking a still-warming trusty-memory embedder again before the findability row fails. Pass a `WarmupWait` shared across the files of one import.
