//! The guard policy table: every `tm hook --pm-guard` and `--egress-guard`
//! refusal names one entry (#10641, owner ruling D29).
//!
//! Why: the owner ruled that the Architect is the owner's proxy — a call is
//! refused only by a rule that names a written policy. Before this table the
//! rules were code with free-text audit labels, so nothing tied a refusal to
//! the policy it enforced. PR1 of #10641 adds the table and changes no verdict.
//! What: [`GuardPolicy`] is one entry: a stable id, the audit label the rule
//! already records, a one-sentence summary, the written source it enforces,
//! the roles it binds, whether it holds under the operator bypasses, the
//! output-masking flag (always `false` until #10641 PR3), and, for an entry
//! that binds the Architect, the Architect core hard limit it implements.
//! Entries are built only by the private `policy` constructor, which refuses
//! an empty `source` at compile time, so an unwritten rule cannot enter the
//! table. [`find`] looks an entry up by its audit label; [`for_rule`] never
//! fails — a miss answers [`POLICY_LOOKUP_MISS`], itself a refusal, so a
//! lookup miss can only deny. [`POLICIES`] lists every entry once, in id order.
//! Test: `guard_policy_tests.rs`.

#[path = "guard_policy_egress.rs"]
mod egress;
#[path = "guard_policy_entries.rs"]
mod entries;

pub use egress::*;
pub use entries::*;

/// Who a guard rule binds.
///
/// Test: `every_entry_binds_at_least_one_role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
    /// A PM session's own thread.
    Pm,
    /// The process-bound Architect's main thread.
    Architect,
    /// A dispatched agent or subagent, the Architect's included.
    Agent,
}

impl Role {
    /// The role as the policy document prints it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pm => "PM",
            Self::Architect => "Architect",
            Self::Agent => "Agent",
        }
    }
}

/// One guard policy entry (#10641).
///
/// Why: see the module doc — the unit a refusal names.
/// What: plain data. `#[non_exhaustive]` keeps every other crate, the `tm`
/// binary included, from writing an entry as a struct literal: an entry
/// exists only in this module's table.
/// Test: `every_entry_has_a_source_and_a_unique_id_and_rule`.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct GuardPolicy {
    /// Stable id, `GP-NNN`; never reused.
    pub id: &'static str,
    /// The audit label the rule records (`check=` in the denial store).
    pub rule: &'static str,
    /// One sentence naming what the rule refuses.
    pub summary: &'static str,
    /// The written policy the rule enforces: an issue, ADR or ruling.
    pub source: &'static str,
    /// The roles the rule binds today.
    pub applies: &'static [Role],
    /// `true` when the rule holds under `TRUSTY_MPM_PM_UNRESTRICTED` and
    /// `TRUSTY_MPM_DISABLE_HOOKS` for every role it binds.
    pub absolute: bool,
    /// Architect only: allow with output masking (#10641 PR3). `false` today.
    pub mask: bool,
    /// For an entry that binds the Architect: the name of the Architect core
    /// hard limit it implements (`architect_core::ARCHITECT_CORE`), per D29
    /// item 5. `None` for an entry that does not bind the Architect.
    pub architect_limit: Option<&'static str>,
}

impl GuardPolicy {
    /// The tag a refusal carries: `[policy GP-005 destructive-delete; source: #4031]`.
    ///
    /// Test: `the_tag_names_id_rule_and_source`.
    pub fn tag(&self) -> String {
        format!(
            "[policy {} {}; source: {}]",
            self.id, self.rule, self.source
        )
    }

    /// Whether the entry binds `role`.
    pub fn binds(&self, role: Role) -> bool {
        self.applies.contains(&role)
    }
}

/// Build one entry; a missing id, rule, summary, source or role is a
/// compile error, because every entry is a `static` evaluated at build time.
///
/// Why (#10641): "a rule without a `source` does not compile into the table".
/// Test: `every_entry_has_a_source_and_a_unique_id_and_rule` (the runtime
/// mirror of the compile-time asserts).
const fn policy(
    id: &'static str,
    rule: &'static str,
    summary: &'static str,
    source: &'static str,
    applies: &'static [Role],
    absolute: bool,
    architect_limit: Option<&'static str>,
) -> GuardPolicy {
    assert!(!id.is_empty(), "a guard policy needs an id");
    assert!(!rule.is_empty(), "a guard policy needs an audit rule label");
    assert!(!summary.is_empty(), "a guard policy needs a summary");
    assert!(!source.is_empty(), "a guard policy needs a written source");
    assert!(
        !applies.is_empty(),
        "a guard policy binds at least one role"
    );
    GuardPolicy {
        id,
        rule,
        summary,
        source,
        applies,
        absolute,
        mask: false,
        architect_limit,
    }
}

/// The entry whose audit label is `rule`, or `None`.
///
/// Test: `find_answers_every_listed_rule_and_nothing_else`.
pub fn find(rule: &str) -> Option<&'static GuardPolicy> {
    POLICIES.iter().copied().find(|p| p.rule == rule)
}

/// The entry for `rule`; a miss answers [`POLICY_LOOKUP_MISS`].
///
/// Why (#10641): a call site holding a dynamic audit label still prints a
/// refusal on a miss — it can never turn the miss into an allow, because
/// this function has no "absent" answer to branch on.
/// What: [`find`], falling back to [`POLICY_LOOKUP_MISS`].
/// Test: `a_policy_lookup_miss_denies`.
pub fn for_rule(rule: &str) -> &'static GuardPolicy {
    find(rule).unwrap_or(&POLICY_LOOKUP_MISS)
}

/// The policy table as Markdown, the body of `docs/reference/guard-policies.md`.
///
/// Test: `policy_doc_matches_table`.
pub fn render_markdown() -> String {
    let mut out = String::from(
        "# Guard policies\n\n\
         Generated from `crates/trusty-mpm/src/core/guard_policy_entries.rs` by the \
         `policy_doc_matches_table` test (#10641). Do not edit by hand; run \
         `TM_UPDATE_GUARD_POLICY_DOC=1 cargo test -p trusty-mpm --lib policy_doc_matches_table`.\n\n\
         Every `tm hook --pm-guard` and `tm hook --egress-guard` refusal ends with \
         `[policy <id> <rule>; source: <source>]`.\n\n\
         | Id | Rule | Binds | Absolute | Architect core limit | Source | Summary |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for p in POLICIES {
        let roles: Vec<&str> = p.applies.iter().map(|r| r.label()).collect();
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} | {} |\n",
            p.id,
            p.rule,
            roles.join(", "),
            if p.absolute { "yes" } else { "no" },
            p.architect_limit.unwrap_or("-"),
            p.source,
            p.summary,
        ));
    }
    out
}

#[cfg(test)]
#[path = "guard_policy_tests.rs"]
mod tests;
