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

/// Result of resolving a caller-named subset AND expanding it over the
/// [`super::dependency_graph`] runtime "requires" edges (#2036).
///
/// Why: Install needs three things from one resolution pass: the full,
/// topologically-ordered set to actually install; the unresolved names to
/// error out on; and which members were pulled in by dependency (so the CLI
/// and picker can tell the operator why).
///
/// What: `members` is the transitive closure of the resolved explicit names,
/// filtered from [`stable_set`] (so it is already in topological — dependency
/// before dependent — order); `unknown` mirrors [`select_members`]'s unknown
/// list; `added` describes members present in `members` that were not named
/// explicitly.
///
/// Test: `tests::select_transitive_expands_mpm`,
/// `tests::select_transitive_noop_for_leaf`,
/// `tests::select_transitive_idempotent_when_dep_named_explicitly`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitiveSelection {
    /// The resolved set, transitively closed, in topological install order.
    pub members: Vec<StableMember>,
    /// Names from the caller's request that matched no stable-set member.
    pub unknown: Vec<String>,
    /// Members pulled in because something explicitly requested requires them.
    pub added: Vec<super::dependency_graph::AddedMember>,
}

/// Resolve a caller-named subset AND expand it to the transitive closure of
/// its runtime dependencies (#2036).
///
/// Why: `tctl install trusty-mpm` must also bring up trusty-memory and
/// trusty-search — the daemons trusty-mpm actually needs at runtime — rather
/// than leaving the operator with a silently-incomplete stack. Scoped to
/// install/picker only (not `upgrade`/`config`/`lifecycle`, which use
/// [`select_members`] unchanged): those commands target already-installed,
/// independently-running daemons where auto-expanding the blast radius (e.g.
/// `tctl stop trusty-mpm` also stopping the shared trusty-search daemon) would
/// surprise the operator rather than help them.
///
/// What: Resolves `names` against [`stable_set`] exactly like [`select_members`]
/// (empty = all, matched by crate name or binary), then runs
/// [`super::dependency_graph::transitive_closure`] over the resolved crate
/// names and filters the master ordered list down to that closure — which is
/// already a valid topological order because every dependency edge points to a
/// crate earlier in [`stable_set`]'s list (see
/// `dependency_graph::tests::edges_precede_dependent_in_stable_set_order`).
/// `unknown` short-circuits: when any name is unrecognised, `members`/`added`
/// are left empty (mirrors [`select_members`]'s existing "unknown wins"
/// caller contract used by `install.rs`).
///
/// Test: `tests::select_transitive_expands_mpm`,
/// `tests::select_transitive_reports_unknown`,
/// `tests::select_transitive_preserves_order_with_explicit_deps`.
pub fn select_members_transitive(names: &[String]) -> TransitiveSelection {
    let all = stable_set();
    if names.is_empty() {
        return TransitiveSelection {
            members: all,
            unknown: Vec::new(),
            added: Vec::new(),
        };
    }

    let mut explicit: Vec<String> = Vec::new();
    for n in names {
        if let Some(m) = all.iter().find(|m| n == &m.crate_name || n == &m.binary) {
            if !explicit.contains(&m.crate_name) {
                explicit.push(m.crate_name.clone());
            }
        }
    }
    let unknown: Vec<String> = names
        .iter()
        .filter(|n| !all.iter().any(|m| *n == &m.crate_name || *n == &m.binary))
        .cloned()
        .collect();

    if !unknown.is_empty() {
        return TransitiveSelection {
            members: Vec::new(),
            unknown,
            added: Vec::new(),
        };
    }

    let closure = super::dependency_graph::transitive_closure(&explicit);
    let members: Vec<StableMember> = all
        .into_iter()
        .filter(|m| closure.contains(&m.crate_name))
        .collect();
    let added = super::dependency_graph::added_members(&explicit, &closure);

    TransitiveSelection {
        members,
        unknown: Vec::new(),
        added,
    }
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

    // ── select_members_transitive (#2036) ────────────────────────────────────

    /// Why: The core #2036 contract — installing trusty-mpm must transitively
    /// pull in trusty-memory and trusty-search, in topological (deps-first) order.
    /// What: Requests only trusty-mpm; asserts the resolved set is exactly
    /// [trusty-search, trusty-memory, trusty-mpm] (stable-set order) and both
    /// pulled-in members are reported in `added`.
    /// Test: This is the test.
    #[test]
    fn select_transitive_expands_mpm() {
        let sel = select_members_transitive(&["trusty-mpm".to_owned()]);
        let names: Vec<String> = sel.members.iter().map(|m| m.crate_name.clone()).collect();
        assert_eq!(names, vec!["trusty-search", "trusty-memory", "trusty-mpm"]);
        assert!(sel.unknown.is_empty());
        let mut added_names: Vec<String> = sel.added.iter().map(|a| a.crate_name.clone()).collect();
        added_names.sort();
        assert_eq!(added_names, vec!["trusty-memory", "trusty-search"]);
        for a in &sel.added {
            assert_eq!(a.required_by, vec!["trusty-mpm".to_owned()]);
        }
    }

    /// Why: The other #2036-confirmed edge — installing trusty-review alone
    /// must transitively pull in trusty-search and trusty-analyze (the #590
    /// required-context gate), in topological order.
    /// What: Requests only trusty-review; asserts the resolved set is exactly
    /// [trusty-search, trusty-analyze, trusty-review] and both pulled-in
    /// members are reported in `added`.
    /// Test: This is the test.
    #[test]
    fn select_transitive_expands_review() {
        let sel = select_members_transitive(&["trusty-review".to_owned()]);
        let names: Vec<String> = sel.members.iter().map(|m| m.crate_name.clone()).collect();
        assert_eq!(
            names,
            vec!["trusty-search", "trusty-analyze", "trusty-review"]
        );
        let mut added_names: Vec<String> = sel.added.iter().map(|a| a.crate_name.clone()).collect();
        added_names.sort();
        assert_eq!(added_names, vec!["trusty-analyze", "trusty-search"]);
    }

    /// Why: A leaf crate (no dependents) must not pull in anything extra.
    /// What: Requests only tga; asserts the resolved set is exactly `[tga]` with
    /// no `added` entries.
    /// Test: This is the test.
    #[test]
    fn select_transitive_noop_for_leaf() {
        let sel = select_members_transitive(&["tga".to_owned()]);
        let names: Vec<String> = sel.members.iter().map(|m| m.crate_name.clone()).collect();
        assert_eq!(names, vec!["tga"]);
        assert!(sel.added.is_empty());
    }

    /// Why: Naming a dependency explicitly alongside its dependent must be
    /// idempotent — no duplicate, no spurious `added` entry for the
    /// explicitly-named dependency, and order is still preserved.
    /// What: Requests `["trusty-mpm", "trusty-memory"]`; asserts the resolved
    /// set is still exactly [trusty-search, trusty-memory, trusty-mpm] and only
    /// trusty-search shows up in `added`.
    /// Test: This is the test.
    #[test]
    fn select_transitive_preserves_order_with_explicit_deps() {
        let sel = select_members_transitive(&["trusty-mpm".to_owned(), "trusty-memory".to_owned()]);
        let names: Vec<String> = sel.members.iter().map(|m| m.crate_name.clone()).collect();
        assert_eq!(names, vec!["trusty-search", "trusty-memory", "trusty-mpm"]);
        let added_names: Vec<String> = sel.added.iter().map(|a| a.crate_name.clone()).collect();
        assert_eq!(added_names, vec!["trusty-search"]);
    }

    /// Why: Unknown names must still be surfaced (not silently dropped) even
    /// though this function does dependency expansion.
    /// What: Requests a bogus name; asserts `members`/`added` are empty and the
    /// bogus name lands in `unknown`.
    /// Test: This is the test.
    #[test]
    fn select_transitive_reports_unknown() {
        let sel = select_members_transitive(&["not-a-tool".to_owned()]);
        assert!(sel.members.is_empty());
        assert!(sel.added.is_empty());
        assert_eq!(sel.unknown, vec!["not-a-tool".to_owned()]);
    }

    /// Why: An empty selection means "the whole platform" — same contract as
    /// `select_members`.
    /// What: Asserts `select_members_transitive(&[])` returns the full set, no
    /// unknowns, no added.
    /// Test: This is the test.
    #[test]
    fn select_transitive_empty_returns_all() {
        let sel = select_members_transitive(&[]);
        assert_eq!(sel.members.len(), stable_set().len());
        assert!(sel.unknown.is_empty());
        assert!(sel.added.is_empty());
    }
}
