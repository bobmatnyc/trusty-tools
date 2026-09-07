//! Who wrote this agent file? The `provenance:` frontmatter field (#4698).
//!
//! Why: an agent file on disk carried no marker saying which of three writers
//! produced it — the framework's own deploy, `tm-agent-manager` composing one
//! on request, or a human hand-writing one through Claude Code's `/agents`.
//! The never-clobber-what-we-did-not-write rule needs that distinction, and
//! [`crate::agents::manifest::Origin::is_framework_owned`] cannot supply it:
//! it answers only from the ledger, so a file with no ledger entry — every
//! hand-written agent, and every stale leftover — reads identically. Issue
//! #4698 records the owner's ruling of 2026-08-03 verbatim: "We use
//! frontmatter to track this."
//!
//! What: [`Provenance`], the three declared values, plus the parse
//! ([`Provenance::from_str`], rejecting anything else with the typed
//! [`UnknownProvenance`]) and the absence rule. Issue #4698's design note
//! fixes that rule: "ABSENCE of the field is itself the signal for 'not ours,
//! never touch.'" — so an absent field resolves to
//! [`Provenance::UserAuthored`], never to a framework-owned default.
//! [`reconcile_with_ledger`] settles the one case where a file's declaration
//! and the ownership ledger can disagree.
//!
//! This field is ADDITIVE. It records who wrote a file; it is not a tier, and
//! nothing here participates in tier resolution — the owner's 2026-08-01
//! ruling collapsed the agent tiers to a single deploy target fed by
//! prioritised sources, and no code in this module may be extended into a
//! second tier mechanism.
//!
//! Test: `provenance_round_trips_every_value`,
//! `provenance_rejects_an_unknown_value`, `absent_provenance_is_user_authored`,
//! `only_framework_owned_is_framework_owned`,
//! `reconcile_agreeing_declaration_is_silent`,
//! `reconcile_disagreement_lets_the_ledger_win_and_names_the_file`,
//! `reconcile_without_a_declaration_is_silent`.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// The frontmatter key this module owns.
pub const PROVENANCE_KEY: &str = "provenance";

/// Who wrote an agent file.
///
/// Why: the three writers of a `.claude/agents/*.md` file need to stay
/// distinguishable on disk, independently of any ledger — see the module doc.
/// What: a closed enum whose wire spellings are exactly the three the issue
/// title names: `framework-owned`, `tm-agent-manager-built`, `user-authored`.
/// The spelling is the contract — it appears in deployed files an operator
/// reads, so it is written out by hand in [`Provenance::as_str`] rather than
/// derived from the variant name.
/// Test: `provenance_round_trips_every_value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Written by the framework's own agent deploy — the `Overwrite` tier.
    FrameworkOwned,
    /// Composed on request by `tm-agent-manager`.
    TmAgentManagerBuilt,
    /// Hand-authored by the operator (Claude Code's `/agents`, or any editor).
    UserAuthored,
}

impl Provenance {
    /// Every accepted value, in declaration order.
    ///
    /// Why: [`UnknownProvenance`]'s message lists what WAS accepted, and the
    /// round-trip test iterates the same list, so neither can drift from the
    /// match arms below.
    pub const ALL: [Provenance; 3] = [
        Provenance::FrameworkOwned,
        Provenance::TmAgentManagerBuilt,
        Provenance::UserAuthored,
    ];

    /// The wire spelling written into and read from frontmatter.
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::FrameworkOwned => "framework-owned",
            Provenance::TmAgentManagerBuilt => "tm-agent-manager-built",
            Provenance::UserAuthored => "user-authored",
        }
    }

    /// Whether this declaration marks a FRAMEWORK-owned file.
    ///
    /// Why: the frontmatter counterpart of
    /// [`crate::agents::manifest::Origin::is_framework_owned`], and it must
    /// agree with it wherever both exist — [`reconcile_with_ledger`] is what
    /// checks that.
    /// What: `true` only for [`Provenance::FrameworkOwned`].
    /// [`Provenance::TmAgentManagerBuilt`] is deliberately NOT framework-owned:
    /// the operator asked for that file, so it keeps the preserve-on-mismatch
    /// treatment [`crate::agents::manifest::Origin::Registry`] gets for the
    /// same reason.
    /// Test: `only_framework_owned_is_framework_owned`.
    pub fn is_framework_owned(self) -> bool {
        matches!(self, Provenance::FrameworkOwned)
    }
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `provenance:` value that is not one of the three accepted spellings.
///
/// Why: #4698 requires an unknown value be REJECTED, not silently coerced to a
/// default — coercing would let a typo (`provenance: framework_owned`) read as
/// user-authored and freeze a framework file forever, which is #4408's failure
/// shape reached through a new door. A typed error keeps that decision at the
/// call site instead of inside a `String`.
/// What: carries the offending value verbatim and names what was accepted.
/// Test: `provenance_rejects_an_unknown_value`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "unknown `provenance:` value `{value}` — expected one of \
     framework-owned, tm-agent-manager-built, user-authored"
)]
pub struct UnknownProvenance {
    /// The value as it appeared in the frontmatter, trimmed.
    pub value: String,
}

impl FromStr for Provenance {
    type Err = UnknownProvenance;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        Provenance::ALL
            .into_iter()
            .find(|p| p.as_str() == trimmed)
            .ok_or_else(|| UnknownProvenance {
                value: trimmed.to_string(),
            })
    }
}

/// Resolve a declaration that may be absent.
///
/// Why: #4698's design note, verbatim — "ABSENCE of the field is itself the
/// signal for 'not ours, never touch.'" That makes the default the SAFE one,
/// and stating it in one function keeps a caller from choosing a different
/// default and quietly reclassifying every pre-#4698 file as the framework's.
/// What: the declaration when present, else [`Provenance::UserAuthored`].
/// Test: `absent_provenance_is_user_authored`.
pub fn declared_or_default(declared: Option<Provenance>) -> Provenance {
    declared.unwrap_or(Provenance::UserAuthored)
}

/// The same document with its `provenance:` frontmatter line removed.
///
/// Why: the ADOPTION path (#2504) registers an untracked target file when its
/// bytes already equal what the deployer would write. Every file deployed
/// BEFORE #4698 lacks the stamped line, so a naive byte comparison stopped
/// matching the moment the deployer began stamping — and a file that fails
/// adoption is skipped, warned about, and never refreshed again. That is
/// #4408's freeze reached through a new door, so adoption compares against
/// both spellings.
/// What: drops the first line whose key is `provenance` from the leading
/// frontmatter block, leaving everything else — the body included — byte-exact.
/// A document with no such line is returned unchanged. Only the frontmatter
/// block is scanned, so a `provenance:` line in prose survives.
/// Test: `without_provenance_line_drops_only_that_line`,
/// `without_provenance_line_is_a_no_op_when_absent`,
/// `without_provenance_line_ignores_a_body_line`.
pub fn without_provenance_line(doc: &str) -> String {
    let mut out = String::with_capacity(doc.len());
    let mut in_frontmatter = false;
    let mut dropped = false;
    for (idx, line) in doc.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if idx == 0 && trimmed == "---" {
            in_frontmatter = true;
            out.push_str(line);
            continue;
        }
        if in_frontmatter && trimmed == "---" {
            in_frontmatter = false;
            out.push_str(line);
            continue;
        }
        let is_provenance = in_frontmatter
            && !dropped
            && trimmed
                .split_once(':')
                .is_some_and(|(key, _)| key.trim() == PROVENANCE_KEY);
        if is_provenance {
            dropped = true;
            continue;
        }
        out.push_str(line);
    }
    out
}

/// The outcome of checking a file's declaration against the ownership ledger.
///
/// Why: the disagreement must be both LOGGED and testable. Returning the
/// message as data — as well as emitting it through `tracing` — lets a test
/// assert that the file is named without standing up a subscriber.
/// What: the ledger's verdict (always — the ledger wins), plus the
/// disagreement message when there was one.
/// Test: `reconcile_disagreement_lets_the_ledger_win_and_names_the_file`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciled {
    /// Whether the file is framework-owned. Always the LEDGER's answer.
    pub framework_owned: bool,
    /// `Some` only when a declaration existed and contradicted the ledger.
    pub disagreement: Option<String>,
}

/// Reconcile a file's declared `provenance:` against its ledger entry.
///
/// Why: #4698 adds a second, independent record of the same fact, and two
/// records can disagree — a hand-edited deployed file is exactly how. The
/// ledger wins, because it is the record the deployer itself wrote under a
/// lock and checksummed; the frontmatter is the copy anyone with an editor can
/// change. Silently preferring the file would hand an operator (or a corrupted
/// write) a way to flip a bundled agent to user-owned and freeze it.
/// What: returns the ledger's `framework_owned` unchanged in every case. When
/// a declaration exists and contradicts it, also emits a `tracing::warn!`
/// naming `file_name`, both spellings, and returns that message in
/// [`Reconciled::disagreement`]. A file with NO declaration is silent — that
/// is every file written before this field existed, which is not an anomaly.
///
/// This never runs where there is no ledger entry: an absent entry is not a
/// disagreement, and letting a declaration stand in for one would manufacture
/// a user-owned exemption out of a field anybody can type (see
/// [`crate::agents::tier_audit`]'s invariant 2).
/// Test: `reconcile_agreeing_declaration_is_silent`,
/// `reconcile_disagreement_lets_the_ledger_win_and_names_the_file`,
/// `reconcile_without_a_declaration_is_silent`.
pub fn reconcile_with_ledger(
    file_name: &str,
    ledger_framework_owned: bool,
    declared: Option<Provenance>,
) -> Reconciled {
    let Some(declared) = declared else {
        return Reconciled {
            framework_owned: ledger_framework_owned,
            disagreement: None,
        };
    };
    if declared.is_framework_owned() == ledger_framework_owned {
        return Reconciled {
            framework_owned: ledger_framework_owned,
            disagreement: None,
        };
    }
    let ledger_spelling = if ledger_framework_owned {
        Provenance::FrameworkOwned.as_str()
    } else {
        "not framework-owned"
    };
    let message = format!(
        "'{file_name}' declares `provenance: {}` but the deployed-agent manifest records it as \
         {ledger_spelling}; the manifest wins (#4698)",
        declared.as_str()
    );
    tracing::warn!(
        file = %file_name,
        declared = %declared.as_str(),
        ledger_framework_owned,
        "agent `provenance:` disagrees with the deployed-agent manifest — the manifest wins"
    );
    Reconciled {
        framework_owned: ledger_framework_owned,
        disagreement: Some(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provenance_round_trips_every_value() {
        for p in Provenance::ALL {
            assert_eq!(p.as_str().parse::<Provenance>(), Ok(p), "{p} round-trips");
        }
        // The exact spellings #4698's title names, pinned literally so a
        // variant rename cannot silently change the on-disk contract.
        assert_eq!(Provenance::FrameworkOwned.as_str(), "framework-owned");
        assert_eq!(
            Provenance::TmAgentManagerBuilt.as_str(),
            "tm-agent-manager-built"
        );
        assert_eq!(Provenance::UserAuthored.as_str(), "user-authored");
    }

    #[test]
    fn provenance_parse_trims_surrounding_whitespace() {
        assert_eq!(
            "  framework-owned ".parse::<Provenance>(),
            Ok(Provenance::FrameworkOwned)
        );
    }

    #[test]
    fn provenance_rejects_an_unknown_value() {
        // An underscore typo of the real spelling: the exact mistake that must
        // NOT coerce to a default.
        let err = "framework_owned".parse::<Provenance>().unwrap_err();
        assert_eq!(err.value, "framework_owned");
        assert!(
            err.to_string().contains("framework-owned"),
            "the error lists the accepted spellings: {err}"
        );
        assert!("".parse::<Provenance>().is_err(), "empty is not a value");
        assert!(
            "Framework-Owned".parse::<Provenance>().is_err(),
            "the spelling is case-sensitive"
        );
    }

    #[test]
    fn absent_provenance_is_user_authored() {
        // #4698: "ABSENCE of the field is itself the signal for 'not ours,
        // never touch.'"
        assert_eq!(declared_or_default(None), Provenance::UserAuthored);
        assert!(!declared_or_default(None).is_framework_owned());
        assert_eq!(
            declared_or_default(Some(Provenance::FrameworkOwned)),
            Provenance::FrameworkOwned
        );
    }

    #[test]
    fn only_framework_owned_is_framework_owned() {
        assert!(Provenance::FrameworkOwned.is_framework_owned());
        assert!(!Provenance::TmAgentManagerBuilt.is_framework_owned());
        assert!(!Provenance::UserAuthored.is_framework_owned());
    }

    #[test]
    fn without_provenance_line_drops_only_that_line() {
        let doc = "---\nname: qa\nprovenance: framework-owned\nrole: qa\n---\n\nBody.\n";
        assert_eq!(
            without_provenance_line(doc),
            "---\nname: qa\nrole: qa\n---\n\nBody.\n"
        );
    }

    #[test]
    fn without_provenance_line_is_a_no_op_when_absent() {
        let doc = "---\nname: qa\nrole: qa\n---\n\nBody.\n";
        assert_eq!(without_provenance_line(doc), doc);
        // No frontmatter at all is equally untouched.
        assert_eq!(without_provenance_line("just prose\n"), "just prose\n");
    }

    #[test]
    fn without_provenance_line_ignores_a_body_line() {
        // Prose that happens to mention the key must survive byte-exact —
        // agent bodies document this field.
        let doc = "---\nname: qa\n---\n\nSet provenance: framework-owned in frontmatter.\n";
        assert_eq!(without_provenance_line(doc), doc);
    }

    #[test]
    fn reconcile_agreeing_declaration_is_silent() {
        let r = reconcile_with_ledger("qa.md", true, Some(Provenance::FrameworkOwned));
        assert!(r.framework_owned);
        assert_eq!(r.disagreement, None);

        let r = reconcile_with_ledger("mine.md", false, Some(Provenance::UserAuthored));
        assert!(!r.framework_owned);
        assert_eq!(r.disagreement, None);

        // tm-agent-manager-built agrees with a non-framework-owned ledger row.
        let r = reconcile_with_ledger("built.md", false, Some(Provenance::TmAgentManagerBuilt));
        assert_eq!(r.disagreement, None);
    }

    #[test]
    fn reconcile_disagreement_lets_the_ledger_win_and_names_the_file() {
        // A hand-edited deployed file claiming to be the operator's while the
        // ledger still records the framework as its author.
        let r = reconcile_with_ledger("rust-engineer.md", true, Some(Provenance::UserAuthored));
        assert!(r.framework_owned, "the manifest wins");
        let msg = r.disagreement.expect("a disagreement is reported");
        assert!(msg.contains("rust-engineer.md"), "names the file: {msg}");
        assert!(
            msg.contains("user-authored"),
            "names the declaration: {msg}"
        );

        // And the mirror: a declaration claiming the framework wrote a file the
        // ledger records as the operator's.
        let r = reconcile_with_ledger("mine.md", false, Some(Provenance::FrameworkOwned));
        assert!(!r.framework_owned, "the manifest wins in both directions");
        assert!(r.disagreement.is_some_and(|m| m.contains("mine.md")));
    }

    #[test]
    fn reconcile_without_a_declaration_is_silent() {
        // Every file written before #4698 lands here; it is not an anomaly.
        for owned in [true, false] {
            let r = reconcile_with_ledger("legacy.md", owned, None);
            assert_eq!(r.framework_owned, owned);
            assert_eq!(r.disagreement, None);
        }
    }
}
