Added
- `deploy_agents_filtered_with_suffix`, which appends caller-supplied text to every composed agent before validation, checksum, and write, so the manifest records the bytes that land. `deploy_agents_filtered` delegates to it with no suffix and is byte-identical to before.
