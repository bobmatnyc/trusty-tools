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
use super::stable_set::select_members;
use crate::output::render_json;

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
    /// Whether every member's config was obtained.
    pub all_ok: bool,
}

impl ConfigAggregate {
    /// Build the aggregate and derive `all_ok`.
    ///
    /// Why: one place derives the verdict so JSON + exit code agree.
    /// What: `all_ok = every member ok`.
    /// Test: `tests::aggregate_all_ok`.
    fn build(members: Vec<MemberConfig>) -> Self {
        let all_ok = members.iter().all(|m| m.ok);
        Self {
            command: "config",
            members,
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
/// What: resolves members (empty = all; unknown → exit 3), fetches each member's
/// `config --json`, builds + renders the aggregate, returns the exit code.
/// Test: side-effecting (subprocess); the aggregation is tested via `ConfigAggregate`.
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

    let records: Vec<MemberConfig> = selected
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

    let aggregate = ConfigAggregate::build(records);
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
        let a = ConfigAggregate::build(vec![ok_member("trusty-search")]);
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
        let a = ConfigAggregate::build(vec![
            ok_member("a"),
            MemberConfig {
                member: "b".to_owned(),
                ok: false,
                config: None,
                error: Some("not installed".to_owned()),
            },
        ]);
        assert!(!a.all_ok);
        assert_eq!(a.exit_code(), 2);

        let all = ConfigAggregate::build(vec![ok_member("a")]);
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
}
