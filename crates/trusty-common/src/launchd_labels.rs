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
//! **A retired daemon keeps its row (#6290).** [`SERVICES`] is what an install
//! WRITES; [`RETIRED_SERVICES`] is what an upgrade must CLEAR. trusty-review is
//! the first row in the second table: ADR-0032's review lane retired its daemon
//! outright, so nothing installs `com.trusty.review` any more — but every host
//! that ran the old binary still has that unit loaded, pointed at a `serve`
//! subcommand the binary no longer has. Dropping the row would leave the unit
//! unnamed by anything and therefore un-evictable, which is why a retirement is
//! a MOVE between the two tables, never a deletion.
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

/// The RETIRED trusty-review daemon (#6290).
///
/// trusty-review has no daemon: reviews run per invocation. This label is kept
/// so an upgrade can EVICT the unit a pre-#6290 install left loaded — it lives
/// in [`RETIRED_SERVICES`], not [`SERVICES`], and nothing writes it any more.
/// Deleting it would strand that unit on every host that ever installed the
/// old binary, respawning a `serve` subcommand the binary no longer has.
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
///
/// #6290: the same struct describes a RETIRED service in [`RETIRED_SERVICES`],
/// where `label` reads as "the last label this service had" rather than "what a
/// fresh install writes" — a retired service has no fresh install. Everything
/// else means what it means here.
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
        member: "trusty-agents",
        sub_unit: Some("slack"),
        label: AGENTS_SLACK,
        legacy: &[],
    },
];

/// Services this workspace once installed and now only EVICTS (#6290).
///
/// Why: retiring a daemon is not the same as never having had one. Every host
/// that installed trusty-review before #6290 has `com.trusty.review` loaded,
/// pointed at a `serve` subcommand the binary no longer has, and
/// `KeepAlive::Always` respawns it forever. Deleting the row would leave that
/// unit unnamed by anything, so nothing could boot it out; keeping it in
/// [`SERVICES`] would keep an install WRITING it. This table is the third
/// answer: named, so it can be evicted; separate, so it is never installed.
///
/// What: the same [`Service`] shape, read as "the labels an upgrade must clear
/// for this member" — `label` plus every entry in `legacy`.
/// [`retired_labels_for_member`] flattens the two.
///
/// A row moves here the moment its daemon is retired and stays forever: the
/// population of hosts carrying a stale unit only ever grows more diffuse, and
/// an eviction that costs one `launchctl bootout` against a label that is not
/// loaded costs nothing at all.
///
/// Test: `retired_services_are_not_installed`,
/// `retired_review_carries_both_its_labels`.
pub const RETIRED_SERVICES: &[Service] = &[
    // #6290: ADR-0032's review lane. Reviews run per invocation
    // (`trusty-review run`); there is no listener to supervise.
    Service {
        member: "trusty-review",
        sub_unit: None,
        label: REVIEW,
        legacy: &["com.trusty.trusty-review"],
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
/// What: returns the [`SERVICES`] or [`RETIRED_SERVICES`] entry whose `label`
/// matches, or `None`.
///
/// #6290 — why retired rows are searched too: the eviction of a retired unit
/// needs its `legacy` list exactly as an install needs a live one's, and
/// `com.trusty.review` is still a label this workspace names. Excluding it
/// would make [`legacy_labels_for`] return empty for it, so an upgrade would
/// boot out the canonical unit and leave `com.trusty.trusty-review` loaded
/// beside it — #2938's two-units-one-service shape, arrived at from the other
/// direction.
/// Test: `every_legacy_label_resolves_to_one_service`,
/// `retired_review_carries_both_its_labels`.
#[must_use]
pub fn service_for_label(label: &str) -> Option<&'static Service> {
    SERVICES
        .iter()
        .chain(RETIRED_SERVICES)
        .find(|s| s.label == label)
}

/// The retired service registered for `member`, if any.
///
/// What: the [`RETIRED_SERVICES`] entry whose `member` matches. `None` for a
/// member that never had a retired unit, which is every member but one.
/// Test: `retired_services_are_not_installed`.
#[must_use]
pub fn retired_service_for_member(member: &str) -> Option<&'static Service> {
    RETIRED_SERVICES.iter().find(|s| s.member == member)
}

/// Every launchd label an upgrade must clear for `member`.
///
/// Why: an installer asking "is there anything to evict for this member?" wants
/// one list, not a canonical label plus a separate legacy walk it has to
/// remember to do — forgetting the second half is how a pre-rename unit
/// survives an upgrade (#2938).
/// What: the retired service's own `label` followed by its `legacy` aliases,
/// newest first; empty for a member with no retired unit.
/// Test: `retired_review_carries_both_its_labels`.
#[must_use]
pub fn retired_labels_for_member(member: &str) -> Vec<&'static str> {
    retired_service_for_member(member).map_or_else(Vec::new, |s| {
        std::iter::once(s.label)
            .chain(s.legacy.iter().copied())
            .collect()
    })
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

/// What became of ONE launchd label an eviction pass tried to clear.
///
/// Why: an eviction that reports only "which labels were evicted" cannot tell
/// "there was nothing there" apart from "the removal failed" (#6290). Those
/// need opposite handling: the first is the steady state on every host after
/// the first pass, the second leaves a retired unit loaded and respawning, and
/// an installer that treats it as success exits 0 with the daemon still up.
/// What: per label, one of evicted / absent / failed-with-a-reason.
/// Test: `eviction_outcome_only_failed_is_a_failure`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvictionOutcome {
    /// The unit was loaded or its plist was on disk; neither is now.
    Evicted,
    /// Nothing to clear: not loaded, and no plist on disk.
    Absent,
    /// The unit is still loaded, or its plist is still on disk. The payload is
    /// the operator-facing reason.
    Failed(String),
}

impl EvictionOutcome {
    /// Whether this outcome must fail the pass that produced it.
    ///
    /// Why: classified once, on the enum, rather than re-derived at each call
    /// site — the same rule `BootstrapAction::is_failure` follows, and for the
    /// same reason (#4470): an inline `matches!` at one call site is how a
    /// variant comes to be silently treated as success.
    /// Test: `eviction_outcome_only_failed_is_a_failure`.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, EvictionOutcome::Failed(_))
    }
}

/// One label paired with what an eviction pass did to it.
///
/// Why: the caller reports per label, so the label has to travel with its
/// outcome rather than being recoverable only by position (#6290).
/// Test: `eviction_outcome_only_failed_is_a_failure`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LabelEviction {
    /// The launchd label this outcome is about.
    pub label: String,
    /// What became of it.
    pub outcome: EvictionOutcome,
}

impl LabelEviction {
    /// Pair `label` with `outcome`.
    ///
    /// `#[non_exhaustive]` keeps a future field from being a breaking change,
    /// so external crates (the installer, its test fakes) construct through
    /// here rather than with a struct literal.
    #[must_use]
    pub fn new(label: impl Into<String>, outcome: EvictionOutcome) -> Self {
        Self {
            label: label.into(),
            outcome,
        }
    }
}

#[cfg(test)]
mod tests;
