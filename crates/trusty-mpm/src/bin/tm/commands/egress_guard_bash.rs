//! The Bash arm of `tm hook --egress-guard` (#8454 G3, slice S3).
//!
//! Seam: the types and entry points only; the scan lands with the green
//! commit.
//! Test: `egress_guard_bash_tests.rs`.

/// The audit slug of a Bash egress deny.
pub(crate) const BASH_CHECK: &str = "egress-bash";

/// One outbound-channel name a Bash command carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hit {
    /// The service the token reaches, as the deny names it.
    pub(crate) service: &'static str,
    /// The text that matched: a host, a program or a module name.
    pub(crate) token: String,
}

/// The first outbound-channel token `command` carries, if any.
pub(crate) fn bash_egress_hit(_command: &str) -> Option<Hit> {
    None
}

/// The plan §6 deny reason for `hit`.
pub(crate) fn bash_deny_reason(hit: &Hit) -> String {
    format!(
        "tm hook --egress-guard denies this Bash command: it reaches {} ({}) outside the \
         owner channel, and no tm-launched session, PM or subagent may message Slack, Gmail, \
         Notion or Google Chat directly (#8454). Ask the operator instead.",
        hit.service, hit.token
    )
}

#[cfg(test)]
#[path = "egress_guard_bash_tests.rs"]
mod tests;
