//! Unit tests for `tools::recall_rulings_floor` (#9143 AC2): the relevance
//! condition and the reserved slots it grants inside `top_k`.

use super::*;
use trusty_common::memory_core::Drawer;

fn hit(content: &str, score: f32, layer: u8) -> RecallResult {
    RecallResult {
        drawer: Drawer::new(Uuid::new_v4(), content),
        score,
        layer,
    }
}

/// `count` project hits scored from 0.9 down, then one ruling at 0.1.
fn buried(count: usize) -> (Vec<RecallResult>, Uuid) {
    let mut results: Vec<_> = (0..count)
        .map(|i| hit(&format!("project {i}"), 0.9 - i as f32 * 0.01, 2))
        .collect();
    let ruling = hit("ruling", 0.1, 1);
    let id = ruling.drawer.id;
    results.push(ruling);
    (results, id)
}

fn rank(results: &[RecallResult], id: Uuid) -> Option<usize> {
    results.iter().position(|r| r.drawer.id == id)
}

/// Why: the floor overrides score order, so the condition that arms it must
/// separate a ruling about the question from one that is not.
/// What: a table of (query, ruling content, answers?). Plurals fold, case and
/// punctuation are ignored, function words never count, a one-term query
/// needs its one term, and a longer query needs half its terms and two.
#[test]
fn answering_needs_half_the_query_terms_and_at_least_two() {
    let q15 = "Q15 standing rule: issue titles name the symptom, not the presumed cause";
    let ruling =
        "Standing rule Q15 (owner): name the symptom in issue titles, never the presumed cause";
    let cases = [
        (q15, ruling, true),
        ("issue titles", "An issue title names the symptom.", true),
        ("what is the rule about issue titles?", ruling, true),
        ("titles", "Issue titles name the symptom", true),
        // A ruling on another subject shares no term.
        (
            q15,
            "Commits never land on local main; the PM opens a PR",
            false,
        ),
        ("how do basalt columns form when lava cools", ruling, false),
        // Function words alone share nothing.
        ("what should they do", "they should do what it must", false),
        // #9279: a two-letter code is a term, not a function word.
        (
            "what should the PM do",
            "the PM should do what it must",
            true,
        ),
        ("", ruling, false),
        ("?? !!", ruling, false),
        // Two terms, one shared: half, but not two.
        ("issue backlog", "issue titles name the symptom", false),
    ];
    for (query, content, expected) in cases {
        assert_eq!(
            ruling_answers_query(query, content),
            expected,
            "query {query:?} against {content:?}"
        );
    }
}

/// Why (#9279): with `e1` dropped, "ruling e1" was a one-term query, so every
/// ruling that says "ruling" answered it and took a floor slot.
/// What: the id term separates the E1 ruling from another ruling.
#[test]
fn a_short_id_term_separates_two_rulings() {
    let query = "ruling e1";
    assert!(ruling_answers_query(
        query,
        "Ruling E1: hold new builder dispatches"
    ));
    assert!(!ruling_answers_query(
        query,
        "Ruling F0: the PM files no fix round"
    ));
}

/// Why (#9143 AC2): the live check ranked an answering ruling 30th under
/// many strong project hits. The floor puts it at rank 3 inside `top_k`.
#[test]
fn a_buried_answering_ruling_is_lifted_to_rank_3() {
    let (mut results, id) = buried(30);
    let before: Vec<Uuid> = results.iter().map(|r| r.drawer.id).collect();
    apply_rulings_floor(&mut results, &[id], 10);
    assert_eq!(rank(&results, id), Some(RULING_RANK_FLOOR));
    assert_eq!(results.len(), 31, "nothing added or dropped");
    let others: Vec<Uuid> = results
        .iter()
        .map(|r| r.drawer.id)
        .filter(|x| *x != id)
        .collect();
    let others_before: Vec<Uuid> = before.into_iter().filter(|x| *x != id).collect();
    assert_eq!(others, others_before, "every other hit keeps its order");
}

/// Why: the floor is a floor, never a demotion; and an id that is not
/// floored, or absent, moves nothing.
#[test]
fn a_ruling_already_above_the_floor_stays() {
    let mut results = vec![hit("ruling", 0.95, 1), hit("project", 0.9, 2)];
    let id = results[0].drawer.id;
    apply_rulings_floor(&mut results, &[id], 10);
    assert_eq!(rank(&results, id), Some(0));

    let (mut results, id) = buried(12);
    apply_rulings_floor(&mut results, &[Uuid::new_v4()], 10);
    assert_eq!(
        rank(&results, id),
        Some(12),
        "an unfloored ruling keeps its score rank"
    );
    apply_rulings_floor(&mut results, &[], 10);
    apply_rulings_floor(&mut results, &[id], 0);
    assert_eq!(rank(&results, id), Some(12), "no ids or top_k 0: no change");
}

/// Why: with several answering rulings each needs its own slot, and with a
/// small `top_k` the slots must still fall inside the cut.
/// What: two buried rulings take ranks 3 and 4 at `top_k` 10, and at `top_k`
/// 4 (cap 2) the last two slots, ranks 3 and 4. One ruling at `top_k` 1 takes
/// the only slot, at `top_k` 2 the second, at `top_k` 3 the third.
#[test]
fn several_answering_rulings_take_consecutive_slots_inside_a_small_top_k() {
    let (mut results, first) = buried(20);
    let second_hit = hit("second ruling", 0.05, 1);
    let second = second_hit.drawer.id;
    results.push(second_hit);
    let original = results.clone();

    apply_rulings_floor(&mut results, &[first, second], 10);
    assert_eq!(
        (rank(&results, first), rank(&results, second)),
        (Some(2), Some(3))
    );

    let mut small = original.clone();
    apply_rulings_floor(&mut small, &[second, first], 4);
    assert_eq!(
        (rank(&small, first), rank(&small, second)),
        (Some(2), Some(3))
    );

    for (top_k, expected) in [(1, 0), (2, 1), (3, 2)] {
        let (mut results, id) = buried(20);
        apply_rulings_floor(&mut results, &[id], top_k);
        assert_eq!(rank(&results, id), Some(expected), "top_k {top_k}");
    }
}
