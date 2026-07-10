//! Tests for the deterministic fill engine.
//!
//! Why: the fill engine implements the honesty rule and block repetition that
//! the whole report layer depends on; its scalar substitution, missing-value
//! marker, single/repeated/nested block behaviour, and dataset-comment
//! preservation must be pinned.
//! What: builds scopes directly and asserts on rendered output.
//! Test: included as `#[cfg(test)] mod tests` from `fill.rs`.

use super::{HONESTY_MARKER, Scope, render, strip_leading_comment};

/// Why: scalars must substitute verbatim from the scope.
/// What: sets one scalar and asserts it replaces the placeholder.
/// Test: this test itself.
#[test]
fn scalar_substitution() {
    let mut scope = Scope::new();
    scope.set("name", "Acme");
    let out = render("Project: {{name}}", &scope);
    assert_eq!(out, "Project: Acme");
}

/// Why: the honesty rule forbids leaving `{{…}}` or inventing values.
/// What: renders a placeholder with no scope value and asserts the marker.
/// Test: this test itself.
#[test]
fn missing_scalar_renders_honesty_marker() {
    let out = render("Value: {{unknown}}", &Scope::new());
    assert_eq!(out, format!("Value: {HONESTY_MARKER}"));
    assert!(!out.contains("{{"));
}

/// Why: `set_opt(None)` must leave the placeholder to the honesty marker.
/// What: sets an optional scalar to `None` and asserts the marker.
/// Test: this test itself.
#[test]
fn set_opt_none_falls_to_marker() {
    let mut scope = Scope::new();
    scope.set_opt("x", None::<String>);
    let out = render("{{x}}", &scope);
    assert_eq!(out, HONESTY_MARKER);
}

/// Why: per-application blocks must repeat once per provided scope.
/// What: pushes two child scopes and asserts both render in order.
/// Test: this test itself.
#[test]
fn block_repeats_per_scope() {
    let template = "<!-- BEGIN app -->[{{n}}]<!-- END app -->";
    let mut root = Scope::new();
    let mut a = Scope::new();
    a.set("n", "one");
    let mut b = Scope::new();
    b.set("n", "two");
    root.push_block("app", a);
    root.push_block("app", b);
    let out = render(template, &root);
    assert_eq!(out, "[one][two]");
}

/// Why: a block with no provided scope must render exactly once with markers
/// (never zero-and-silent, never invented data).
/// What: renders a block absent from the scope and asserts a single marker copy.
/// Test: this test itself.
#[test]
fn absent_block_renders_once_with_marker() {
    let template = "<!-- BEGIN app -->[{{n}}]<!-- END app -->";
    let out = render(template, &Scope::new());
    assert_eq!(out, format!("[{HONESTY_MARKER}]"));
}

/// Why: templates nest a findings list inside a per-application block.
/// What: renders an outer block containing an inner block, each with its own
/// scope, and asserts the nested repetition.
/// Test: this test itself.
#[test]
fn nested_blocks_expand() {
    let template =
        "<!-- BEGIN app -->A:{{app}}(<!-- BEGIN f -->{{f}};<!-- END f -->)<!-- END app -->";
    let mut root = Scope::new();
    let mut app = Scope::new();
    app.set("app", "web");
    let mut f1 = Scope::new();
    f1.set("f", "x");
    let mut f2 = Scope::new();
    f2.set("f", "y");
    app.push_block("f", f1);
    app.push_block("f", f2);
    root.push_block("app", app);
    let out = render(template, &root);
    assert_eq!(out, "A:web(x;y;)");
}

/// Why: `<!-- dataset: … -->` markers must survive rendering untouched so
/// downstream charting tools can lift datasets mechanically.
/// What: renders text containing a dataset comment and asserts it is preserved.
/// Test: this test itself.
#[test]
fn dataset_comment_preserved() {
    let template = "<!-- dataset: loc | chart: bar -->\n| {{app}} |";
    let mut scope = Scope::new();
    scope.set("app", "web");
    let out = render(template, &scope);
    assert!(out.contains("<!-- dataset: loc | chart: bar -->"));
    assert!(out.contains("| web |"));
}

// ─── strip_leading_comment (live-QA defect 3) ─────────────────────────────────

/// Why: a leading instructional comment — including one whose documentation
/// text embeds literal `{{field}}` / `<!-- BEGIN … --> … <!-- END … -->`
/// examples that would otherwise be mangled by the fill engine — must be
/// removed entirely so the generated output starts at the real content.
/// What: strips a header shaped exactly like the bundled templates' (with a
/// same-line BEGIN/END example nested as text) and asserts the output starts
/// at the title with no comment residue and no mangled example content.
/// Test: this test itself.
#[test]
fn strip_leading_comment_removes_header() {
    let template = "<!--\n  PLACEHOLDER SYNTAX\n  - `{{field_name}}` a scalar.\n  - `<!-- BEGIN per_application --> ... <!-- END per_application -->`\n    marks a repeatable block.\n-->\n\n# Title: {{name}}\n";
    let stripped = strip_leading_comment(template);
    assert!(stripped.starts_with("# Title:"));
    assert!(!stripped.contains("PLACEHOLDER SYNTAX"));
    assert!(!stripped.contains("field_name"));

    // Rendering the stripped template must not mangle-expand the header's fake
    // `per_application` example (it is gone before the fill engine ever runs).
    let mut root = Scope::new();
    root.set("name", "Acme");
    let mut app = Scope::new();
    app.set("app", "irrelevant");
    root.push_block("per_application", app);
    let out = render(stripped, &root);
    assert_eq!(out, "# Title: Acme\n");
}

/// Why: a template with no leading comment (e.g. a minimal custom override)
/// must render unchanged — stripping must never touch content that isn't a
/// leading instructional block.
/// What: asserts a template starting directly with a heading is byte-identical
/// after stripping.
/// Test: this test itself.
#[test]
fn strip_leading_comment_noop_without_header() {
    let template = "# Title: {{name}}\nBody text.\n";
    assert_eq!(strip_leading_comment(template), template);
}

/// Why: the guard must remove ONLY the first leading comment block — later,
/// independent comments (e.g. dataset markers) must survive untouched.
/// What: a header followed by real content that itself contains further
/// `<!-- … -->` comments; asserts those survive after stripping.
/// Test: this test itself.
#[test]
fn strip_leading_comment_only_removes_first_block() {
    let template =
        "<!--\n  header text\n-->\n\n# Title\n\n<!-- dataset: loc | chart: bar -->\n| {{app}} |\n";
    let stripped = strip_leading_comment(template);
    assert!(stripped.starts_with("# Title"));
    assert!(stripped.contains("<!-- dataset: loc | chart: bar -->"));
}

/// Why: a malformed template whose leading comment never closes must be left
/// untouched rather than guessing a cut point and corrupting output.
/// What: a leading `<!--` with no `-->` anywhere; asserts no change.
/// Test: this test itself.
#[test]
fn strip_leading_comment_unterminated_is_untouched() {
    let template = "<!-- never closes\n# Title\n";
    assert_eq!(strip_leading_comment(template), template);
}
