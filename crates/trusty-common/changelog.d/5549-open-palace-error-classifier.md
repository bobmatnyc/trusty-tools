Added

- `PalaceRegistry::open_error_is_absent` answers whether an `open_palace` failure means the palace is genuinely not there. `open_palace` returns `anyhow::Error`, which flattens a missing `palace.json` together with a denied read, a transient `EIO`/`ESTALE`, undecodable metadata, an open-queue timeout, and a redb write-lock conflict — so callers that mapped `Err` to "not found" reported a palace they could not read as one that does not exist. The classification lives next to `open_palace` because that is the only place that knows its failure modes (#5549, ADR-0045).
