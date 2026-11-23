Fixed
- `GlobalConfig::save` publishes through `state_writer::atomic_update`, so it takes the same `config.toml.lock` every other writer of that file takes. It was the one `config.toml` writer with no lock, and since the startup listeners-to-channels drain writes that file on every non-`--api` start, a `save()` could rename over the drain's bytes and lose the migration.
