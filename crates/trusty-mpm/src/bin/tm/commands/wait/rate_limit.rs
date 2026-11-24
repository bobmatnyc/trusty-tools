//! Rate-limit backoff for `tm wait` probes (#10550).
//!
//! #11673: the classifier moved to [`trusty_mpm::core::gh_rate_limit`] so the
//! daemon's PR watch backs off on the same refusal; this module keeps the
//! `tm wait` paths.

#[cfg(test)]
pub(crate) use trusty_mpm::core::gh_rate_limit::RateLimited;
pub(crate) use trusty_mpm::core::gh_rate_limit::{
    MAX_CONSECUTIVE_RATE_LIMITED, backoff_secs, classify,
};
