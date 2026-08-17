//! `tctl config [<members>…]` — read-only effective merged config per member.
//!
//! Why: Surfaces the config each member is actually running with — system +
//! project layers merged, secrets redacted by the member itself (DOC-1 D8 /
//! DOC-3 §7). Read-only; never mutates.
//!
//! What: fans `<binary> config --json` out to each selected member (via
//! `probe::spawn_member_json`), collects the per-member envelopes (or the error
//! when a member is absent / non-conformant), and renders a human summary or a
//! `--json` aggregate object keyed by member.
//!
//! Test: `tests` covers the aggregate shaping (`ConfigAggregate`) and the
//! unknown-member guard; the `config --json` spawn is side-effecting.

use serde::Serialize;

use super::probe::spawn_member_json;
use super::stable_set::{select_members, StableMember};
use crate::output::render_json;

/// Split a resolved selection into the members `tctl` may spawn and the names
/// it must not.
///
/// Why (#5805): once trusty-installer joined the stable set, this fan-out
/// enumerated it and ran `trusty-installer config --json` — which is `tctl
/// config`, which enumerates and spawns again. `spawn_member_json` blocks on
/// `Command::output()`, so every level waited on a child that never exits, and
/// bare `tctl config` is the documented "all members" form. A predicate on the
/// member is what makes the exclusion checkable without spawning anything.
///
/// What: partitions on [`StableMember::forwards_contract_verbs`], returning the
/// spawn targets and the crate names skipped for being the control plane.
///
/// Test: `tests::fan_out_never_targets_our_own_binaries`,
/// `tests::fan_out_over_the_whole_set_skips_only_the_installer`.
fn partition_forwardable(selected: &[StableMember]) -> (Vec<StableMember>, Vec<String>) {
    let (forward, skipped): (Vec<_>, Vec<_>) = selected
        .iter()
        .cloned()
        .partition(StableMember::forwards_contract_verbs);
    (forward, skipped.into_iter().map(|m| m.crate_name).collect())
}

/// One member's config-fetch outcome.
///
/// Why: a typed per-member record keeps the `--json` aggregate stable + testable
/// and distinguishes a successful envelope from a fetch error.
/// What: `member` crate name; `ok` whether the envelope was obtained; `config`
/// the parsed envelope (present iff `ok`); `error` the failure message otherwise.
/// Test: `tests::aggregate_serialises`.
#[derive(Clone, Debug, Serialize)]
pub struct MemberConfig {
    /// Crate name.
    pub member: String,
    /// Whether `config --json` was obtained.
    pub ok: bool,
    /// The parsed config envelope (present iff `ok`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    /// Failure message (present iff not `ok`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The aggregate config report.
///
/// Why: `--json` consumers want every member's config in one object.
/// What: holds the per-member records + whether every fetch succeeded.
/// Test: `tests::aggregate_serialises`, `tests::aggregate_all_ok`.
#[derive(Clone, Debug, Serialize)]
pub struct ConfigAggregate {
    /// Fixed command tag.
    pub command: &'static str,
    /// Per-member config outcomes in stable-set order.
    pub members: Vec<MemberConfig>,
    /// Members resolved but never spawned because they are the control plane
    /// itself (#5805) — see [`partition_forwardable`]. Absent when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
    /// Whether every member's config was obtained.
    pub all_ok: bool,
}

impl ConfigAggregate {
    /// Build the aggregate and derive `all_ok`.
    ///
    /// Why: one place derives the verdict so JSON + exit code agree.
    /// What: `all_ok = every member ok`. A skipped member has no config to
    /// obtain, so it neither passes nor fails this.
    /// Test: `tests::aggregate_all_ok`.
    fn build(members: Vec<MemberConfig>, skipped: Vec<String>) -> Self {
        let all_ok = members.iter().all(|m| m.ok);
        Self {
            command: "config",
            members,
            skipped,
            all_ok,
        }
    }

    /// Process exit code: 0 all obtained, 2 any failed.
    ///
    /// Why: scripts branch on this.
    /// What: `0` if `all_ok`, else `2`.
    /// Test: `tests::aggregate_all_ok`.
    fn exit_code(&self) -> i32 {
        if self.all_ok {
            0
        } else {
            2
        }
    }
}

/// Handle `tctl config [<members>…]`.
///
/// Why: Phase-2 entry point — show each selected member's effective config.
/// What: resolves members (empty = all; unknown → exit 3), drops the control
/// plane from the spawn list ([`partition_forwardable`], #5805), fetches each
/// remaining member's `config --json`, builds + renders the aggregate, returns
/// the exit code. A selection that names ONLY the control plane is exit 3: it
/// resolves to a real member, but that member has no config envelope to
/// forward, and reporting an empty aggregate as exit 0 would be the same
/// vacuous success #5806 fixed elsewhere.
/// Test: `tests::run_naming_only_the_installer_is_a_usage_error`; the spawn
/// itself is side-effecting and the aggregation is tested via `ConfigAggregate`.
pub fn run(members: &[String], json: bool) -> i32 {
    let (selected, unknown) = select_members(members);
    if !unknown.is_empty() {
        let msg = format!("unknown member(s): {}", unknown.join(", "));
        if json {
            let _ = render_json(&serde_json::json!({ "command": "config", "error": msg }));
        } else {
            eprintln!("tctl config: {msg}");
        }
        return 3;
    }

    let (forward, skipped) = partition_forwardable(&selected);
    if forward.is_empty() {
        let msg = format!(
            "{} is the control plane — `tctl config` IS its config surface, so there \
             is nothing to forward to it. Name a member, or run `tctl config` bare.",
            skipped.join(", ")
        );
        if json {
            let _ = render_json(&serde_json::json!({ "command": "config", "error": msg }));
        } else {
            eprintln!("tctl config: {msg}");
        }
        return 3;
    }

    let records: Vec<MemberConfig> = forward
        .iter()
        .map(|m| match spawn_member_json(&m.binary, "config") {
            Ok(v) => MemberConfig {
                member: m.crate_name.clone(),
                ok: true,
                config: Some(v),
                error: None,
            },
            Err(e) => MemberConfig {
                member: m.crate_name.clone(),
                ok: false,
                config: None,
                error: Some(e.to_string()),
            },
        })
        .collect();

    let aggregate = ConfigAggregate::build(records, skipped);
    if json {
        if render_json(&aggregate).is_err() {
            eprintln!("tctl config: failed to write JSON output");
            return 1;
        }
    } else {
        print_human(&aggregate);
    }
    aggregate.exit_code()
}

/// Render the human-readable config summary.
///
/// Why: the full envelopes are verbose; the human view summarises per member and
/// surfaces errors, directing scripted consumers to `--json` for the full data.
/// What: prints one line per member (ok / error) plus a hint.
/// Test: side-effect-only; the data is tested via the report structs.
fn print_human(aggregate: &ConfigAggregate) {
    println!("tctl config — effective config per member (use --json for full envelopes)");
    for m in &aggregate.members {
        if m.ok {
            let keys = m
                .config
                .as_ref()
                .and_then(|c| c.as_object())
                .map(|o| o.len())
                .unwrap_or(0);
            println!("  {:<18} ok ({keys} top-level keys)", m.member);
        } else {
            let err = m.error.as_deref().unwrap_or("unknown error");
            println!("  {:<18} FAILED: {err}", m.member);
        }
    }
    for name in &aggregate.skipped {
        println!("  {name:<18} skipped (this is the control plane)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_member(name: &str) -> MemberConfig {
        MemberConfig {
            member: name.to_owned(),
            ok: true,
            config: Some(serde_json::json!({ "scope": "all", "key": "value" })),
            error: None,
        }
    }

    /// Why: the JSON aggregate is a contract; pin its shape including the
    /// per-member nested config envelope.
    /// What: builds an aggregate and asserts keys.
    /// Test: This is the test.
    #[test]
    fn aggregate_serialises() {
        let a = ConfigAggregate::build(vec![ok_member("trusty-search")], Vec::new());
        let v = serde_json::to_value(&a).expect("serialises");
        assert_eq!(v["command"], "config");
        assert_eq!(v["all_ok"], true);
        assert_eq!(v["members"][0]["member"], "trusty-search");
        assert_eq!(v["members"][0]["config"]["scope"], "all");
        // A successful member must not carry an `error` field.
        assert!(v["members"][0].get("error").is_none());
    }

    /// Why: `all_ok` must be false if any member failed.
    /// What: mixes ok + failed; asserts `all_ok = false` and exit 2.
    /// Test: This is the test.
    #[test]
    fn aggregate_all_ok() {
        let a = ConfigAggregate::build(
            vec![
                ok_member("a"),
                MemberConfig {
                    member: "b".to_owned(),
                    ok: false,
                    config: None,
                    error: Some("not installed".to_owned()),
                },
            ],
            Vec::new(),
        );
        assert!(!a.all_ok);
        assert_eq!(a.exit_code(), 2);

        let all = ConfigAggregate::build(vec![ok_member("a")], Vec::new());
        assert!(all.all_ok);
        assert_eq!(all.exit_code(), 0);
    }

    /// Why: an unknown member must be a clean error (exit 3), not a silent skip.
    /// What: calls `run` with a bogus member in JSON mode; asserts exit 3.
    /// Test: This is the test.
    #[test]
    fn run_unknown_member_is_error() {
        assert_eq!(run(&["not-a-real-tool".to_owned()], true), 3);
    }

    /// Why (#5805): THE regression. Bare `tctl config` is the documented "all
    /// members" form, and membership put trusty-installer in that list — so the
    /// fan-out spawned `trusty-installer config --json`, which is this same
    /// command, which spawned it again. `spawn_member_json` uses
    /// `Command::output()` with no timeout, so every level blocked forever on a
    /// child that never exits. Asserting on the SPAWN LIST rather than on
    /// `run`'s behaviour is what makes this checkable without actually forking:
    /// no target may be a binary this crate itself installs.
    /// What: partitions the full stable set and asserts no forwarding target's
    /// binary — or any binary its crate places — appears in this crate's own
    /// `bin_resolve` row.
    /// Test: This is the test.
    #[test]
    fn fan_out_never_targets_our_own_binaries() {
        let (forward, _) = partition_forwardable(&super::super::stable_set::stable_set());
        let ours = trusty_common::bin_resolve::installed_binaries(env!("CARGO_PKG_NAME"));
        assert!(
            !ours.is_empty(),
            "the shared table must know this crate's binaries, or the guard proves nothing"
        );
        for m in &forward {
            for b in std::iter::once(m.binary.clone()).chain(m.binaries()) {
                assert!(
                    !ours.contains(&b),
                    "`tctl config` would spawn `{b} config --json`, which is itself — \
                     unbounded recursion (member {})",
                    m.crate_name
                );
            }
        }
    }

    /// Why (#5805): the exclusion must be exactly one member wide. Dropping a
    /// real daemon from the fan-out would silently stop reporting its config,
    /// which reads as "that member has no config" rather than as a bug.
    /// What: asserts the whole stable set partitions into every member except
    /// trusty-installer, plus exactly `["trusty-installer"]` skipped.
    /// Test: This is the test.
    #[test]
    fn fan_out_over_the_whole_set_skips_only_the_installer() {
        let all = super::super::stable_set::stable_set();
        let (forward, skipped) = partition_forwardable(&all);
        assert_eq!(skipped, vec!["trusty-installer".to_owned()]);
        assert_eq!(forward.len(), all.len() - 1);
        let names: Vec<&str> = forward.iter().map(|m| m.crate_name.as_str()).collect();
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
            ],
            "the fan-out must lose the control plane and nothing else"
        );
    }

    /// Why (#5805): `tctl config trusty-installer` resolves to a real member
    /// with nothing to forward. Returning exit 0 over an empty aggregate would
    /// be a vacuous success in the same family as #5806 — the operator asked a
    /// question and got a silent, successful nothing.
    /// What: asserts exit 3 for both spellings the crate answers to. The guard
    /// fires on an EMPTY forward list, never on the installer merely being
    /// present, so a mixed selection is unaffected — that half is covered
    /// without spawning by `fan_out_over_the_whole_set_skips_only_the_installer`.
    /// Test: This is the test.
    #[test]
    fn run_naming_only_the_installer_is_a_usage_error() {
        for name in ["trusty-installer", "tctl"] {
            assert_eq!(
                run(&[name.to_owned()], true),
                3,
                "`tctl config {name}` must say why, not report an empty success"
            );
        }
    }
}
