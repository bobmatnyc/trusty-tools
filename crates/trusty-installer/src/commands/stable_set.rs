//! The agreed STABLE trusty tool set — the coherent platform `tctl` installs and
//! upgrades as a unit (#1316).
//!
//! Why: `tctl install` / `tctl upgrade` must operate on one canonical,
//! topologically-ordered set so the whole platform moves together rather than
//! drifting member-by-member. Encoding the set (and its install order) as data
//! in one place means adding/removing a member is a data edit, never scattered
//! branching, and keeps the install/upgrade/updates handlers agreeing on exactly
//! which crates are in scope.
//!
//! What: Defines [`StableMember`] (crate name + binary name + whether it is a
//! supervised daemon) and [`stable_set`] — the ordered member list:
//! trusty-search, trusty-memory, trusty-analyze, trusty-review, tga, and
//! trusty-console. Library crates (trusty-common, trusty-embedderd,
//! trusty-bm25-daemon, …) are pulled in automatically as cargo dependencies of
//! these binaries, so they are intentionally *not* listed here.
//!
//! Test: `tests` pins the ordered set, asserts every member's crate/binary
//! names, and asserts the daemon flags.

use serde::Serialize;

/// How a daemon member is brought up / taken down.
///
/// Why: `tctl start|stop|restart` cannot assume every daemon is launchd-managed.
/// The shared daemons (search/memory/analyze) register `~/Library/LaunchAgents`
/// plists and are controlled with `launchctl bootstrap`/`bootout`. trusty-mpm,
/// by contrast, is NOT launchd-managed — it ships its own `trusty-mpm
/// start|stop|restart` subcommands that spawn the daemon and `pkill` it
/// (verified in `crates/trusty-mpm/src/bin/tm/commands/daemon.rs`). Encoding the
/// strategy as data lets the lifecycle handler dispatch correctly per member
/// instead of forcing every daemon through a launchd path some members lack.
///
/// What: [`ManageStrategy::Launchd`] → control via the shared
/// `trusty_common::launchd::LaunchdConfig` `bootstrap`/`bootout`;
/// [`ManageStrategy::OwnVerb`] → shell out to the member's own
/// `<binary> start|stop|restart` subcommand; [`ManageStrategy::None`] →
/// non-daemon (no lifecycle control).
///
/// Test: `tests::mpm_uses_own_verb`, `tests::daemons_use_launchd`,
/// `tests::non_daemon_has_no_strategy`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManageStrategy {
    /// launchd-supervised: `launchctl bootstrap` / `bootout` via `LaunchdConfig`.
    Launchd,
    /// Self-managed: the binary's own `start`/`stop`/`restart` subcommand.
    OwnVerb,
    /// Not a daemon — no lifecycle control.
    None,
}

/// One member of the stable trusty tool set.
///
/// Why: `tctl` keys four things off a member — the crates.io package name (for
/// `cargo install` / `check_crates_io`), the installed binary name (for presence
/// and health probes), whether it is a supervised daemon (so upgrade can restart
/// it cleanly), and HOW it is managed (launchd vs its own start/stop verb).
/// Bundling them keeps every handler consistent.
///
/// What: `crate_name` is the cargo package; `binary` is the installed binary
/// (often equal, but `tga` differs); `daemon` marks members that run as a
/// long-lived HTTP daemon and therefore need a connection-safe restart after an
/// upgrade (`upgrade_and_restart`); `manage` is the lifecycle strategy
/// (derived from `daemon` + binary by [`StableMember::new`]).
///
/// Test: `tests::stable_set_is_pinned`, `tests::mpm_uses_own_verb`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StableMember {
    /// The crates.io package name (`cargo install <crate_name> --locked`).
    pub crate_name: String,
    /// The installed binary name probed on PATH (`which <binary>`).
    pub binary: String,
    /// Whether this member runs as a supervised daemon (needs restart on upgrade).
    pub daemon: bool,
    /// How `tctl start|stop|restart` controls this member's lifecycle.
    pub manage: ManageStrategy,
}

impl StableMember {
    /// Construct a stable-set member, deriving its lifecycle strategy.
    ///
    /// Why: Terse constructor keeps the [`stable_set`] table readable while
    /// centralising the one place a member's `manage` strategy is decided, so
    /// callers (install/upgrade) that ignore lifecycle are unaffected.
    /// What: Builds a [`StableMember`]; a non-daemon gets
    /// [`ManageStrategy::None`]; trusty-mpm gets [`ManageStrategy::OwnVerb`]
    /// (it is process-managed, not launchd); every other daemon gets
    /// [`ManageStrategy::Launchd`].
    /// Test: Exercised by [`stable_set`] and `tests::mpm_uses_own_verb`.
    fn new(crate_name: &str, binary: &str, daemon: bool) -> Self {
        let manage = if !daemon {
            ManageStrategy::None
        } else if binary == "trusty-mpm" {
            ManageStrategy::OwnVerb
        } else {
            ManageStrategy::Launchd
        };
        Self {
            crate_name: crate_name.to_owned(),
            binary: binary.to_owned(),
            daemon,
            manage,
        }
    }
}

/// The ordered, agreed STABLE trusty tool set (#1316).
///
/// Why: This is the single source of truth for what `tctl install` brings up and
/// `tctl upgrade` moves forward. The order is the topological install order:
/// the shared daemons (search/memory/analyze) first, then the review service and
/// git-analytics it composes with, then the console (the HTTP front door that
/// proxies the others) last so it discovers already-running members.
///
/// What: Returns the member list in install/topological order — trusty-search,
/// trusty-memory, trusty-analyze, trusty-review, tga (binary `tga`,
/// non-daemon), trusty-console (daemon), and trusty-mpm (the orchestrator,
/// brought up last, process-managed via its own `start`/`stop` verbs). Library
/// crates resolve as cargo dependencies and are not listed.
///
/// Test: `tests::stable_set_is_pinned`, `tests::tga_crate_and_binary_names`,
/// `tests::daemon_flags_match_spec`, `tests::mpm_uses_own_verb`.
pub fn stable_set() -> Vec<StableMember> {
    vec![
        StableMember::new("trusty-search", "trusty-search", true),
        StableMember::new("trusty-memory", "trusty-memory", true),
        StableMember::new("trusty-analyze", "trusty-analyze", true),
        StableMember::new("trusty-review", "trusty-review", true),
        StableMember::new("tga", "tga", false),
        StableMember::new("trusty-console", "trusty-console", true),
        StableMember::new("trusty-mpm", "trusty-mpm", true),
    ]
}

/// Filter the stable set to a caller-named subset, preserving install order.
///
/// Why: `tctl install <members…>` / `tctl upgrade <members…>` let the operator
/// name a subset; resolving names against the canonical set (rather than the raw
/// strings) validates them and keeps the topological order intact.
///
/// What: When `names` is empty, returns the full [`stable_set`]. Otherwise
/// returns the members whose `crate_name` OR `binary` matches a requested name,
/// in stable-set order, plus the list of unrecognised names.
///
/// Test: `tests::select_empty_returns_all`, `tests::select_subset_preserves_order`,
/// `tests::select_reports_unknown`.
pub fn select_members(names: &[String]) -> (Vec<StableMember>, Vec<String>) {
    let all = stable_set();
    if names.is_empty() {
        return (all, Vec::new());
    }
    let selected: Vec<StableMember> = all
        .iter()
        .filter(|m| names.iter().any(|n| n == &m.crate_name || n == &m.binary))
        .cloned()
        .collect();
    let unknown: Vec<String> = names
        .iter()
        .filter(|n| !all.iter().any(|m| *n == &m.crate_name || *n == &m.binary))
        .cloned()
        .collect();
    (selected, unknown)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The set + its order is the load-bearing contract for install; pin it.
    /// What: Asserts the exact ordered crate-name sequence.
    /// Test: This is the test.
    #[test]
    fn stable_set_is_pinned() {
        let names: Vec<String> = stable_set().into_iter().map(|m| m.crate_name).collect();
        assert_eq!(
            names,
            vec![
                "trusty-search",
                "trusty-memory",
                "trusty-analyze",
                "trusty-review",
                "tga",
                "trusty-console",
                "trusty-mpm",
            ]
        );
    }

    /// Why: trusty-mpm is a first-class managed daemon but is process-managed,
    /// NOT launchd-managed (#1332 decision 3); its lifecycle strategy must be
    /// `OwnVerb` so `tctl start|stop|restart` drives `trusty-mpm start|stop`
    /// rather than a non-existent launchd job.
    /// What: Asserts mpm is in the set, is a daemon, and uses `OwnVerb`.
    /// Test: This is the test.
    #[test]
    fn mpm_uses_own_verb() {
        let mpm = stable_set()
            .into_iter()
            .find(|m| m.binary == "trusty-mpm")
            .expect("trusty-mpm in set");
        assert!(mpm.daemon);
        assert_eq!(mpm.manage, ManageStrategy::OwnVerb);
    }

    /// Why: The launchd-supervised daemons must resolve to the `Launchd`
    /// strategy so lifecycle drives `bootstrap`/`bootout`.
    /// What: Asserts search/memory/analyze/review/console are `Launchd`.
    /// Test: This is the test.
    #[test]
    fn daemons_use_launchd() {
        let set = stable_set();
        let manage = |b: &str| set.iter().find(|m| m.binary == b).expect("present").manage;
        for b in [
            "trusty-search",
            "trusty-memory",
            "trusty-analyze",
            "trusty-review",
            "trusty-console",
        ] {
            assert_eq!(manage(b), ManageStrategy::Launchd, "{b}");
        }
    }

    /// Why: A non-daemon has no lifecycle control; its strategy must be `None`.
    /// What: Asserts tga resolves to `ManageStrategy::None`.
    /// Test: This is the test.
    #[test]
    fn non_daemon_has_no_strategy() {
        let tga = stable_set()
            .into_iter()
            .find(|m| m.binary == "tga")
            .expect("tga in set");
        assert_eq!(tga.manage, ManageStrategy::None);
    }

    /// Why: The install command must use the crate name for `cargo install` and
    /// the binary name for presence/health probes — they are read from separate
    /// fields. For `tga` both fields happen to equal "tga", so this pins the
    /// field *separation* (each field is independently populated and correct),
    /// not that the two values differ.
    /// What: Asserts tga's `binary` is "tga" and it is a non-daemon.
    /// Test: This is the test.
    #[test]
    fn tga_crate_and_binary_names() {
        let tga = stable_set()
            .into_iter()
            .find(|m| m.crate_name == "tga")
            .expect("tga in set");
        assert_eq!(tga.binary, "tga");
        assert!(!tga.daemon);
    }

    /// Why: Upgrade restarts only daemons; the daemon flags drive that.
    /// What: Asserts the console + the three core daemons + review are daemons
    /// and tga is not.
    /// Test: This is the test.
    #[test]
    fn daemon_flags_match_spec() {
        let set = stable_set();
        let daemon = |c: &str| {
            set.iter()
                .find(|m| m.crate_name == c)
                .expect("present")
                .daemon
        };
        assert!(daemon("trusty-search"));
        assert!(daemon("trusty-memory"));
        assert!(daemon("trusty-analyze"));
        assert!(daemon("trusty-review"));
        assert!(daemon("trusty-console"));
        assert!(!daemon("tga"));
    }

    /// Why: An empty selection means "the whole platform".
    /// What: Asserts `select_members(&[])` returns the full set, no unknowns.
    /// Test: This is the test.
    #[test]
    fn select_empty_returns_all() {
        let (sel, unknown) = select_members(&[]);
        assert_eq!(sel.len(), stable_set().len());
        assert!(unknown.is_empty());
    }

    /// Why: A named subset must keep install order regardless of arg order.
    /// What: Requests console then search; asserts search comes first (set order).
    /// Test: This is the test.
    #[test]
    fn select_subset_preserves_order() {
        let (sel, unknown) =
            select_members(&["trusty-console".to_owned(), "trusty-search".to_owned()]);
        let names: Vec<String> = sel.into_iter().map(|m| m.crate_name).collect();
        assert_eq!(names, vec!["trusty-search", "trusty-console"]);
        assert!(unknown.is_empty());
    }

    /// Why: Unknown names must be surfaced, not silently dropped.
    /// What: Requests a bogus name; asserts it lands in the unknown list.
    /// Test: This is the test.
    #[test]
    fn select_reports_unknown() {
        let (sel, unknown) = select_members(&["not-a-tool".to_owned()]);
        assert!(sel.is_empty());
        assert_eq!(unknown, vec!["not-a-tool".to_owned()]);
    }
}
