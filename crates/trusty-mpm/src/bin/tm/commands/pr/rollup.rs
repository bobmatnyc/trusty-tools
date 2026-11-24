//! A PR's `statusCheckRollup` and the latest-run rule (#8638).
//!
//! #11673: the rule moved to [`trusty_mpm::core::pr_rollup`] so the daemon's
//! PR watch shares it; this module keeps the `tm pr` and `tm wait` paths.

pub(crate) use trusty_mpm::core::pr_rollup::{RollupEntry, deciding_per_check, deciding_runs};
