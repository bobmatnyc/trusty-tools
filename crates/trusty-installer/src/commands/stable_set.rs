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
//! trusty-search, trusty-memory, trusty-analyze, trusty-review, tga,
//! trusty-console, trusty-mpm, and trusty-installer itself (#5805). Library
//! crates (trusty-common, trusty-embedderd, …) are pulled in automatically as
//! cargo dependencies of these binaries, so they are intentionally *not*
//! listed here.
//!
//! A member may ship more than one binary; [`StableMember::binary`] names the
//! one probed for health, and [`StableMember::binaries`] enumerates the full
//! set from the shared `trusty_common::bin_resolve` table so every alias
//! (`tctl`, `tm`, `trusty-embedderd`) resolves to its crate.
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
/// Why: `tctl` keys five things off a member — the crates.io package name (for
/// `cargo install` / `check_crates_io`), the installed binary name (for presence
/// and health probes), whether it is a supervised daemon (so upgrade can restart
/// it cleanly), HOW it is managed (launchd vs its own start/stop verb), and
/// whether the overall install run may fail/exit-nonzero when THIS member is
/// missing on the host platform. Bundling them keeps every handler consistent.
///
/// What: `crate_name` is the cargo package; `binary` is the installed binary
/// (often equal, but `tga` differs); `daemon` marks members that run as a
/// long-lived HTTP daemon and therefore need a connection-safe restart after an
/// upgrade (`upgrade_and_restart`); `manage` is the lifecycle strategy
/// (derived from `daemon` + binary by [`StableMember::new`]); `required` gates
/// the graceful-degrade policy (demo-critical fix): a REQUIRED member failing
/// to install fails the whole run (`exit 2`, `NOT VERIFIED`); an OPTIONAL
/// member failing (e.g. no prebuilt for this platform, on a host with no Rust
/// toolchain to fall back to `cargo install`) is reported as skipped and never
/// flips the overall verdict.
///
/// Test: `tests::stable_set_is_pinned`, `tests::mpm_uses_own_verb`,
/// `tests::required_vs_optional_classification`.
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
    /// Whether this member is REQUIRED for a verified install (see the field
    /// group doc above). `false` means OPTIONAL: install/verify failures for
    /// this member degrade gracefully instead of failing the overall run.
    pub required: bool,
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
    /// [`ManageStrategy::Launchd`]. `required` is passed straight through —
    /// see [`stable_set`] for the current REQUIRED/OPTIONAL assignment.
    /// Test: Exercised by [`stable_set`] and `tests::mpm_uses_own_verb`.
    ///
    /// #4246: the strategy derivation itself moved out to
    /// [`manage_strategy_for`], because `tctl up`'s `BootMember` (which carries no
    /// `manage` field) now needs the SAME rule to route through the one shared
    /// probe — two copies of "is this member launchd or self-managed?" is exactly
    /// how the two divergent probes came about.
    fn new(crate_name: &str, binary: &str, daemon: bool, required: bool) -> Self {
        Self {
            crate_name: crate_name.to_owned(),
            binary: binary.to_owned(),
            daemon,
            manage: manage_strategy_for(binary, daemon),
            required,
        }
    }

    /// Every binary `cargo install <crate_name>` puts in the bin dir.
    ///
    /// Why (#5805): [`StableMember::binary`] holds ONE name, but three members
    /// ship more than one — trusty-installer (`trusty-installer` + `tctl`),
    /// trusty-mpm (`tm` + `trusty-mpm`), trusty-search (`trusty-search` +
    /// `trusty-embedderd`). Resolving `tctl install tctl` against the single
    /// field reported `unknown member(s): tctl` for a binary the installer
    /// itself writes. The rule already exists once, in
    /// [`trusty_common::bin_resolve::installed_binaries`] — the same table the
    /// tarball allowlist and the cargo ownership guard read — so this delegates
    /// rather than adding a second copy (CLAUDE.md, common entry point).
    ///
    /// What: the shared table's row for `crate_name`, which falls back to
    /// `[crate_name]` for the single-binary members.
    ///
    /// Test: `tests::binaries_covers_every_alias_binary`,
    /// `tests::binaries_are_unique_across_the_stable_set`.
    pub fn binaries(&self) -> Vec<String> {
        trusty_common::bin_resolve::installed_binaries(&self.crate_name)
    }

    /// Whether this member IS the running control plane — `tctl` itself.
    ///
    /// Why (#5805): membership put the installer in the same list every
    /// per-member fan-out iterates, and one of those fan-outs spawns the
    /// member's binary. `tctl config` spawned `trusty-installer config --json`,
    /// which enumerated the set and spawned it again, with
    /// [`super::probe::spawn_member_json`] blocking on each child. Any surface
    /// that runs a member's binary must ask this question first.
    ///
    /// What: `true` when the member's crate is this crate, or when it claims
    /// any binary this crate installs — so the `tctl` alias is caught even if
    /// a future row spells the crate differently. Both sides read
    /// [`trusty_common::bin_resolve::installed_binaries`], the shared table
    /// [`StableMember::binaries`] already delegates to.
    ///
    /// Test: `tests::exactly_one_member_is_the_control_plane`.
    pub fn is_control_plane(&self) -> bool {
        let me = env!("CARGO_PKG_NAME");
        self.crate_name == me || {
            let own = trusty_common::bin_resolve::installed_binaries(me);
            self.binaries().iter().any(|b| own.contains(b))
        }
    }

    /// Whether `tctl` may spawn this member's binary to forward a contract verb
    /// (`<binary> config --json`, `<binary> version --json`).
    ///
    /// Why (#5805): forwarding to itself is unbounded recursion, not a config
    /// read — see [`StableMember::is_control_plane`]. `tctl`'s own answer to
    /// `config` is the aggregate it is building, so there is nothing to fetch.
    ///
    /// What: the negation of [`StableMember::is_control_plane`].
    ///
    /// Test: `tests::the_control_plane_forwards_no_contract_verb`,
    /// `super::config::tests::fan_out_never_targets_our_own_binaries`.
    pub fn forwards_contract_verbs(&self) -> bool {
        !self.is_control_plane()
    }

    /// Whether `name` refers to this member — its crate name or any of its
    /// installed binaries.
    ///
    /// Why (#5805): the one predicate [`select_members`] and
    /// [`select_members_transitive`] share, so the two resolvers cannot drift
    /// on which spellings are accepted.
    ///
    /// What: matches `crate_name`, [`StableMember::binary`], or any name in
    /// [`StableMember::binaries`].
    ///
    /// Test: `tests::select_resolves_alias_binary_names`.
    fn matches_name(&self, name: &str) -> bool {
        name == self.crate_name || name == self.binary || self.binaries().iter().any(|b| b == name)
    }
}

/// Fold a per-row verdict over the REQUIRED subset, falling back to every row
/// when the selection contains none.
///
/// Why (#5806): `filter(|r| required(r)).all(ok)` over zero required rows is
/// vacuously `true`, so a selection of only OPTIONAL members reported success
/// no matter what happened. The rule was written twice — once in
/// [`super::install_report::InstallReport::build`] for `all_ok`, once in
/// [`super::verify_tail::VerifyTailReport::build`] for `verified` — and the
/// first was fixed while the second kept failing open. It lives here now, so a
/// third consumer inherits the fix rather than the bug.
///
/// "Degrade gracefully" only means anything relative to a required core. A
/// selection with no required core is one where the operator asked for exactly
/// these rows, so every one of them gates.
///
/// # Postconditions
/// - With any required row, only required rows gate.
/// - With no required row, every row gates.
/// - An empty `rows` returns `true`. The two callers disagree about what no
///   rows MEANS — for `install` it is "nothing was installed", for the verify
///   tail it is "this selection contains no daemon to probe" — so emptiness is
///   each caller's to judge, not this fold's.
///
/// Test: `tests::required_gate_truth_table`,
/// `tests::required_gate_over_no_rows_is_the_callers_problem`.
pub(super) fn required_gate<T>(
    rows: &[T],
    required: impl Fn(&T) -> bool,
    ok: impl Fn(&T) -> bool,
) -> bool {
    let any_required = rows.iter().any(&required);
    rows.iter().filter(|r| required(r) || !any_required).all(ok)
}

/// Derive a member's lifecycle [`ManageStrategy`] from its binary name and
/// daemon flag.
///
/// Why: `tctl status`/`stack`/the verify tail read `StableMember::manage`, but
/// `tctl up` works from a `BootMember`, which has no such field — so routing `up`
/// through the one shared health probe (#4246) needs the rule as a standalone
/// function rather than a `StableMember` constructor detail. Keeping it in ONE
/// place is the point: the divergent `SystemRunner::probe` this replaces existed
/// precisely because "how is this member managed?" was answered twice.
/// What: a non-daemon is [`ManageStrategy::None`]; trusty-mpm is
/// [`ManageStrategy::OwnVerb`] (process-managed, not launchd); every other daemon
/// is [`ManageStrategy::Launchd`].
/// Test: `tests::manage_strategy_for_matches_the_stable_set`.
pub fn manage_strategy_for(binary: &str, daemon: bool) -> ManageStrategy {
    if !daemon {
        ManageStrategy::None
    } else if binary == "trusty-mpm" {
        ManageStrategy::OwnVerb
    } else {
        ManageStrategy::Launchd
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
/// non-daemon), trusty-console (daemon), trusty-mpm (the orchestrator,
/// process-managed via its own `start`/`stop` verbs), and trusty-installer
/// itself, LAST. Library crates resolve as cargo dependencies and are not
/// listed.
///
/// #5805 — why the installer is a member, and why it is last: the tool whose
/// job is placing trusty-* binaries could not place its own, so
/// `tctl install trusty-installer` answered `unknown member(s)`. It is a
/// non-daemon (`daemon: false`), so every daemon-shaped surface skips it by
/// the filter it already applies to `tga`: `daemon_members`, the verify tail,
/// `plans_service_bootstrap`, and `order_for` in `lifecycle`. It sorts LAST
/// because installing it REPLACES the running binary — doing that after the
/// rest of the stack has landed means a failure there costs nothing already
/// installed. The replacement itself is safe: both write paths rename
/// atomically over the destination (`download::fetch::place_binaries`, or
/// `cargo install`), so the running process keeps its open inode and the
/// macOS cdhash cache never sees a partially-overwritten file — the hazard
/// CLAUDE.md warns about is `cp`, which neither path uses.
///
/// REQUIRED vs OPTIONAL (graceful-degrade policy, demo-critical fix): REQUIRED
/// = trusty-mpm + its runtime deps trusty-search + trusty-memory +
/// trusty-review — a from-scratch install on a Tier-1 platform must always be
/// able to bring these up. OPTIONAL = trusty-analyze, trusty-console, tga,
/// trusty-installer — members that may lack a prebuilt for a given platform on
/// a host with no Rust toolchain to fall back to; their absence must not fail
/// the run or print a scary FAILED/exit-2 verdict. trusty-installer is
/// OPTIONAL for a second reason: a bulk `tctl install` runs FROM a working
/// installer, so failing to refresh that copy leaves the stack usable. Naming
/// it explicitly is the different case, and [`super::install_report`]'s
/// `all_ok` derivation handles it — a selection with no REQUIRED member gates
/// on every member instead of vacuously passing.
///
/// NOTE for future editors: a separate lane may add `trusty-agents` to this
/// set as REQUIRED — insert it with `required: true` in topological position;
/// no other code needs to change (`install`/`verify_tail`'s all_ok/verified
/// derivations already key off the `required` field, not a hardcoded list).
///
/// Test: `tests::stable_set_is_pinned`, `tests::tga_crate_and_binary_names`,
/// `tests::daemon_flags_match_spec`, `tests::mpm_uses_own_verb`,
/// `tests::required_vs_optional_classification`.
pub fn stable_set() -> Vec<StableMember> {
    vec![
        StableMember::new("trusty-search", "trusty-search", true, true),
        StableMember::new("trusty-memory", "trusty-memory", true, true),
        StableMember::new("trusty-analyze", "trusty-analyze", true, false),
        StableMember::new("trusty-review", "trusty-review", true, true),
        StableMember::new("tga", "tga", false, false),
        StableMember::new("trusty-console", "trusty-console", true, false),
        StableMember::new("trusty-mpm", "trusty-mpm", true, true),
        // #5805: the control plane installs itself. Last, non-daemon, OPTIONAL
        // — see this function's doc for all three reasons.
        StableMember::new("trusty-installer", "trusty-installer", false, false),
    ]
}

/// The daemon subset of the stable set, in install order.
///
/// Why: "which members are daemons?" is read by three surfaces — `tctl stack
/// doctor`, `tctl stack health`, and `vmtest-harness`, which DERIVES its
/// daemon-liveness set from doctor's `--json` member table rather than
/// transcribing one. It had two independent copies of the same
/// `filter(|m| m.daemon)` expression, so narrowing the rule in one place was a
/// silent divergence rather than a reviewed decision. Same motivation as
/// [`manage_strategy_for`] (#4246): the rule lives once, and the pinning test
/// sits next to it.
///
/// What: [`stable_set`] filtered to `daemon == true`, preserving the
/// topological install order.
///
/// Test: `tests::daemon_members_is_pinned` pins the resulting names.
pub fn daemon_members() -> Vec<StableMember> {
    stable_set().into_iter().filter(|m| m.daemon).collect()
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
        .filter(|m| names.iter().any(|n| m.matches_name(n)))
        .cloned()
        .collect();
    let unknown: Vec<String> = names
        .iter()
        .filter(|n| !all.iter().any(|m| m.matches_name(n)))
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
        if let Some(m) = all.iter().find(|m| m.matches_name(n)) {
            if !explicit.contains(&m.crate_name) {
                explicit.push(m.crate_name.clone());
            }
        }
    }
    let unknown: Vec<String> = names
        .iter()
        .filter(|n| !all.iter().any(|m| m.matches_name(n)))
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
                "trusty-installer",
            ]
        );
    }

    /// Why (#5805): the whole point — `tctl install trusty-installer` used to
    /// answer `unknown member(s): trusty-installer`, so the tool that places
    /// trusty-* binaries could not place its own. Pin membership AND the three
    /// properties that keep it from turning every daemon-shaped surface into
    /// nonsense: it is not a daemon, it has no lifecycle strategy, and it sorts
    /// last so a bulk install replaces the running binary only after
    /// everything else has landed.
    /// What: asserts the member resolves, is `daemon: false` /
    /// `ManageStrategy::None`, and is the final entry in install order.
    /// Test: This is the test.
    #[test]
    fn installer_is_a_non_daemon_member_installed_last() {
        let set = stable_set();
        let installer = set
            .iter()
            .find(|m| m.crate_name == "trusty-installer")
            .expect("trusty-installer must be a stable-set member (#5805)");
        assert!(!installer.daemon, "the installer is not a daemon");
        assert_eq!(installer.manage, ManageStrategy::None);
        assert_eq!(
            set.last().map(|m| m.crate_name.as_str()),
            Some("trusty-installer"),
            "self-replacement must come after the rest of the stack has landed"
        );
    }

    /// Why (#5805): every fan-out that SPAWNS a member's binary must be able to
    /// ask "is this me?" and get exactly one yes. Two would silently drop a
    /// real member from `tctl config`; zero is the recursion itself.
    /// What: asserts exactly one member of the shipped set is the control
    /// plane, that it is trusty-installer, and that the `tctl` alias answers
    /// the same way even though the crate name does not spell it.
    /// Test: This is the test.
    #[test]
    fn exactly_one_member_is_the_control_plane() {
        let set = stable_set();
        let selves: Vec<&str> = set
            .iter()
            .filter(|m| m.is_control_plane())
            .map(|m| m.crate_name.as_str())
            .collect();
        assert_eq!(selves, vec!["trusty-installer"]);

        let alias = StableMember::new("trusty-installer", "tctl", false, false);
        assert!(
            alias.is_control_plane(),
            "the `tctl` alias must resolve to self through the shared table too"
        );
    }

    /// Why (#5805): the predicate `config::partition_forwardable` reads. A
    /// member that forwards nothing must be the control plane and nothing else,
    /// or a daemon stops reporting its config with no error anywhere.
    /// What: asserts `forwards_contract_verbs` is the exact negation of
    /// `is_control_plane` for every shipped member.
    /// Test: This is the test.
    #[test]
    fn the_control_plane_forwards_no_contract_verb() {
        for m in stable_set() {
            assert_eq!(
                m.forwards_contract_verbs(),
                !m.is_control_plane(),
                "{} disagrees with itself about forwarding",
                m.crate_name
            );
        }
    }

    /// Why (#5806): the gating rule two reports derive their verdict from.
    /// `filter(required).all(…)` over zero required rows is vacuously true, and
    /// that shape shipped twice — fixed in `InstallReport::build`, left in
    /// `VerifyTailReport::build`. Pin the truth table where the rule now lives.
    /// What: asserts a failing OPTIONAL row gates when nothing is REQUIRED, and
    /// stops gating as soon as a REQUIRED row joins the selection.
    /// Test: This is the test.
    #[test]
    fn required_gate_truth_table() {
        // (required, ok)
        let rows = |v: &[(bool, bool)]| v.to_vec();
        let req = |r: &(bool, bool)| r.0;
        let ok = |r: &(bool, bool)| r.1;

        // No required row: every row gates, so a failed optional fails the run.
        assert!(!required_gate(&rows(&[(false, false)]), req, ok));
        assert!(required_gate(&rows(&[(false, true)]), req, ok));
        assert!(!required_gate(
            &rows(&[(false, true), (false, false)]),
            req,
            ok
        ));

        // Any required row: only required rows gate — graceful degrade.
        assert!(required_gate(
            &rows(&[(true, true), (false, false)]),
            req,
            ok
        ));
        assert!(!required_gate(
            &rows(&[(true, false), (false, true)]),
            req,
            ok
        ));
    }

    /// Why (#5806): the two callers disagree about what "no rows" means, so the
    /// fold must not decide for them — `install` treats it as "nothing was
    /// installed" (failure), the verify tail as "no daemon in this selection"
    /// (defer to `ensure_ok`). Pin that the fold itself stays neutral, so a
    /// future caller reads the postcondition rather than inheriting a guess.
    /// What: asserts the fold returns `true` over zero rows.
    /// Test: This is the test.
    #[test]
    fn required_gate_over_no_rows_is_the_callers_problem() {
        let empty: Vec<(bool, bool)> = Vec::new();
        assert!(required_gate(&empty, |r| r.0, |r| r.1));
    }

    /// Why (#5805): `daemon_members` is the set `tctl stack doctor`,
    /// `tctl stack health`, and vmtest-harness enumerate. Adding a member to
    /// `stable_set` must not silently enrol the installer as a daemon those
    /// surfaces then probe for an HTTP `/health` it does not serve.
    /// What: asserts the daemon set still holds exactly the six daemons.
    /// Test: This is the test.
    #[test]
    fn installer_is_absent_from_the_daemon_set() {
        let names: Vec<String> = daemon_members().into_iter().map(|m| m.crate_name).collect();
        assert!(
            !names.contains(&"trusty-installer".to_owned()),
            "the installer must never reach a daemon-shaped surface: {names:?}"
        );
        assert_eq!(names.len(), 6, "daemon set unchanged by #5805: {names:?}");
    }

    /// Why (#5805): the crate ships TWO binaries, and `tctl` is the one
    /// operators actually type — `tctl install tctl` answered
    /// `unknown member(s): tctl`. The alias set comes from the shared
    /// `trusty_common::bin_resolve` table, so this also covers trusty-mpm's
    /// `tm` and trusty-search's `trusty-embedderd`.
    /// What: asserts every member's `binaries()` contains its own `binary`,
    /// then pins the three multi-binary rows by name.
    /// Test: This is the test.
    #[test]
    fn binaries_covers_every_alias_binary() {
        for m in stable_set() {
            assert!(
                m.binaries().contains(&m.binary),
                "{} must list its own probe binary",
                m.crate_name
            );
        }
        let set = stable_set();
        let bins = |c: &str| {
            set.iter()
                .find(|m| m.crate_name == c)
                .expect("present")
                .binaries()
        };
        assert!(bins("trusty-installer").contains(&"tctl".to_owned()));
        assert!(bins("trusty-mpm").contains(&"tm".to_owned()));
        assert!(bins("trusty-search").contains(&"trusty-embedderd".to_owned()));
    }

    /// Why (#5805): [`StableMember::matches_name`] resolves a caller's name
    /// against every binary a member ships. If two members claimed the same
    /// binary name, resolution would silently pick whichever sorted first in
    /// [`stable_set`] and the operator would install the wrong crate. Nothing
    /// in the shared table enforces uniqueness, so pin it here where the
    /// ambiguity would bite.
    /// What: collects every name each member answers to and asserts no
    /// duplicates across the whole set.
    /// Test: This is the test.
    #[test]
    fn binaries_are_unique_across_the_stable_set() {
        let mut seen: Vec<(String, String)> = Vec::new();
        for m in stable_set() {
            for b in m.binaries() {
                if let Some((owner, _)) = seen.iter().find(|(_, name)| *name == b) {
                    panic!(
                        "binary {b:?} is claimed by both {owner} and {}",
                        m.crate_name
                    );
                }
                seen.push((m.crate_name.clone(), b));
            }
        }
    }

    /// Why (#5805): the resolver used to match only `crate_name` and the
    /// single `binary` field, so the alias binaries a member genuinely
    /// installs were rejected as unknown.
    /// What: asserts `tctl`, `tm`, and `trusty-embedderd` each resolve to
    /// their owning crate through both resolvers, with no unknowns.
    /// Test: This is the test.
    #[test]
    fn select_resolves_alias_binary_names() {
        for (typed, expected) in [
            ("tctl", "trusty-installer"),
            ("tm", "trusty-mpm"),
            ("trusty-embedderd", "trusty-search"),
        ] {
            let (sel, unknown) = select_members(&[typed.to_owned()]);
            assert!(unknown.is_empty(), "{typed} must resolve: {unknown:?}");
            assert_eq!(sel.len(), 1, "{typed} must resolve to exactly one member");
            assert_eq!(sel[0].crate_name, expected);

            let t = select_members_transitive(&[typed.to_owned()]);
            assert!(t.unknown.is_empty(), "{typed} must resolve transitively");
            assert!(
                t.members.iter().any(|m| m.crate_name == expected),
                "{typed} must expand to include {expected}"
            );
        }
    }

    /// Why (#5805): the acceptance case. `tctl install trusty-installer`
    /// resolved to nothing; it must now resolve to exactly the installer, with
    /// no transitive expansion (it has no runtime dependencies on other
    /// members, so it must not silently drag the stack in).
    /// What: asserts the resolved set is exactly `[trusty-installer]` and
    /// `added` is empty.
    /// Test: This is the test.
    #[test]
    fn select_transitive_installer_pulls_in_nothing() {
        let sel = select_members_transitive(&["trusty-installer".to_owned()]);
        let names: Vec<String> = sel.members.iter().map(|m| m.crate_name.clone()).collect();
        assert_eq!(names, vec!["trusty-installer"]);
        assert!(sel.unknown.is_empty());
        assert!(
            sel.added.is_empty(),
            "the installer requires no other member at runtime: {:?}",
            sel.added
        );
    }

    /// Why: [`daemon_members`] is the rule three surfaces enumerate — `tctl
    /// stack doctor`, `tctl stack health`, and `vmtest-harness`, whose
    /// `_verify_daemon_set` (`vmtest-harness/lib/verify.sh:1060`) DERIVES its
    /// liveness set from doctor's `--json` member table instead of transcribing
    /// one. Narrowing the rule therefore shrinks that oracle silently: the
    /// dropped daemon stops being probed, its name lands on the already-noisy
    /// `unreported` log line, and the run still reports PASS. Pin the NAMES so
    /// a narrowing is a deliberate, reviewed decision, not accidental drift —
    /// same reasoning as `required_vs_optional_classification`. Asserting the
    /// filter expression back at itself would be a tautology and would catch
    /// nothing.
    /// What: asserts the daemon set is exactly these six crate names, in
    /// stable-set order — the seven members minus `tga`, the one non-daemon.
    /// Test: This is the test.
    #[test]
    fn daemon_members_is_pinned() {
        let names: Vec<String> = daemon_members().into_iter().map(|m| m.crate_name).collect();
        assert_eq!(
            names,
            vec![
                "trusty-search",
                "trusty-memory",
                "trusty-analyze",
                "trusty-review",
                "trusty-console",
                "trusty-mpm",
            ]
        );
    }

    /// Why: trusty-mpm is a first-class managed daemon but is process-managed,
    /// NOT launchd-managed (#1332 decision 3); its lifecycle strategy must be
    /// `OwnVerb` so `tctl start|stop|restart` drives `trusty-mpm start|stop`
    /// rather than a non-existent launchd job.
    /// Why (#4246): `manage_strategy_for` is now the single rule two callers
    /// share — `StableMember::new` and `tctl up`'s `SystemRunner::probe`. If they
    /// ever disagreed, `tctl up` would probe trusty-mpm over HTTP while
    /// `tctl status` reported it `unknown`, which is the divergence this
    /// extraction exists to make impossible.
    /// What: asserts the function agrees with every `manage` field the canonical
    /// [`stable_set`] carries, plus the non-daemon case.
    /// Test: This is the test.
    #[test]
    fn manage_strategy_for_matches_the_stable_set() {
        for m in stable_set() {
            assert_eq!(
                manage_strategy_for(&m.binary, m.daemon),
                m.manage,
                "{} disagrees with its own stable-set entry",
                m.binary
            );
        }
        assert_eq!(
            manage_strategy_for("trusty-mpm", true),
            ManageStrategy::OwnVerb
        );
        assert_eq!(
            manage_strategy_for("trusty-search", true),
            ManageStrategy::Launchd
        );
        assert_eq!(manage_strategy_for("tga", false), ManageStrategy::None);
    }

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

    /// Why: The graceful-degrade policy (demo-critical fix) reads `required`
    /// off each member; pin the exact REQUIRED/OPTIONAL split so a future
    /// edit here is a deliberate, reviewed decision, not an accidental drift.
    /// What: Asserts trusty-mpm/trusty-search/trusty-memory/trusty-review are
    /// `required: true` and trusty-analyze/trusty-console/tga are
    /// `required: false`.
    /// Test: This is the test.
    #[test]
    fn required_vs_optional_classification() {
        let set = stable_set();
        let required = |c: &str| {
            set.iter()
                .find(|m| m.crate_name == c)
                .expect("present")
                .required
        };
        for c in [
            "trusty-mpm",
            "trusty-search",
            "trusty-memory",
            "trusty-review",
        ] {
            assert!(required(c), "{c} must be REQUIRED");
        }
        for c in [
            "trusty-analyze",
            "trusty-console",
            "tga",
            // #5805: a bulk install runs FROM a working installer, so failing
            // to refresh that copy leaves the stack usable. Naming it
            // explicitly still exits nonzero — see
            // `install_report::InstallReport::build`.
            "trusty-installer",
        ] {
            assert!(!required(c), "{c} must be OPTIONAL");
        }
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
