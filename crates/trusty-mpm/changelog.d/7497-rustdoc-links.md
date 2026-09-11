Documentation

- Fixed the four intra-doc links #7506 left broken: `disk_usage_guard`'s module header now carries explicit crate-absolute reference definitions for `DEFAULT_MAX_USAGE_PCT`, `check_measured` and `bash_refusal` — a module whose docs are split between a `pub mod` outer comment and the file's own `//!` header resolves bare links in the parent module's scope — and the `pm_guard_bash::disk_usage` header names `trusty_mpm::core::disk_usage_guard`, since `crate::` inside the `tm` bin is the bin, not the library (#7497).
