//! Invariants of the shared PM routing rows (#8293).
//!
//! Why: the rows are consumed by two product crates that cannot see each
//! other, so every property that holds ACROSS the consumers — id uniqueness,
//! per-consumer cell ordering, the shared research/engineer/verification spine
//! — has to be proven here. Each consumer separately proves its own delivered
//! prompt matches what these rows render.
//! What: unit tests over [`super::ROUTING_ROWS`] and the render/fill helpers.
//! Test: this module.

use super::*;

/// Every backtick-quoted span in `text`, in order.
///
/// Why: a cell's agent list must be derivable from the cell's own prose, or the
/// list and the text can disagree silently.
/// What: the odd-indexed spans of a backtick split; panics on an unbalanced
/// count, which would invert inside and outside.
/// Test: `every_route_agent_is_named_by_its_own_cell`.
fn backticked(text: &str) -> Vec<&str> {
    let spans: Vec<&str> = text.split('`').collect();
    assert!(spans.len() % 2 == 1, "unbalanced backticks in: {text:?}");
    spans.iter().skip(1).step_by(2).copied().collect()
}

const CONSUMERS: [Consumer; 2] = [Consumer::Mpm, Consumer::Tcode];

#[test]
fn row_ids_are_unique() {
    let mut seen: Vec<&str> = Vec::new();
    for row in ROUTING_ROWS {
        assert!(
            !seen.contains(&row.id),
            "duplicate routing-row id {:?} — the spine and the drift tests key on it",
            row.id
        );
        seen.push(row.id);
    }
}

#[test]
fn routes_yields_only_the_consumers_rows() {
    let mpm: Vec<&str> = routes(Consumer::Mpm).map(|(id, _)| id).collect();
    let tcode: Vec<&str> = routes(Consumer::Tcode).map(|(id, _)| id).collect();
    assert!(
        !tcode.contains(&"review"),
        "trusty-code routes no review class from the PM card (DOC-75 §4b): {tcode:?}"
    );
    assert!(
        mpm.contains(&"review"),
        "trusty-mpm's four choices include the review one: {mpm:?}"
    );
}

/// Each consumer's cells carry positions `1..=n` exactly once (#8293).
///
/// Why: `render_table` sorts on [`Cell::order`], so a duplicated or skipped
/// position silently reorders — or drops the determinism of — a delivered
/// routing table.
/// Test: this test.
#[test]
fn cell_order_is_unique_and_contiguous_per_consumer() {
    for consumer in CONSUMERS {
        let mut orders: Vec<u8> = routes(consumer)
            .filter_map(|(_, route)| route.cell.as_ref().map(|cell| cell.order))
            .collect();
        orders.sort_unstable();
        let expected: Vec<u8> = (1..=orders.len() as u8).collect();
        assert_eq!(
            orders, expected,
            "{consumer:?}'s cell positions must be 1..=n with no gap or repeat"
        );
    }
}

/// Every agent a route names appears in that route's own rendered cell (#8293).
///
/// Why: the per-consumer roster tests resolve [`agents`], so a name listed there
/// but absent from the prose would demand an agent the prompt never routes to —
/// and a name in the prose but not the list would escape the roster check
/// entirely. Both directions are asserted for rows that render a cell.
/// Test: this test.
#[test]
fn every_route_agent_is_named_by_its_own_cell() {
    for consumer in CONSUMERS {
        for (id, route) in routes(consumer) {
            let Some(cell) = route.cell.as_ref() else {
                continue;
            };
            let named = backticked(cell.text);
            for agent in std::iter::once(&route.agent).chain(route.also.iter()) {
                assert!(
                    named.contains(agent),
                    "{consumer:?} row {id:?} lists agent {agent:?}, which its cell text \
                     never names: {:?}",
                    cell.text
                );
            }
            // The reverse direction, total over the cell's spans: every
            // backticked span is either a declared routing target or a declared
            // non-agent (`ops`, `make`, `mise run`). A span that is neither
            // would reach the delivered prompt with nothing checking it.
            let declared: Vec<&str> = [route.agent]
                .into_iter()
                .chain(route.also.iter().copied())
                .chain(route.non_agents.iter().copied())
                .collect();
            for name in named {
                assert!(
                    declared.contains(&name),
                    "{consumer:?} row {id:?} backticks {name:?}, which is neither a \
                     declared routing target nor a declared non-agent"
                );
            }
        }
    }
}

#[test]
fn agents_are_deduplicated() {
    for consumer in CONSUMERS {
        let names = agents(consumer);
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            names.len(),
            "{consumer:?} yields a duplicated agent name: {names:?}"
        );
    }
}

/// Both pipelines follow [`SIMPLE_TASK_SPINE`] in order (#8293).
///
/// Why: this is the criterion the two products actually share — context, then
/// the source change, then verification. The AGENT NAMES differ (`qa` vs
/// `qa-agent`), so the invariant is stated over row ids and resolved through
/// each consumer's own route.
/// Test: this test.
#[test]
fn every_pipeline_follows_the_shared_spine() {
    for consumer in CONSUMERS {
        let chain = pipeline(consumer);
        let mut cursor = 0usize;
        for id in SIMPLE_TASK_SPINE {
            let route = ROUTING_ROWS
                .iter()
                .find(|row| row.id == *id)
                .and_then(|row| row.route(consumer))
                .unwrap_or_else(|| panic!("{consumer:?} routes no {id:?} class"));
            let at = chain[cursor..]
                .iter()
                .position(|name| *name == route.agent)
                .unwrap_or_else(|| {
                    panic!(
                        "{consumer:?}'s pipeline {chain:?} does not name {:?} \
                         (the {id:?} class) at or after position {cursor}",
                        route.agent
                    )
                });
            cursor += at + 1;
        }
    }
}

/// The verification class resolves to a different agent per product (#8293).
///
/// Why: this is the one row whose agent name the owner ruled must differ —
/// trusty-mpm keeps routing testing to `qa`, and changing that silently would be
/// a routing regression for every trusty-mpm session. trusty-code needs raw test
/// output, which its read-only `qa` fork cannot produce.
/// Test: this test.
#[test]
fn verification_keeps_a_per_product_agent_name() {
    let row = ROUTING_ROWS
        .iter()
        .find(|row| row.id == "verification")
        .expect("the verification class is a shared row");
    assert_eq!(
        row.route(Consumer::Mpm).map(|r| r.agent),
        Some("qa"),
        "trusty-mpm routes testing to `qa` — #8293 does not change that"
    );
    assert_eq!(
        row.route(Consumer::Tcode).map(|r| r.agent),
        Some("qa-agent"),
        "trusty-code routes verification to `qa-agent` (DOC-75 §6 wants real output)"
    );
}

#[test]
fn mpm_table_renders_its_four_choice_rows() {
    let table = render_table(Consumer::Mpm);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines[0], "| Choice | Which agent |");
    assert_eq!(lines[1], "|---|---|");
    assert_eq!(
        lines.len(),
        6,
        "four rows plus a header and a rule: {table}"
    );
    assert!(lines[2].starts_with("| Review BEFORE implementation"));
    assert!(lines[5].starts_with("| Testing |"));
}

#[test]
fn tcode_table_renders_its_seven_rows() {
    let table = render_table(Consumer::Tcode);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines[0], "| The task needs | Delegate to |");
    assert_eq!(lines[1], "|---|---|");
    assert_eq!(
        lines.len(),
        9,
        "seven rows plus a header and a rule: {table}"
    );
    assert!(lines[2].ends_with("| `research` |"));
    assert!(lines[4].ends_with("| `qa-agent` |"));
}

/// trusty-code's rendered rows name nothing trusty-code does not ship (#8293).
///
/// Why: DOC-75 §4b forbids the tcode card from pointing at a skill, a `tm` CLI
/// verb or the tmux-hosted harness. trusty-code's own card test checks the
/// ASSEMBLED prompt; this one fails at the source, where the mistake would
/// actually be made — someone widening a shared cell with trusty-mpm wording.
/// Test: this test.
#[test]
fn tcode_rows_name_no_harness_tcode_lacks() {
    let rendered = format!(
        "{}\n{}",
        render_table(Consumer::Tcode),
        render_pipeline(Consumer::Tcode)
    );
    for absent in ["tmux", "Skill(", "mise run", "tm ", "make "] {
        assert!(
            !rendered.contains(absent),
            "a trusty-code routing cell names {absent:?}, which tcode does not ship"
        );
    }
}

#[test]
fn pipeline_chain_is_punctuated_per_consumer() {
    assert_eq!(
        render_pipeline(Consumer::Mpm),
        "`research` → `engineer` → `local-ops` → `qa` → `documentation`"
    );
    assert_eq!(
        render_pipeline(Consumer::Tcode),
        "`research`, then `engineer`, then `qa-agent`"
    );
}

#[test]
fn fill_replaces_both_placeholders() {
    let filled = fill(
        &format!("before\n\n{TABLE_PLACEHOLDER}\n\nmid {PIPELINE_PLACEHOLDER}.\n"),
        Consumer::Tcode,
    );
    assert!(!filled.contains(TABLE_PLACEHOLDER));
    assert!(!filled.contains(PIPELINE_PLACEHOLDER));
    assert!(filled.contains(&render_table(Consumer::Tcode)));
    assert!(filled.contains("mid `research`, then `engineer`, then `qa-agent`."));
}

#[test]
fn fill_leaves_an_unmarked_template_alone() {
    let template = "no markers here\n";
    assert_eq!(fill(template, Consumer::Mpm), template);
}
