Fixed

- Boot reconcile can now see commits made while the daemon was down. `indexed_head_sha` is persisted in `indexes.toml` and restore reads it instead of re-deriving it from live git, which previously made the stored and current SHA equal by construction so every git-backed index reported up-to-date on every boot (#4391).
- An interrupted deferred-embed pass no longer leaves a silently under-embedded index. The pass is recorded as outstanding in `indexes.toml` before it is queued and cleared when it commits, and warm boot re-arms the catch-up for any index still carrying the marker (#4390).
- `/health` now reports `deferred_embed_queue_depth`, which its own doc comment already claimed was exposed (#4390).

Changed

- **Breaking:** `PersistedIndex` is now `#[non_exhaustive]`, so future field additions stay non-breaking by construction. Construct it with `PersistedIndex::new(id, root_path)` (or `Default::default()`) and assign the fields you need — every field remains `pub`, so only the struct-literal syntax is withdrawn.
