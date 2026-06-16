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

/// One member of the stable trusty tool set.
///
/// Why: `tctl` keys three things off a member — the crates.io package name (for
/// `cargo install` / `check_crates_io`), the installed binary name (for presence
/// and health probes), and whether it is a launchd-supervised daemon (so upgrade
/// can restart it cleanly). Bundling them keeps every handler consistent.
///
/// What: `crate_name` is the cargo package; `binary` is the installed binary
/// (often equal, but `tga` differs); `daemon` marks members that run as a
/// long-lived HTTP daemon and therefore need a connection-safe restart after an
/// upgrade (`upgrade_and_restart`).
///
/// Test: `tests::stable_set_is_pinned`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StableMember {
    /// The crates.io package name (`cargo install <crate_name> --locked`).
    pub crate_name: String,
    /// The installed binary name probed on PATH (`which <binary>`).
    pub binary: String,
    /// Whether this member runs as a supervised daemon (needs restart on upgrade).
    pub daemon: bool,
}

impl StableMember {
    /// Construct a stable-set member.
    ///
    /// Why: Terse constructor keeps the [`stable_set`] table readable.
    /// What: Builds a [`StableMember`] from borrowed parts.
    /// Test: Exercised by [`stable_set`] and its tests.
    fn new(crate_name: &str, binary: &str, daemon: bool) -> Self {
        Self {
            crate_name: crate_name.to_owned(),
            binary: binary.to_owned(),
            daemon,
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
/// What: Returns the member list in install order — trusty-search,
/// trusty-memory, trusty-analyze, trusty-review, tga (binary `tga`,
/// non-daemon), and trusty-console (daemon). Library crates resolve as cargo
/// dependencies and are not listed.
///
/// Test: `tests::stable_set_is_pinned`, `tests::tga_crate_and_binary_differ`,
/// `tests::daemon_flags_match_spec`.
pub fn stable_set() -> Vec<StableMember> {
    vec![
        StableMember::new("trusty-search", "trusty-search", true),
        StableMember::new("trusty-memory", "trusty-memory", true),
        StableMember::new("trusty-analyze", "trusty-analyze", true),
        StableMember::new("trusty-review", "trusty-review", true),
        StableMember::new("tga", "tga", false),
        StableMember::new("trusty-console", "trusty-console", true),
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
            ]
        );
    }

    /// Why: `tga`'s crate name differs from most; the install command must use
    /// the crate name for `cargo install` and the binary for probes.
    /// What: Asserts tga's crate_name == binary == "tga" (both happen to match
    /// here, but the field separation is what the install path depends on).
    /// Test: This is the test.
    #[test]
    fn tga_crate_and_binary_differ() {
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
