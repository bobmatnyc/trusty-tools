//! Interactive numbered-list component picker for `tctl install`.
//!
//! Why: When the user runs `tctl install` with no arguments in a TTY, presenting
//! a numbered menu lets them opt into a subset of the stable set rather than
//! blindly installing everything. The confirmation-first design (Phase 4,
//! SPEC-INSTALLER-01) keeps the default safe while making selective installs
//! discoverable without needing to know crate names in advance.
//!
//! What: Renders a numbered table of selectable components (index, crate name,
//! short description) to stderr, reads a line from stdin, and returns the chosen
//! [`crate::commands::stable_set::StableMember`] slice. The parsing logic
//! [`parse_selection`] is pure and unit-testable without touching stdin.
//!
//! Test: `tests` exercises `parse_selection` for all input variants (numeric
//! indices, `all`, `none`/empty, out-of-range, non-numeric, whitespace). The
//! interactive I/O surface (`prompt_picker`) is side-effect-only and validated
//! manually.

use crate::commands::stable_set::StableMember;

/// A short human-readable description for each stable-set member.
///
/// Why: The picker table needs a "what does this do?" column so the operator can
/// make an informed choice without looking up documentation. The descriptions are
/// static and co-located with the picker so they evolve together with the set.
///
/// What: Returns the description string for `crate_name`, falling back to
/// `"trusty-* component"` when no entry is found (forward-compat for future
/// members).
///
/// Test: `tests::description_for_known_and_unknown`.
pub fn member_description(crate_name: &str) -> &'static str {
    match crate_name {
        "trusty-search" => "hybrid semantic+lexical code-search daemon + MCP server",
        "trusty-memory" => "persistent memory palace MCP server",
        "trusty-analyze" => "code quality and complexity analysis daemon",
        "trusty-review" => "automated code-review MCP server",
        "tga" => "git analytics CLI (blame, churn, contributor heatmaps)",
        "trusty-console" => "web console proxying the trusty-* daemon suite",
        "trusty-mpm" => "multi-session process manager and agent orchestrator",
        _ => "trusty-* component",
    }
}

/// Parse a selection string into zero-based component indices.
///
/// Why: Separating parsing from I/O makes the selection logic fully unit-testable
/// without spawning a real terminal, enabling exhaustive edge-case coverage before
/// the function is wired into interactive flow.
///
/// What: Accepts `"all"` (returns `0..n`), `"none"` or empty string (returns
/// `[]`), or a comma-and/or-whitespace-separated list of 1-based integers (e.g.
/// `"1, 3"` → `[0, 2]`). Returns an error string describing the first problem on
/// invalid input (non-numeric token, index out of range 1..=n).
///
/// The returned indices are **0-based** (ready to index into the components
/// slice) and deduplicated, preserving the order of first appearance.
///
/// Test: `tests::parse_*` covers all variants.
pub fn parse_selection(input: &str, n: usize) -> Result<Vec<usize>, String> {
    let trimmed = input.trim();

    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Ok(Vec::new());
    }

    if trimmed.eq_ignore_ascii_case("all") {
        return Ok((0..n).collect());
    }

    // Split on commas and/or whitespace.
    let tokens: Vec<&str> = trimmed
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|t| !t.is_empty())
        .collect();

    let mut indices = Vec::with_capacity(tokens.len());
    for token in &tokens {
        let one_based: usize = token
            .parse::<usize>()
            .map_err(|_| format!("not a number: {token:?}"))?;
        if one_based == 0 || one_based > n {
            return Err(format!(
                "index {one_based} is out of range (valid: 1..={n})"
            ));
        }
        let zero_based = one_based - 1;
        if !indices.contains(&zero_based) {
            indices.push(zero_based);
        }
    }

    Ok(indices)
}

/// Display the picker menu and return the chosen components.
///
/// Why: The interactive entry point: renders the numbered table to stderr (so
/// stdout stays clean for data), reads one line from stdin, and re-prompts once
/// on invalid input before aborting. Callers must have confirmed that
/// `progress_ui::is_tty()` is true before calling this.
///
/// What: Prints a numbered list of `components` (index, crate name, description)
/// and a selection prompt. Accepts `all`, `none`/empty, or comma/space-separated
/// 1-based indices. On invalid input: re-prompts once with an error note, then
/// returns an error if the second attempt also fails. On success returns the
/// selected [`StableMember`] slice in stable-set order.
///
/// Test: Side-effect-only (stderr + stdin); `parse_selection` is unit-tested.
pub fn prompt_picker(components: &[StableMember]) -> anyhow::Result<Vec<StableMember>> {
    use std::io::Write;

    print_menu(components);

    let first = read_line()?;
    match parse_selection(&first, components.len()) {
        Ok(indices) => Ok(indices.into_iter().map(|i| components[i].clone()).collect()),
        Err(e) => {
            // Re-prompt once with the error note.
            eprintln!("  (invalid: {e} — try again or type 'all'/'none')");
            eprint!("  Selection: ");
            let _ = std::io::stderr().flush();
            let second = read_line()?;
            let indices = parse_selection(&second, components.len()).map_err(|e2| {
                anyhow::anyhow!("invalid selection: {e2} (aborting — re-run tctl install)")
            })?;
            Ok(indices.into_iter().map(|i| components[i].clone()).collect())
        }
    }
}

/// Print the numbered component menu to stderr.
///
/// Why: Separated from the readline loop so the table format can be reviewed and
/// adjusted without touching the I/O control flow.
///
/// What: Emits a header, one row per component (`  [N]  <crate_name>  —  <desc>`),
/// and a prompt line to stderr.
///
/// Test: Side-effect-only; visually inspected manually.
fn print_menu(components: &[StableMember]) {
    eprintln!();
    eprintln!("  Select components to install:");
    eprintln!("  ─────────────────────────────────────────────────────────────────────");
    for (i, m) in components.iter().enumerate() {
        let desc = member_description(&m.crate_name);
        eprintln!("  [{:>2}]  {:<20}  —  {}", i + 1, m.crate_name, desc);
    }
    eprintln!("  ─────────────────────────────────────────────────────────────────────");
    eprintln!("  Enter numbers (e.g. 1,3), 'all', or 'none'/empty to skip.");
    eprint!("  Selection: ");
    let _ = std::io::Write::flush(&mut std::io::stderr());
}

/// Read one line from stdin, trimming the trailing newline.
///
/// Why: Centralises the stdin read so `prompt_picker` is not littered with
/// identical `BufRead` boilerplate.
///
/// What: Returns the trimmed line, or an `anyhow::Error` on I/O failure.
///
/// Test: Side-effect-only.
fn read_line() -> anyhow::Result<String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| anyhow::anyhow!("failed to read selection: {e}"))?;
    Ok(line.trim_end_matches(['\n', '\r']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::stable_set::stable_set;

    // ── parse_selection ──────────────────────────────────────────────────────

    /// Why: `"all"` is the quick-select shortcut; it must return every index.
    /// What: Asserts `parse_selection("all", 4)` returns `[0, 1, 2, 3]`.
    /// Test: This is the test.
    #[test]
    fn parse_all_returns_every_index() {
        assert_eq!(parse_selection("all", 4).unwrap(), vec![0, 1, 2, 3]);
    }

    /// Why: Case-insensitivity avoids frustrating rejections for `ALL` / `All`.
    /// What: Asserts `"ALL"` and `"All"` both succeed.
    /// Test: This is the test.
    #[test]
    fn parse_all_case_insensitive() {
        assert_eq!(parse_selection("ALL", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(parse_selection("All", 3).unwrap(), vec![0, 1, 2]);
    }

    /// Why: `"none"` and empty string both mean "install nothing" — an opt-out.
    /// What: Asserts both return an empty index list.
    /// Test: This is the test.
    #[test]
    fn parse_none_and_empty_return_empty() {
        assert!(parse_selection("none", 4).unwrap().is_empty());
        assert!(parse_selection("", 4).unwrap().is_empty());
        assert!(parse_selection("   ", 4).unwrap().is_empty());
        assert!(parse_selection("NONE", 4).unwrap().is_empty());
    }

    /// Why: The picker displays 1-based indices; the function must convert to 0-based.
    /// What: `"1,3"` on n=4 returns `[0, 2]`.
    /// Test: This is the test.
    #[test]
    fn parse_comma_separated_indices_converts_to_zero_based() {
        assert_eq!(parse_selection("1,3", 4).unwrap(), vec![0, 2]);
    }

    /// Why: Space-separated input is a natural alternative to comma-separated.
    /// What: `"2 4"` on n=4 returns `[1, 3]`.
    /// Test: This is the test.
    #[test]
    fn parse_space_separated_indices() {
        assert_eq!(parse_selection("2 4", 4).unwrap(), vec![1, 3]);
    }

    /// Why: Mixed comma+space is a common real-world input form.
    /// What: `"1, 3"` (comma then space) on n=4 returns `[0, 2]`.
    /// Test: This is the test.
    #[test]
    fn parse_mixed_separator() {
        assert_eq!(parse_selection("1, 3", 4).unwrap(), vec![0, 2]);
    }

    /// Why: Duplicate indices from the user should not install a member twice.
    /// What: `"1,1,2"` on n=3 deduplicates to `[0, 1]`.
    /// Test: This is the test.
    #[test]
    fn parse_deduplicates_indices() {
        assert_eq!(parse_selection("1,1,2", 3).unwrap(), vec![0, 1]);
    }

    /// Why: A non-numeric token must produce a clear error, not a panic.
    /// What: `"foo"` returns `Err` containing "not a number".
    /// Test: This is the test.
    #[test]
    fn parse_non_numeric_is_error() {
        let err = parse_selection("foo", 4).unwrap_err();
        assert!(err.contains("not a number"), "got: {err}");
    }

    /// Why: Index 0 is invalid (menu is 1-based); it must be rejected.
    /// What: `"0"` on n=4 returns an out-of-range error.
    /// Test: This is the test.
    #[test]
    fn parse_zero_index_is_out_of_range() {
        let err = parse_selection("0", 4).unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
    }

    /// Why: An index exceeding n must be rejected with a clear message.
    /// What: `"5"` on n=4 returns an out-of-range error.
    /// Test: This is the test.
    #[test]
    fn parse_index_exceeding_n_is_error() {
        let err = parse_selection("5", 4).unwrap_err();
        assert!(err.contains("out of range"), "got: {err}");
    }

    /// Why: Surrounding whitespace should not cause parse failures.
    /// What: `"  1 , 2  "` on n=4 returns `[0, 1]`.
    /// Test: This is the test.
    #[test]
    fn parse_tolerates_surrounding_whitespace() {
        assert_eq!(parse_selection("  1 , 2  ", 4).unwrap(), vec![0, 1]);
    }

    // ── index → crate name resolution ────────────────────────────────────────

    /// Why: The caller resolves indices back to StableMember via slice indexing;
    /// confirm the round-trip yields the expected crate names.
    /// What: Selects indices [0, 2] from the real stable_set, asserts names.
    /// Test: This is the test.
    #[test]
    fn indices_resolve_to_correct_crate_names() {
        let components = stable_set();
        let indices = parse_selection("1,3", components.len()).unwrap();
        let names: Vec<&str> = indices
            .iter()
            .map(|&i| components[i].crate_name.as_str())
            .collect();
        assert_eq!(names, vec!["trusty-search", "trusty-analyze"]);
    }

    // ── member_description ───────────────────────────────────────────────────

    /// Why: Every member of the stable set must have a non-fallback description.
    /// What: Asserts none of the known crate names return the fallback string.
    /// Test: This is the test.
    #[test]
    fn description_for_known_and_unknown() {
        for m in stable_set() {
            let d = member_description(&m.crate_name);
            assert_ne!(
                d, "trusty-* component",
                "{} has no custom description",
                m.crate_name
            );
        }
        // Unknown name gets the fallback.
        assert_eq!(
            member_description("nonexistent-crate"),
            "trusty-* component"
        );
    }
}
