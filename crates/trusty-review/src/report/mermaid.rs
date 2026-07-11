//! Mermaid chart rendering from Graph-Ready Data Appendix markers (wave 4, #2366).
//!
//! Why: §7's `<!-- dataset: <slug> | chart: <type> | x: <field> | y: <field>[,
//! group: <field>] -->` markers already declare a chart intent per populated
//! table, but the marker was opaque to the renderer — a reader saw only the
//! machine-readable pipe table.  This pass turns each POPULATED dataset table into
//! a human-viewable Mermaid chart emitted directly under it, deterministically
//! (pure rendering from the table rows — no LLM, no network).  The pipe table
//! stays the authoritative source of truth; the chart is a derived view.
//! What: [`inject`] scans filled+polished markdown, and for each dataset marker
//! whose following table is populated appends a fenced ```mermaid block per the
//! chart-type mapping (bar/stacked-bar → `xychart-beta`, radar → `radar-beta`,
//! heatmap → a note-only fallback).  Empty datasets (table dropped by omit-empty)
//! get no chart.  Numeric parsing strips provenance markers / separators; labels
//! are Mermaid-escaped and capped.
//! Test: `mermaid_tests.rs` covers per-chart-type syntax, numeric parsing, label
//! escaping + caps, the heatmap fallback, empty datasets, and end-to-end
//! injection under populated tables.

use tracing::debug;

/// Max distinct x-categories rendered before the remainder is folded into a note.
///
/// Why: a chart with dozens of categories is illegible; the pipe table above the
/// chart remains authoritative for the full set, so the chart is capped for
/// readability and the overflow is disclosed.
const MAX_CATEGORIES: usize = 12;

/// Max distinct series/curves (group values) rendered before an overflow note.
const MAX_SERIES: usize = 8;

/// The note emitted in place of a Mermaid block for a `heatmap` dataset.
///
/// Why: Mermaid has no native heatmap; rather than a misleading approximation the
/// pipe table stays the authoritative artifact and a one-line note explains why
/// no chart is drawn.
const HEATMAP_NOTE: &str = "_(heatmap: no Mermaid rendering; see table above)_";

/// Inject a ```mermaid block under every populated dataset table.
///
/// Why: the single entry point the reporter calls (when mermaid is enabled) after
/// the polish pass, so charts render into the final §7 appendix without the
/// reporter knowing the chart grammar.
/// What: walks `markdown` line by line tracking fenced regions (opaque — never
/// interpreted); on a `<!-- dataset: … -->` marker it copies the marker, the
/// following table, then appends the rendered block (or a note) derived from the
/// table rows.  A dataset whose table was dropped (omit-empty) gets no block.
/// The original trailing-newline state is preserved so a disabled run stays
/// byte-identical (this fn is simply not called when disabled).
/// Test: `mermaid_tests.rs::{injects_bar_after_table, empty_dataset_no_chart}`.
pub fn inject(markdown: &str) -> String {
    let lines: Vec<&str> = markdown.lines().collect();
    let mut out = String::with_capacity(markdown.len() + 512);
    let mut i = 0usize;
    let mut in_fence = false;

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        // Fenced regions (evidence quotes, and any ```mermaid we may have already
        // emitted) are opaque — never interpret a marker inside one.
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            push_line(&mut out, line);
            i += 1;
            continue;
        }
        if in_fence {
            push_line(&mut out, line);
            i += 1;
            continue;
        }

        if let Some(marker) = parse_marker(trimmed) {
            push_line(&mut out, line);
            i += 1;
            // Skip/copy blank lines between the marker and its table.
            while i < lines.len() && lines[i].trim().is_empty() {
                push_line(&mut out, lines[i]);
                i += 1;
            }
            // Copy the contiguous table (if any), then append the chart.
            if i < lines.len() && is_table_line(lines[i].trim()) {
                let start = i;
                while i < lines.len() && is_table_line(lines[i].trim()) {
                    push_line(&mut out, lines[i]);
                    i += 1;
                }
                if let Some(table) = parse_table(&lines[start..i])
                    && let Some(block) = render_block(&marker, &table)
                {
                    out.push('\n');
                    out.push_str(&block);
                }
            }
            continue;
        }

        push_line(&mut out, line);
        i += 1;
    }

    // `lines()` drops the trailing newline; restore the original state.
    if !markdown.ends_with('\n') {
        while out.ends_with('\n') {
            out.pop();
        }
    }
    out
}

/// Push a line plus a trailing newline.
fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

// ─── Marker parsing ─────────────────────────────────────────────────────────

/// The chart intent declared by a dataset marker.
///
/// Why: the `chart:` field is a closed vocabulary; an explicit enum makes the
/// per-type mapping exhaustive and keeps an unknown value from panicking.
/// What: the four documented types plus `Unknown` for any unrecognized/absent
/// value (rendered as no block + a debug log).
/// Test: `mermaid_tests.rs::parses_chart_types`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChartType {
    /// Single-series bars (`xychart-beta`); a declared `group:` is aggregated away.
    Bar,
    /// One bar series per group value (`xychart-beta`); layered, NOT truly stacked.
    StackedBar,
    /// One curve per group value (`radar-beta`, Mermaid ≥ 11.6).
    Radar,
    /// No native Mermaid support — note-only fallback, table stays authoritative.
    Heatmap,
    /// Unrecognized or absent chart type — no block emitted.
    Unknown,
}

/// A parsed dataset marker: chart intent plus the field names naming its columns.
struct Marker {
    /// Dataset slug — used as the chart title (humanized).
    slug: String,
    /// The declared chart type.
    chart: ChartType,
    /// The x-axis (category) field name.
    x: String,
    /// The y-axis (numeric value) field name.
    y: String,
    /// Optional grouping (series/curve) field name.
    group: Option<String>,
}

/// Parse a `<!-- dataset: … -->` marker line into a [`Marker`].
///
/// Why: the marker declares every field the chart needs; parsing it up front
/// keeps the per-type renderers free of grammar concerns.
/// What: strips the comment delimiters, splits on `|` AND `,` into `key: value`
/// pairs (the `group:` field lives comma-appended inside the `y:` segment), and
/// collects `dataset`/`chart`/`x`/`y`/`group`.  Returns `None` for any non-dataset
/// comment or one missing the required `dataset`/`x`/`y` fields.
/// Test: `mermaid_tests.rs::{parses_full_marker, ignores_non_dataset_comment}`.
fn parse_marker(trimmed: &str) -> Option<Marker> {
    let inner = trimmed.strip_prefix("<!--")?.strip_suffix("-->")?.trim();
    if !inner.starts_with("dataset:") {
        return None;
    }
    let mut slug = None;
    let mut chart = ChartType::Unknown;
    let mut x = None;
    let mut y = None;
    let mut group = None;
    for token in inner.replace('|', ",").split(',') {
        let Some((key, val)) = token.split_once(':') else {
            continue;
        };
        let val = val.trim().to_string();
        match key.trim() {
            "dataset" => slug = Some(val),
            "chart" => chart = parse_chart(&val),
            "x" => x = Some(val),
            "y" => y = Some(val),
            "group" => group = Some(val),
            _ => {}
        }
    }
    Some(Marker {
        slug: slug?,
        chart,
        x: x?,
        y: y?,
        group: group.filter(|g| !g.is_empty()),
    })
}

/// Map the `chart:` field value to a [`ChartType`].
fn parse_chart(s: &str) -> ChartType {
    match s.trim().to_ascii_lowercase().as_str() {
        "bar" => ChartType::Bar,
        "stacked-bar" | "stacked_bar" | "stacked" => ChartType::StackedBar,
        "radar" => ChartType::Radar,
        "heatmap" => ChartType::Heatmap,
        _ => ChartType::Unknown,
    }
}

// ─── Table parsing ──────────────────────────────────────────────────────────

/// A parsed pipe table: header cells plus body rows (separator row dropped).
struct Table {
    /// Column headers (trimmed, outer pipes removed).
    header: Vec<String>,
    /// Body rows, each a vector of trimmed cell strings.
    rows: Vec<Vec<String>>,
}

/// Parse a contiguous run of `|`-lines into a [`Table`].
///
/// Why: the chart renderers work off resolved columns; parsing once yields the
/// header (for column resolution) and the body rows (for values).
/// What: first line is the header, the `|---|` separator is skipped, remaining
/// non-separator lines are body rows.  Returns `None` when there is no body.
/// Test: `mermaid_tests.rs::parses_table_rows`.
fn parse_table(lines: &[&str]) -> Option<Table> {
    let mut iter = lines.iter().map(|l| l.trim());
    let header = cells_of(iter.next()?);
    let rows: Vec<Vec<String>> = iter
        .filter(|l| !is_separator_row(l))
        .map(cells_of)
        .collect();
    if rows.is_empty() {
        return None;
    }
    Some(Table { header, rows })
}

/// True when a trimmed line belongs to a markdown pipe table.
fn is_table_line(trimmed: &str) -> bool {
    trimmed.starts_with('|')
}

/// True when a table line is the `|---|---|` separator row.
fn is_separator_row(trimmed: &str) -> bool {
    let inner = trimmed.trim_matches('|');
    !inner.is_empty()
        && inner
            .chars()
            .all(|c| matches!(c, '-' | ':' | '|' | ' ' | '\t'))
}

/// Split a table row into trimmed cell strings (outer pipes dropped).
fn cells_of(trimmed: &str) -> Vec<String> {
    trimmed
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

// ─── Chart rendering ────────────────────────────────────────────────────────

/// Render the Mermaid block (or note) for one marker + its table, if any.
///
/// Why: the single dispatch over chart type keeps [`inject`] agnostic of the
/// grammar and centralizes the "no block" cases (heatmap note, unknown, no data).
/// What: resolves the x/y/group columns by name, then delegates to the per-type
/// builder.  `Heatmap` → the fallback note; `Unknown` → `None` + a debug log;
/// bar/stacked-bar/radar → a ```mermaid block, or `None` when no row yields a
/// numeric y (chart would be empty).
/// Test: `mermaid_tests.rs::{renders_bar, heatmap_fallback, unknown_no_block}`.
fn render_block(marker: &Marker, table: &Table) -> Option<String> {
    if marker.chart == ChartType::Heatmap {
        return Some(format!("{HEATMAP_NOTE}\n"));
    }
    if marker.chart == ChartType::Unknown {
        debug!(slug = %marker.slug, "unknown/absent chart type — no mermaid block");
        return None;
    }

    let x_col = resolve_column(&table.header, &marker.x).unwrap_or(0);
    let y_col = resolve_column(&table.header, &marker.y)?;
    let group_col = marker
        .group
        .as_ref()
        .and_then(|g| resolve_column(&table.header, g));

    match marker.chart {
        ChartType::Bar => render_bar(marker, table, x_col, y_col),
        ChartType::StackedBar => render_stacked(marker, table, x_col, y_col, group_col),
        ChartType::Radar => render_radar(marker, table, x_col, y_col, group_col),
        _ => None,
    }
}

/// Aggregated chart data: categories (x) × series (group) numeric sums.
struct Aggregated {
    /// Distinct x-values in first-seen order (capped at [`MAX_CATEGORIES`]).
    categories: Vec<String>,
    /// Count of x-values dropped by the category cap.
    cat_overflow: usize,
    /// One series per group value, each aligned to `categories`.
    series: Vec<Series>,
    /// Count of group values dropped by the series cap.
    series_overflow: usize,
}

/// A single chart series: a group label plus one value per category.
struct Series {
    /// The group value (empty for the no-group single-series case).
    label: String,
    /// Numeric value per category (summed over matching rows; 0 when absent).
    values: Vec<f64>,
}

/// Aggregate table rows into categories × series by summing numeric y-values.
///
/// Why: bar/stacked/radar all reduce to "sum y per (x, group)"; sharing the
/// aggregation keeps the numeric-parse, dedup, and cap rules in one place.
/// What: for each row parses y (non-numeric rows skipped), keys the sum on the
/// distinct x value and — when `group_col` is set — the distinct group value.
/// Applies the category/series caps, recording the dropped counts.  Returns
/// `None` when no row yielded a numeric y (the chart would be empty).
/// Test: `mermaid_tests.rs::{skips_non_numeric_rows, caps_categories}`.
fn aggregate(
    table: &Table,
    x_col: usize,
    y_col: usize,
    group_col: Option<usize>,
) -> Option<Aggregated> {
    let mut categories: Vec<String> = Vec::new();
    let mut groups: Vec<String> = Vec::new();
    let mut sums: std::collections::HashMap<(usize, usize), f64> = std::collections::HashMap::new();
    let mut any = false;

    for row in &table.rows {
        let Some(yv) = row.get(y_col).and_then(|c| parse_num(c)) else {
            continue;
        };
        any = true;
        let x = row
            .get(x_col)
            .map(|s| s.trim())
            .unwrap_or_default()
            .to_string();
        let g = match group_col {
            Some(c) => row.get(c).map(|s| s.trim()).unwrap_or_default().to_string(),
            None => String::new(),
        };
        let ci = index_or_push(&mut categories, &x);
        let gi = index_or_push(&mut groups, &g);
        *sums.entry((ci, gi)).or_insert(0.0) += yv;
    }
    if !any {
        return None;
    }

    let cat_overflow = categories.len().saturating_sub(MAX_CATEGORIES);
    categories.truncate(MAX_CATEGORIES);
    let series_overflow = groups.len().saturating_sub(MAX_SERIES);
    groups.truncate(MAX_SERIES);

    let series = groups
        .iter()
        .enumerate()
        .map(|(gi, label)| Series {
            label: label.clone(),
            values: (0..categories.len())
                .map(|ci| *sums.get(&(ci, gi)).unwrap_or(&0.0))
                .collect(),
        })
        .collect();

    Some(Aggregated {
        categories,
        cat_overflow,
        series,
        series_overflow,
    })
}

/// Return the index of `value` in `vec`, appending it (first-seen order) if new.
fn index_or_push(vec: &mut Vec<String>, value: &str) -> usize {
    if let Some(i) = vec.iter().position(|v| v == value) {
        i
    } else {
        vec.push(value.to_string());
        vec.len() - 1
    }
}

/// Build a `bar` chart: single `xychart-beta` bar series over the x categories.
///
/// Why: the documented mapping — `bar` is always one series; a declared `group:`
/// is aggregated away (summed per x) so the bars total across groups.
/// What: aggregates with no group column, emits the `xychart-beta` header, the
/// quoted x-axis categories, a y-axis label, and one `bar [ … ]` line.
/// Test: `mermaid_tests.rs::renders_bar`.
fn render_bar(marker: &Marker, table: &Table, x_col: usize, y_col: usize) -> Option<String> {
    let agg = aggregate(table, x_col, y_col, None)?;
    let mut b = String::new();
    b.push_str("```mermaid\n");
    b.push_str("xychart-beta\n");
    b.push_str(&format!("    title \"{}\"\n", humanize(&marker.slug)));
    b.push_str(&format!("    x-axis [{}]\n", axis_list(&agg.categories)));
    b.push_str(&format!("    y-axis \"{}\"\n", humanize(&marker.y)));
    b.push_str(&format!("    bar [{}]\n", num_list(&agg.series[0].values)));
    b.push_str("```\n");
    b.push_str(&overflow_notes(agg.cat_overflow, 0));
    Some(b)
}

/// Build a `stacked-bar` chart: one bar series per group, layered (overlaid).
///
/// Why: Mermaid `xychart-beta` has NO true stacking — multiple `bar` series are
/// drawn overlaid, not stacked.  This is an intentional approximation: the
/// layering preserves per-group magnitude comparison, and the note + `%%` comment
/// disclose that the bars are not summed on top of one another.
/// What: aggregates by group column (falling back to a single series when the
/// group does not resolve), emits one `bar [ … ]` per series plus a legend note
/// naming the series order (xychart has no built-in legend).
/// Test: `mermaid_tests.rs::renders_stacked_bar`.
fn render_stacked(
    marker: &Marker,
    table: &Table,
    x_col: usize,
    y_col: usize,
    group_col: Option<usize>,
) -> Option<String> {
    let agg = aggregate(table, x_col, y_col, group_col)?;
    let mut b = String::new();
    b.push_str("```mermaid\n");
    b.push_str(
        "%% stacked-bar approximated as layered (overlaid) bars — Mermaid has no native stacking\n",
    );
    b.push_str("xychart-beta\n");
    b.push_str(&format!("    title \"{}\"\n", humanize(&marker.slug)));
    b.push_str(&format!("    x-axis [{}]\n", axis_list(&agg.categories)));
    b.push_str(&format!("    y-axis \"{}\"\n", humanize(&marker.y)));
    for s in &agg.series {
        b.push_str(&format!("    bar [{}]\n", num_list(&s.values)));
    }
    b.push_str("```\n");
    let labels: Vec<&str> = agg.series.iter().map(|s| s.label.as_str()).collect();
    if group_col.is_some() && !labels.iter().all(|l| l.is_empty()) {
        b.push_str(&format!(
            "_Layered series (front-to-back): {}._\n",
            labels.join(", ")
        ));
    }
    b.push_str(&overflow_notes(agg.cat_overflow, agg.series_overflow));
    Some(b)
}

/// Build a `radar` chart (`radar-beta`, Mermaid ≥ 11.6): one curve per group.
///
/// Why: a radar compares many factors across a few entities — axes are the x
/// factors, curves are the group entities.  `radar-beta` is gated on Mermaid
/// 11.6; the `%%` comment records the version floor in the emitted block.
/// What: aggregates by group column (single curve labeled after the y field when
/// no group resolves), emits the `axis` line and one `curve …{ … }` per series.
/// Test: `mermaid_tests.rs::renders_radar`.
fn render_radar(
    marker: &Marker,
    table: &Table,
    x_col: usize,
    y_col: usize,
    group_col: Option<usize>,
) -> Option<String> {
    let agg = aggregate(table, x_col, y_col, group_col)?;
    let mut b = String::new();
    b.push_str("```mermaid\n");
    b.push_str("%% radar-beta requires Mermaid >= 11.6\n");
    b.push_str("radar-beta\n");
    b.push_str(&format!("    title \"{}\"\n", humanize(&marker.slug)));
    let axes: Vec<String> = agg
        .categories
        .iter()
        .enumerate()
        .map(|(i, c)| format!("a{i}{}", bracket_label(c)))
        .collect();
    b.push_str(&format!("    axis {}\n", axes.join(", ")));
    for (i, s) in agg.series.iter().enumerate() {
        let label = if s.label.is_empty() {
            humanize(&marker.y)
        } else {
            s.label.clone()
        };
        b.push_str(&format!(
            "    curve c{i}{}{{{}}}\n",
            bracket_label(&label),
            num_list(&s.values)
        ));
    }
    b.push_str("```\n");
    b.push_str(&overflow_notes(agg.cat_overflow, agg.series_overflow));
    Some(b)
}

/// The overflow disclosure note(s) for dropped categories/series.
fn overflow_notes(cat_overflow: usize, series_overflow: usize) -> String {
    let mut s = String::new();
    if cat_overflow > 0 {
        s.push_str(&format!(
            "_… and {cat_overflow} more categories omitted from the chart; see table above._\n"
        ));
    }
    if series_overflow > 0 {
        s.push_str(&format!(
            "_… and {series_overflow} more series omitted from the chart; see table above._\n"
        ));
    }
    s
}

// ─── Column resolution + value helpers ──────────────────────────────────────

/// Resolve a marker field name to a table column index by fuzzy name matching.
///
/// Why: marker field names (`factor`, `tqi_rank`) are semantic hints, not exact
/// header text (`Factor`, `Rank`); a tiered normalized match maps them robustly
/// without the marker author having to echo the exact column header.
/// What: normalizes both sides to lowercase alphanumerics and tries, in priority
/// order over the full field then its last `_`/space token: exact equality,
/// header-starts-with, header-contains.  Returns the first matching column.
/// Test: `mermaid_tests.rs::{resolves_exact_column, resolves_by_last_token}`.
fn resolve_column(header: &[String], field: &str) -> Option<usize> {
    let full = normalize(field);
    let last_raw = field.rsplit(['_', ' ']).next().unwrap_or(field);
    let last = normalize(last_raw);
    let norms: Vec<String> = header.iter().map(|h| normalize(h)).collect();
    let targets = [&full, &last];

    for t in targets {
        if !t.is_empty()
            && let Some(i) = norms.iter().position(|h| h == t)
        {
            return Some(i);
        }
    }
    for t in targets {
        if !t.is_empty()
            && let Some(i) = norms.iter().position(|h| h.starts_with(t.as_str()))
        {
            return Some(i);
        }
    }
    for t in targets {
        if !t.is_empty()
            && let Some(i) = norms.iter().position(|h| h.contains(t.as_str()))
        {
            return Some(i);
        }
    }
    None
}

/// Lowercase a string down to its ASCII alphanumerics (drop spaces/punctuation).
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Parse a numeric y-value cell, tolerant of provenance markers and separators.
///
/// Why: rendered value cells carry provenance superscripts (` ⁽ᵐ⁾`), thousands
/// separators, `$`/`%`, and stray whitespace; the chart needs the bare number.
/// What: strips the provenance superscript codepoints and `$ % , ` whitespace,
/// then parses the remainder as `f64`.  A non-numeric remainder yields `None`
/// (the caller skips that row).
/// Test: `mermaid_tests.rs::parse_num_strips_markers`.
fn parse_num(cell: &str) -> Option<f64> {
    let cleaned: String = cell
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '$' | '%' | ',' | ' ' | '\t' | '⁽' | '⁾' | 'ᵐ' | 'ᵈ' | 'ⁱ'
            )
        })
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        cleaned.parse::<f64>().ok()
    }
}

/// Format an `f64` compactly: integers without a decimal, else trimmed decimals.
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Join numeric values as a comma-separated Mermaid list.
fn num_list(values: &[f64]) -> String {
    values
        .iter()
        .map(|v| fmt_num(*v))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Join category labels as a comma-separated list of quoted Mermaid strings.
fn axis_list(cats: &[String]) -> String {
    cats.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ")
}

/// A Mermaid double-quoted label with internal quotes sanitized.
fn quote(label: &str) -> String {
    let clean = label.trim().replace('"', "'");
    if clean.is_empty() {
        "\"?\"".to_string()
    } else {
        format!("\"{clean}\"")
    }
}

/// A `radar-beta` bracketed, quoted label: `["Label"]`.
fn bracket_label(label: &str) -> String {
    format!("[{}]", quote(label))
}

/// Humanize a slug/field for a chart title: underscores → spaces, trimmed.
fn humanize(s: &str) -> String {
    s.replace('_', " ").trim().to_string()
}

#[cfg(test)]
#[path = "mermaid_tests.rs"]
mod tests;
