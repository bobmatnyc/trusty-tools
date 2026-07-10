//! Tests for the deterministic fill engine.
//!
//! Why: the fill engine implements the honesty rule and block repetition that
//! the whole report layer depends on; its scalar substitution, missing-value
//! marker, single/repeated/nested block behaviour, and dataset-comment
//! preservation must be pinned.
//! What: builds scopes directly and asserts on rendered output.
//! Test: included as `#[cfg(test)] mod tests` from `fill.rs`.

use super::{HONESTY_MARKER, Scope, render};

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
