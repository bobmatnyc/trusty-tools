//! Canonical launchd labels for every trusty-* LaunchAgent (#4919).
//!
//! Why: there was no single definition of what a service's launchd label IS.
//! Each daemon crate declared its own `LAUNCHD_LABEL` literal, the installer
//! kept a second hand-maintained mirror of those literals
//! (`trusty-installer::commands::plist_label`, whose own doc admitted it was
//! "verified by grepping each daemon crate"), the per-crate Makefiles named a
//! third family (`com.bobmatnyc.trusty-search`), and the signed-install scripts
//! a fourth. Nothing made them agree, so they drifted — that is #2827
//! (install-mpm-signed printed `com.trusty.trusty-mpm.plist` for a daemon whose
//! plist is `com.trusty.mpm.plist`), #2965 (a docs page with yet another
//! family), and #2938 (a stale `com.trusty.trusty-search.plist` sitting beside
//! the live `com.trusty.search.plist`).
//!
//! What it cost: `trusty-search service install` wrote and bootstrapped
//! `com.trusty.trusty-search` while the unit launchd actually had loaded was
//! `com.trusty.search`. The install therefore booted out nothing, started a
//! second daemon contending for :7878 and the index locks, and left the plist
//! fixes made under #4868 (`ExitTimeOut`) sitting in a file launchd never read.
//! Re-fixing the literals one at a time is what let the defect come back after
//! #2827; this module removes the second copy instead.
//!
//! The convention: `com.trusty.<member with its `trusty-` prefix stripped>`,
//! with sub-units suffixed (`com.trusty.mpm.supervisor`,
//! `com.trusty.search.logrotate`). [`canonical_label`] is that rule as code, and
//! [`SERVICES`] is checked against it by `canonical_consts_match_the_convention`
//! — a table entry that restates a label wrongly fails the test run rather than
//! shipping.
//!
//! How far the survey actually reaches, stated exactly, because overstating a
//! partial survey is how the direction got reversed the first time: **every unit
//! with a live daemon obeys it** — `com.trusty.mpm`, `com.trusty.memory`,
//! `com.trusty.analyze`, `com.trusty.search`, `com.trusty.console`,
//! `com.trusty.agents.slack`, all confirmed from `launchctl list` with a pid.
//! Two do not, and both are being normalised onto it here rather than being
//! counted as support for it: `com.trusty.trusty-search.logrotate` is loaded
//! with no pid and no main unit beside it, and `com.trusty.trusty-review.plist`
//! sits on disk unloaded — and that file is this codebase's own output, so it
//! was never independent evidence of anything. Both are recorded as legacy
//! aliases below.
//!
//! Deliberately NOT `#[cfg(target_os = "macos")]`, unlike `crate::launchd`:
//! the registry is data, and gating it would stop the drift tests from running
//! on Linux CI, which is where a divergent literal most needs to be caught.
//!
//! Not to be confused with codesign identifiers
//! (`trusty-installer::commands::macos_signing::codesign_identifier`), which
//! live in their own namespace, use the full binary name, and must NOT be
//! renamed to match — changing a codesign identifier invalidates the binary's
//! designated requirement and re-triggers macOS TCC prompts (#2558).
//!
//! Test: `canonical_consts_match_the_convention`, `sub_unit_labels_extend_their_base`,
//! `legacy_labels_are_never_canonical`, `every_legacy_label_resolves_to_one_service`,
//! `no_stray_launchd_label_literals_in_workspace_sources`.
//!
//! [`canonical_label`]: crate::launchd_labels::canonical_label
//! [`SERVICES`]: crate::launchd_labels::SERVICES

/// Reverse-DNS domain prefix shared by every trusty-* LaunchAgent.
///
/// Why: the one place the `com.trusty` vendor prefix is written down. The
/// `com.bobmatnyc.*` family the trusty-search Makefile invented is a legacy
/// alias, not a second domain.
pub const DOMAIN: &str = "com.trusty";

/// Build a launchd label from a service stem, and optionally a sub-unit name.
///
/// Why: `concat!` is the only const-evaluable string join available without a
/// new dependency, and it takes literals only. Keeping the join in a macro
/// means [`DOMAIN`]'s value is typed once even though `const` items cannot
/// call a function.
/// What: `agent!("search")` → `"com.trusty.search"`;
/// `agent!("search", "logrotate")` → `"com.trusty.search.logrotate"`.
macro_rules! agent {
    ($stem:literal) => {
        concat!("com.trusty.", $stem)
    };
    ($stem:literal, $sub:literal) => {
        concat!("com.trusty.", $stem, ".", $sub)
    };
}

/// The trusty-mpm daemon (`tm daemon`). Binary is `tm`, member is `trusty-mpm`
/// — the label follows the MEMBER, which is why #4059's binary-vs-member
/// confusion cannot be resolved by looking at the executable name.
pub const MPM: &str = agent!("mpm");

/// The optional unattended supervisor that restarts the mpm daemon.
pub const MPM_SUPERVISOR: &str = agent!("mpm", "supervisor");

/// The trusty-memory daemon.
pub const MEMORY: &str = agent!("memory");

/// The trusty-analyze daemon.
pub const ANALYZE: &str = agent!("analyze");

/// The trusty-search daemon.
///
/// #4919: was `com.trusty.trusty-search` in
/// `trusty-search::commands::service::LAUNCHD_LABEL`, which is not the label
/// launchd has loaded on any host — see this module's header.
pub const SEARCH: &str = agent!("search");

/// The newsyslog-driver agent that rotates trusty-search's launchd stderr log.
pub const SEARCH_LOGROTATE: &str = agent!("search", "logrotate");

/// The trusty-console dashboard daemon.
///
/// #4919: was `com.trusty.trusty-console` in code while the loaded unit is
/// `com.trusty.console` — the same divergence as [`SEARCH`], so `console
/// service status` queried a label that does not exist.
pub const CONSOLE: &str = agent!("console");

/// The trusty-review daemon.
pub const REVIEW: &str = agent!("review");

/// The trusty-agents Slack gateway (`tagent --slack`).
pub const AGENTS_SLACK: &str = agent!("agents", "slack");

/// A launchd-managed service: its member name, canonical label, and the labels
/// earlier installs used for the same service.
///
/// Why: an upgrade has to evict what the PREVIOUS installer left behind, or it
/// starts a second unit beside the first (#2938). Recording the old names beside
/// the new one makes eviction derivable instead of remembered.
/// What: `member` is the workspace member / binary family the service belongs
/// to; `label` is what a fresh install writes and bootstraps; `legacy` is every
/// label a prior install of the SAME service could have registered, newest
/// first. `legacy` is never empty for a service whose label has ever changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Service {
    /// Workspace member the service belongs to, e.g. `"trusty-search"`.
    pub member: &'static str,
    /// Sub-unit name within the member, or `None` for the member's main daemon.
    pub sub_unit: Option<&'static str>,
    /// The label a current install writes and bootstraps.
    pub label: &'static str,
    /// Labels a prior install of this same service may have registered.
    pub legacy: &'static [&'static str],
}

/// Every launchd service this workspace installs.
///
/// Why: `tctl`, each daemon's own `service` subcommand, the doctor checks, and
/// the drift tests all need the same answer to "which label, and what does an
/// upgrade have to evict". This is that answer.
/// Test: `canonical_consts_match_the_convention` proves no entry restates a
/// label the convention would not produce.
pub const SERVICES: &[Service] = &[
    Service {
        member: "trusty-mpm",
        sub_unit: None,
        label: MPM,
        legacy: &[],
    },
    Service {
        member: "trusty-mpm",
        sub_unit: Some("supervisor"),
        label: MPM_SUPERVISOR,
        legacy: &[],
    },
    Service {
        member: "trusty-memory",
        sub_unit: None,
        label: MEMORY,
        // The trusty-memory Makefile's PLIST_LEGACY.
        legacy: &["com.trusty.trusty-memory"],
    },
    Service {
        member: "trusty-analyze",
        sub_unit: None,
        label: ANALYZE,
        legacy: &["com.trusty.trusty-analyze"],
    },
    Service {
        member: "trusty-search",
        sub_unit: None,
        label: SEARCH,
        // `com.trusty.trusty-search` is what the pre-#4919 Rust installer wrote
        // and what #2938 found stranded beside the live unit;
        // `com.bobmatnyc.trusty-search` is the trusty-search Makefile's third
        // family. Both must be evicted or an install leaves two units behind.
        legacy: &["com.trusty.trusty-search", "com.bobmatnyc.trusty-search"],
    },
    Service {
        member: "trusty-search",
        sub_unit: Some("logrotate"),
        label: SEARCH_LOGROTATE,
        legacy: &["com.trusty.trusty-search.logrotate"],
    },
    Service {
        member: "trusty-console",
        sub_unit: None,
        label: CONSOLE,
        legacy: &["com.trusty.trusty-console"],
    },
    Service {
        member: "trusty-review",
        sub_unit: None,
        label: REVIEW,
        legacy: &["com.trusty.trusty-review"],
    },
    Service {
        member: "trusty-agents",
        sub_unit: Some("slack"),
        label: AGENTS_SLACK,
        legacy: &[],
    },
];

/// Derive a member's canonical main-daemon label from the convention.
///
/// Why: the convention has to exist as executable code, or "the convention"
/// degrades into whatever the literals happen to say — which is how
/// `com.trusty.trusty-search` survived four issues.
/// What: strips a leading `trusty-` from `member` and prefixes [`DOMAIN`].
/// A member that is already bare (`"mpm"`) is left alone, so the function is
/// idempotent on stems.
/// Test: `canonical_consts_match_the_convention` runs it over every
/// [`SERVICES`] entry.
#[must_use]
pub fn canonical_label(member: &str) -> String {
    let stem = member.strip_prefix("trusty-").unwrap_or(member);
    format!("{DOMAIN}.{stem}")
}

/// Derive a sub-unit's label from its base label.
///
/// What: `sub_label("com.trusty.search", "logrotate")` →
/// `"com.trusty.search.logrotate"`.
/// Test: `sub_unit_labels_extend_their_base`.
#[must_use]
pub fn sub_label(base: &str, sub_unit: &str) -> String {
    format!("{base}.{sub_unit}")
}

/// Look up a service by its canonical label.
///
/// What: returns the [`SERVICES`] entry whose `label` matches, or `None`.
/// Test: `every_legacy_label_resolves_to_one_service`.
#[must_use]
pub fn service_for_label(label: &str) -> Option<&'static Service> {
    SERVICES.iter().find(|s| s.label == label)
}

/// Labels an upgrade must evict before bootstrapping `label`.
///
/// Why: this is what makes an install label-correct across a rename. Without
/// it, `service install` bootstraps the new label and leaves the old unit
/// running — two daemons, one port (#2938).
/// What: the `legacy` list for the service owning `label`, or empty when the
/// label is unknown (an unknown label evicts nothing rather than guessing).
/// Test: `legacy_labels_are_never_canonical`.
#[must_use]
pub fn legacy_labels_for(label: &str) -> &'static [&'static str] {
    service_for_label(label).map_or(&[], |s| s.legacy)
}

/// Whether a string is a CANONICAL launchd label this workspace installs.
///
/// Why: deliberately excludes legacy aliases. A legacy label appearing as a
/// literal in production source is not "a known label", it is the #4919 defect
/// — `trusty-search::commands::service::LAUNCHD_LABEL` was exactly such a
/// literal, and a membership test that accepted it would have passed while the
/// installer bootstrapped a unit launchd does not have.
/// What: true iff some [`SERVICES`] entry's `label` equals `candidate`.
/// Test: `no_stray_launchd_label_literals_in_workspace_sources` calls it to
/// decide whether a Makefile / shell / plist literal is acceptable — those
/// files cannot import a Rust constant, so naming the canonical label is the
/// best they can do, while a legacy or unknown one still fails.
#[must_use]
pub fn is_canonical_label(candidate: &str) -> bool {
    service_for_label(candidate).is_some()
}

#[cfg(test)]
mod tests;
