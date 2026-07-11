//! Unit tests for the Mermaid chart renderer (#2366 wave-4).
//!
//! Why: the chart output is deterministic and syntax-sensitive (Mermaid parses
//! `xychart-beta`/`radar-beta` strictly); these tests pin the block header +
//! structure per chart type, the numeric-parse tolerance, label escaping/caps,
//! the heatmap fallback, empty-dataset omission, and end-to-end injection.
//! What: exercises [`inject`] end-to-end plus the internal helpers via the
//! module's private surface (same-crate `mod tests`).
//! Test: this file IS the test.

use super::*;

/// A dataset marker + its populated table, as it appears post-polish.
fn radar_input() -> &'static str {
    "<!-- dataset: health_factors_by_app | chart: radar | x: factor | y: score, group: application -->\n\
     | Application | Factor | Score (native 1-4) | Normalized (0-100) |\n\
     |---|---|---|---|\n\
     | App One | Reliability | 3 ⁽ᵐ⁾ | 75 ⁽ᵐ⁾ |\n\
     | App One | Security | 2 ⁽ᵐ⁾ | 50 ⁽ᵐ⁾ |\n\
     | App Two | Reliability | 4 ⁽ᵐ⁾ | 100 ⁽ᵐ⁾ |\n\
     | App Two | Security | 1 ⁽ᵐ⁾ | 25 ⁽ᵐ⁾ |\n"
}

/// Why: a `bar` marker must emit a valid `xychart-beta` bar block under its table.
/// What: asserts the fence, chart header, quoted x-axis, y-axis, and bar series.
/// Test: this test.
#[test]
fn injects_bar_after_table() {
    let input = "<!-- dataset: tqi_benchmark_position | chart: bar | x: application | y: tqi_rank -->\n\
                 | Application | Peer set | Compliance % | Quartile | Rank | Rank total |\n\
                 |---|---|---|---|---|---|\n\
                 | App One | SaaS | 62% ⁽ᵈ⁾ | Q2 | 2 ⁽ᵈ⁾ | 8 |\n\
                 | App Two | SaaS | 40% ⁽ᵈ⁾ | Q3 | 5 ⁽ᵈ⁾ | 8 |\n";
    let out = inject(input);
    assert!(out.contains("```mermaid"), "block emitted:\n{out}");
    assert!(out.contains("xychart-beta"));
    assert!(out.contains("x-axis [\"App One\", \"App Two\"]"), "{out}");
    assert!(out.contains("bar [2, 5]"), "rank y-values resolved:\n{out}");
    // Block appears AFTER the table (the last table row precedes the fence).
    let table_pos = out.find("| App Two |").unwrap();
    let block_pos = out.find("```mermaid").unwrap();
    assert!(table_pos < block_pos);
}

/// Why: `radar` → `radar-beta` with a curve per group and the version-floor note.
/// What: asserts the `radar-beta` header, the version comment, axes, and curves.
/// Test: this test.
#[test]
fn renders_radar() {
    let out = inject(radar_input());
    assert!(out.contains("radar-beta"), "{out}");
    assert!(out.contains("%% radar-beta requires Mermaid >= 11.6"));
    // Axes = distinct x (factor) values; curves = distinct group (application).
    assert!(
        out.contains("axis a0[\"Reliability\"], a1[\"Security\"]"),
        "{out}"
    );
    assert!(out.contains("curve c0[\"App One\"]{3, 2}"), "{out}");
    assert!(out.contains("curve c1[\"App Two\"]{4, 1}"), "{out}");
}

/// Why: `stacked-bar` → layered `xychart-beta` with one bar series per group and
/// the documented no-native-stacking disclosure.
/// What: asserts the approximation comment, one bar line per group, and the
/// front-to-back legend note.
/// Test: this test.
#[test]
fn renders_stacked_bar() {
    let input = "<!-- dataset: violations_by_domain | chart: stacked-bar | x: application | y: violation_count, group: domain -->\n\
                 | Application | Domain | Violation count | Compliance % |\n\
                 |---|---|---|---|\n\
                 | App One | Security | 10 ⁽ᵐ⁾ | 80% |\n\
                 | App One | Reliability | 5 ⁽ᵐ⁾ | 90% |\n\
                 | App Two | Security | 3 ⁽ᵐ⁾ | 95% |\n";
    let out = inject(input);
    assert!(
        out.contains("%% stacked-bar approximated as layered"),
        "{out}"
    );
    assert!(out.contains("xychart-beta"));
    // Two group values (Security, Reliability) → two layered bar series.
    let bars = out.matches("    bar [").count();
    assert_eq!(bars, 2, "one bar per group:\n{out}");
    assert!(
        out.contains("_Layered series (front-to-back): Security, Reliability._"),
        "{out}"
    );
}

/// Why: `heatmap` has no Mermaid support — the fallback is a note, not a block.
/// What: asserts NO ```mermaid fence and the presence of the note.
/// Test: this test.
#[test]
fn heatmap_fallback_note_only() {
    let input = "<!-- dataset: cve_by_component_severity | chart: heatmap | x: component | y: severity -->\n\
                 | Application | Component | Severity | CVE ids / Count |\n\
                 |---|---|---|---|\n\
                 | App One | openssl | high | CVE-1 |\n";
    let out = inject(input);
    assert!(!out.contains("```mermaid"), "no block for heatmap:\n{out}");
    assert!(out.contains("_(heatmap: no Mermaid rendering; see table above)_"));
}

/// Why: an unknown/absent chart type must never panic and emit no block.
/// What: a marker with an unrecognized chart yields the table unchanged.
/// Test: this test.
#[test]
fn unknown_chart_no_block() {
    let input = "<!-- dataset: mystery | chart: bubble | x: a | y: b -->\n\
                 | A | B |\n|---|---|\n| x | 1 |\n";
    let out = inject(input);
    assert!(!out.contains("```mermaid"), "{out}");
}

/// Why: an empty dataset (table dropped by omit-empty) gets no chart.
/// What: a marker with no following table passes through untouched.
/// Test: this test.
#[test]
fn empty_dataset_no_chart() {
    let input = "<!-- dataset: loc_by_technology | chart: bar | x: application | y: loc -->\n\
                 \n## Next section\n";
    let out = inject(input);
    assert!(!out.contains("```mermaid"), "{out}");
    assert!(out.contains("## Next section"));
}

/// Why: numeric parsing must strip provenance markers, `$`, `%`, and thousands
/// separators before charting.
/// What: pins the cleaned parse across representative cell shapes.
/// Test: this test.
#[test]
fn parse_num_strips_markers() {
    assert_eq!(parse_num("8200 ⁽ᵐ⁾"), Some(8200.0));
    assert_eq!(parse_num("$1,200,000 ⁽ᵈ⁾"), Some(1_200_000.0));
    assert_eq!(parse_num("45% ⁽ⁱ⁾"), Some(45.0));
    assert_eq!(parse_num("3.5"), Some(3.5));
    assert_eq!(parse_num("n/a"), None);
    assert_eq!(parse_num(""), None);
}

/// Why: a non-numeric y skips only that row; a chart still renders from the rest.
/// What: one bad row is dropped, the good rows still chart.
/// Test: this test.
#[test]
fn skips_non_numeric_rows() {
    let input = "<!-- dataset: d | chart: bar | x: name | y: count -->\n\
                 | Name | Count |\n|---|---|\n| a | 5 |\n| b | n/a |\n| c | 7 |\n";
    let out = inject(input);
    assert!(
        out.contains("x-axis [\"a\", \"c\"]"),
        "bad row dropped:\n{out}"
    );
    assert!(out.contains("bar [5, 7]"));
}

/// Why: all-non-numeric y → no chart at all (nothing to plot).
/// What: a table whose y column never parses emits no block.
/// Test: this test.
#[test]
fn all_non_numeric_no_chart() {
    let input = "<!-- dataset: d | chart: bar | x: name | y: count -->\n\
                 | Name | Count |\n|---|---|\n| a | n/a |\n| b | tbd |\n";
    assert!(!inject(input).contains("```mermaid"));
}

/// Why: labels with spaces/quotes must be Mermaid-escaped (quoted; `\"` sanitized).
/// What: a category containing a double-quote renders with it replaced by `'`.
/// Test: this test.
#[test]
fn escapes_labels() {
    let input = "<!-- dataset: d | chart: bar | x: name | y: count -->\n\
                 | Name | Count |\n|---|---|\n| a \"b\" c | 5 |\n";
    let out = inject(input);
    assert!(out.contains("x-axis [\"a 'b' c\"]"), "{out}");
}

/// Why: categories are capped at [`MAX_CATEGORIES`] with an "and N more" note.
/// What: 15 categories → 12 charted + a "3 more categories" note.
/// Test: this test.
#[test]
fn caps_categories() {
    let mut input = String::from(
        "<!-- dataset: d | chart: bar | x: name | y: count -->\n| Name | Count |\n|---|---|\n",
    );
    for i in 0..15 {
        input.push_str(&format!("| cat{i} | {i} |\n"));
    }
    let out = inject(&input);
    assert!(out.contains("_… and 3 more categories omitted"), "{out}");
    // Only the first 12 categories appear in the axis list.
    assert!(out.contains("\"cat11\""));
    assert!(!out.contains("\"cat12\""));
}

/// Why: series (group values) are capped at [`MAX_SERIES`] with an overflow note.
/// What: 10 groups → 8 charted + a "2 more series" note.
/// Test: this test.
#[test]
fn caps_series() {
    let mut input = String::from(
        "<!-- dataset: d | chart: stacked-bar | x: cat | y: v, group: g -->\n| Cat | V | G |\n|---|---|---|\n",
    );
    for i in 0..10 {
        input.push_str(&format!("| c | {i} | grp{i} |\n"));
    }
    let out = inject(&input);
    assert!(out.contains("_… and 2 more series omitted"), "{out}");
}

/// Why: a marker with a group on a `bar` chart aggregates the group away (single
/// series summed per x), per the documented mapping.
/// What: two rows sharing an x with different groups sum into one bar.
/// Test: this test.
#[test]
fn bar_aggregates_group_away() {
    let input = "<!-- dataset: d | chart: bar | x: bucket | y: count, group: app -->\n\
                 | Bucket | Count | App |\n|---|---|---|\n\
                 | low | 3 | A |\n| low | 2 | B |\n| high | 4 | A |\n";
    let out = inject(input);
    assert!(out.contains("bar [5, 4]"), "low=3+2, high=4:\n{out}");
}

/// Why: content inside a fenced block must never be interpreted as a marker.
/// What: a dataset marker written inside a ``` fence is passed through verbatim,
/// no chart injected.
/// Test: this test.
#[test]
fn fenced_marker_is_opaque() {
    let input = "```\n<!-- dataset: d | chart: bar | x: a | y: b -->\n| A | B |\n|---|---|\n| x | 1 |\n```\n";
    let out = inject(input);
    assert!(
        !out.contains("xychart-beta"),
        "fenced marker not charted:\n{out}"
    );
}

/// Why: chart-type parsing is the closed vocabulary the mapping keys off.
/// What: pins each recognized value and the unknown fallback.
/// Test: this test.
#[test]
fn parses_chart_types() {
    assert_eq!(parse_chart("bar"), ChartType::Bar);
    assert_eq!(parse_chart("stacked-bar"), ChartType::StackedBar);
    assert_eq!(parse_chart("radar"), ChartType::Radar);
    assert_eq!(parse_chart("heatmap"), ChartType::Heatmap);
    assert_eq!(parse_chart("pie"), ChartType::Unknown);
}

/// Why: column resolution maps semantic field names to real headers.
/// What: exact match, last-token match, and a miss.
/// Test: this test.
#[test]
fn resolves_columns() {
    let header: Vec<String> = ["Application", "Factor", "Score (native 1-4)"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(resolve_column(&header, "factor"), Some(1));
    assert_eq!(resolve_column(&header, "score"), Some(2));
    assert_eq!(resolve_column(&header, "application"), Some(0));
    assert_eq!(resolve_column(&header, "nonesuch"), None);

    let ranks: Vec<String> = ["Application", "Rank", "Rank total"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(resolve_column(&ranks, "tqi_rank"), Some(1));
}

/// Why: a full end-to-end report with ≥2 populated datasets must emit a valid
/// mermaid block after EACH table (integration of the injection loop).
/// What: a bar dataset followed by a radar dataset both get their blocks.
/// Test: this test.
#[test]
fn two_datasets_both_charted() {
    let input = format!(
        "## 7. Graph-Ready Data Appendix\n\n\
         <!-- dataset: loc | chart: bar | x: tech | y: loc -->\n\
         | Tech | LoC |\n|---|---|\n| Rust | 8200 ⁽ᵐ⁾ |\n| Python | 3100 ⁽ᵐ⁾ |\n\n{}",
        radar_input()
    );
    let out = inject(&input);
    assert_eq!(out.matches("```mermaid").count(), 2, "two blocks:\n{out}");
    assert!(out.contains("xychart-beta"));
    assert!(out.contains("radar-beta"));
}
