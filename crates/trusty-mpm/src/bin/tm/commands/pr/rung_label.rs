//! The `rung:N` test-ladder label — one spelling for `tm pr open` and
//! `tm pr queue-check` (Dr4).
//!
//! Why: owner ruling Dr4 (2026-11-16) — the rung a PR claims has to be
//! recorded on the PR, so the merge queue can demand a pinned code-critic
//! verdict for the high-risk rungs. `tm pr open --rung N` writes the label;
//! `tm pr queue-check` reads it back. Both sides spell it here.
//!
//! What: [`name`] and [`seed_argv`] for the writer, [`recorded`] for the reader.
//!
//! Test: `queue_check_critic_tests.rs`.

use trusty_mpm::core::policy_labels::{self, PolicyLabel};

/// The label prefix, matched case-insensitively by the reader.
const PREFIX: &str = "rung:";

/// The highest rung on the test ladder (root `CLAUDE.md`).
pub(crate) const MAX_RUNG: u8 = 6;

/// The rungs that need a code-critic verdict pinned to the head (Dr4).
pub(crate) const CRITIC_RUNGS: [u8; 2] = [5, 6];

/// The label colour; one colour for the whole `rung:1` … `rung:6` set.
const COLOR: &str = "5319E7";

/// The label name for rung `n`.
pub(crate) fn name(n: u8) -> String {
    format!("{PREFIX}{n}")
}

/// The `gh label create rung:N --force` argv that makes the label exist.
///
/// Why: `gh pr create --label rung:N` fails outright on a label the
/// repository has never seen. `rung:` is the framework's own namespace, so
/// `--force` is safe and makes the seed idempotent.
/// Test: `dr4_open_rung_5_applies_the_rung_5_label`.
pub(crate) fn seed_argv(n: u8, repo: Option<&str>) -> Vec<String> {
    let label = PolicyLabel::new(name(n), COLOR, format!("Test-ladder rung {n} (CLAUDE.md)"));
    policy_labels::create_label_argv(&label, repo, true)
}

/// The rung a PR's labels record.
///
/// Why: a missing, doubled or malformed rung label must refuse the merge
/// rather than read as "no verdict needed" — the gate fails closed.
/// What: `Ok(None)` when no label starts with `rung:`; `Ok(Some(n))` for
/// exactly one well-formed `rung:N` with N in 1..=6; `Err(reason)` for two or
/// more `rung:` labels, or one whose value is not 1..=6.
/// Test: `dr4_no_rung_label_is_refused`, `dr4_two_rung_labels_are_refused`,
/// `dr4_a_malformed_rung_label_is_refused`.
pub(crate) fn recorded<'a>(
    labels: impl IntoIterator<Item = &'a str>,
) -> Result<Option<u8>, String> {
    let found: Vec<&str> = labels
        .into_iter()
        .map(str::trim)
        .filter(|l| {
            l.get(..PREFIX.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(PREFIX))
        })
        .collect();
    match found.as_slice() {
        [] => Ok(None),
        [one] => {
            let value = one[PREFIX.len()..].trim();
            match value.parse::<u8>() {
                Ok(n) if (1..=MAX_RUNG).contains(&n) => Ok(Some(n)),
                _ => Err(format!(
                    "malformed test-ladder label `{one}`; expected rung:1 … rung:{MAX_RUNG}"
                )),
            }
        }
        many => Err(format!(
            "more than one test-ladder rung recorded ({}); keep exactly one rung:N label",
            many.join(", ")
        )),
    }
}
