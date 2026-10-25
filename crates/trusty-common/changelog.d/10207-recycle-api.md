Breaking
- `embedder_client::RecycleTrigger` gains the `HardFootprint` variant and `RecycleSnapshot` gains the `hard_footprint_kills` field (#10207); an exhaustive `match` on the enum or a struct literal of the snapshot outside this crate no longer compiles. `RecycleConfig` gains `hard_footprint_mb` and `startup_load_scale_max` (it is `#[non_exhaustive]`, so no caller breaks).
