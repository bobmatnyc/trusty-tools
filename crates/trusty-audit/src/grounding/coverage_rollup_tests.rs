//! Tests for the run index's investigation-coverage roll-up (#6784).
//!
//! Why: the figure the bundle index states must come from the reports the
//! bundle CARRIES, and it must survive a renderer that writes a shape this crate
//! was not built against — tga and trusty-review meet at a file, not at a Cargo
//! edge (DOC-67 §5). So every case is a literal JSON fixture rather than a
//! serialised `ReportModel`.
//! Test: this file.

use super::coverage_rollup::{
    ANALYZE_LANE_DEAD_HEADLINE, RepoCoverage, Rollup, index_section, read_coverage, rollup,
};

/// A report JSON stating one repository's coverage, with the gap list given.
fn report_json(name: &str, examined: usize, total: usize, gaps: &[&str]) -> String {
    let gaps = gaps
        .iter()
        .map(|g| format!("{:?}", g))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{ "gaps": [{gaps}],
              "investigation": {{ "repos": [
                {{ "name": {name:?},
                   "coverage": {{ "files_examined": {examined}, "total_files": {total} }} }}
              ] }} }}"#
    )
}

/// #6784 regression. The per-repository figure existed — `trusty-review` writes
/// `files_examined`/`total_files` into every report's JSON twin — and the bundle
/// index never stated it, so "how much of this estate was actually read" was
/// answerable only by opening all 59 reports.
///
/// Against `origin/main` at 11b1ba9f9 this does not compile:
/// `grounding::coverage_rollup` does not exist.
#[test]
fn the_rollup_reads_every_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("00-big");
    let b = dir.path().join("01-small");
    std::fs::create_dir_all(&a).expect("mkdir");
    std::fs::create_dir_all(&b).expect("mkdir");
    std::fs::write(a.join("big.json"), report_json("Big", 40, 3_000, &[])).expect("write");
    std::fs::write(
        b.join("small.json"),
        report_json("Small", 30, 60, &[ANALYZE_LANE_DEAD_HEADLINE]),
    )
    .expect("write");

    let rolled = rollup(&[a, b]);

    assert_eq!(rolled.repos.len(), 2);
    assert_eq!(rolled.examined(), 70);
    assert_eq!(rolled.eligible(), 3_060);
    assert_eq!(
        rolled.repos[0].name, "Big",
        "least-covered first: {:?}",
        rolled.repos
    );
    assert!(
        (rolled.repos[0].share() - 1.3).abs() < 0.05,
        "{:?}",
        rolled.repos[0]
    );
    assert_eq!(
        rolled.analyze_lanes_dead(),
        1,
        "the dead-lane fact travels with the coverage row: {:?}",
        rolled.repos
    );
}

/// #6784. A `.json` beside the report that is not a report — the manifest twin,
/// an `osv.json` — must contribute nothing rather than a row of zeroes, and a
/// directory with nothing readable in it is not an error here: a repository
/// whose render failed already states itself in the index's failure column.
#[test]
fn a_json_that_is_not_a_report_contributes_nothing() {
    assert!(read_coverage("not json at all").is_empty());
    assert!(read_coverage(r#"{"queried": 12, "matched": 0}"#).is_empty());
    assert!(
        read_coverage(r#"{"investigation": {"repos": [{"name": "A"}]}}"#).is_empty(),
        "a repository entry with no coverage object states no figure"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    assert!(rollup(&[dir.path().to_path_buf()]).is_empty());
    assert!(rollup(&[dir.path().join("absent")]).is_empty());
}

/// #6784. The index is the file a recipient opens first, so the estate share and
/// the least-covered rows have to be legible there without opening a report.
#[test]
fn the_index_section_states_the_estate_share() {
    let rolled = Rollup {
        repos: vec![
            RepoCoverage {
                name: "Big".to_owned(),
                examined: 40,
                eligible: 3_000,
                analyze_lane_dead: true,
            },
            RepoCoverage {
                name: "Small".to_owned(),
                examined: 30,
                eligible: 60,
                analyze_lane_dead: false,
            },
        ],
    };

    let text = index_section(Some(&rolled));

    assert!(text.contains("70 of 3060 tracked file(s)"), "{text}");
    assert!(text.contains("2.3% of the estate"), "{text}");
    assert!(
        text.contains("| Big | 40 | 3000 | 1.3% | **did not run** |"),
        "the worst-covered row names its numbers:\n{text}"
    );
    assert!(
        text.contains("1 of those repositor(y/ies) also ran NO static-analysis pass"),
        "the #6811 fact is stated beside the coverage:\n{text}"
    );
}

/// #6784. An empty roll-up is a STATEMENT — a bundle whose renderer records no
/// coverage — and used to be indistinguishable from full coverage. A producer
/// with nothing to read at all states neither.
#[test]
fn a_bundle_with_no_coverage_records_says_so() {
    let text = index_section(Some(&Rollup::default()));
    assert!(text.contains("Not recorded"), "{text}");
    assert!(text.contains("#6784"), "{text}");

    assert_eq!(
        index_section(None),
        "",
        "a producer with no reports to read must claim nothing either way"
    );
}

/// #6784 regression, cross-crate. Both of `trusty-review`'s total-collapse paths
/// lead their gap line with
/// [`trusty_common::review_gap_contract::ANALYZE_LANE_DEAD_HEADLINE`] and append
/// their own detail; the reader must recognise either. The client-build path
/// used to lead with "trusty-analyze data unavailable" and rolled up here as a
/// lane that RAN.
///
/// The producing half is `trusty-review`'s
/// `run_tests::a_dead_analyze_client_reads_as_a_dead_lane_downstream`, which
/// runs the real code path; the two halves are bound by the shared constant
/// because the crates deliberately do not depend on each other (DOC-67 §5).
#[test]
fn a_gap_leading_with_the_shared_headline_is_a_dead_lane() {
    let client_build = format!(
        "{ANALYZE_LANE_DEAD_HEADLINE} — the analysis client could not be built, so no \
         application in this report was assessed against trusty-analyze."
    );
    let walk = format!("{ANALYZE_LANE_DEAD_HEADLINE} — 0 of 59 application(s) assessed.");

    for gap in [&client_build, &walk] {
        let rows = read_coverage(&report_json("A", 10, 100, &[gap]));
        assert_eq!(rows.len(), 1, "{gap}");
        assert!(rows[0].analyze_lane_dead, "must roll up as dead: {gap}");
    }

    let partial = "trusty-analyze lane partially degraded — 58 of 59 application(s) assessed.";
    let rows = read_coverage(&report_json("A", 10, 100, &[partial]));
    assert!(
        !rows[0].analyze_lane_dead,
        "a partly degraded lane DID run: {partial}"
    );
}
