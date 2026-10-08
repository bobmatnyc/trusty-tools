//! Tests for finding changed symbols in a diff (#9196).
//!
//! Why: AC1's shortcut to fail is a query that is not symbol-derived; these
//! pin which names a hunk yields, per language and per edit shape.
//! What: declared, header-named and context-scanned symbols, the `impl`
//! qualification, the unresolved count, and the priority order.
//! Test: included as `#[cfg(test)] mod tests` from `symbols.rs`.

use super::*;

fn names(found: &Found) -> Vec<(&str, Kind)> {
    found
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.kind))
        .collect()
}

/// Critic finding 1: a method is keyed by its `impl`'s self-type text
/// verbatim, as trusty-search's `rust_impl_type_name` keys it
/// (`FilesCall<'a>::new`, never `FilesCall::new`).
#[test]
fn a_rust_method_takes_its_impl_type() {
    let patch = "@@ -10,6 +10,9 @@ impl<'a> Ledger<'a> {\n \
                 \x20   /// Doc.\n\
                 +    pub(crate) async fn finish(&mut self) {\n\
                 +        todo!()\n\
                 +    }\n";
    let found = changed_symbols("src/ledger.rs", patch);
    assert_eq!(names(&found), [("Ledger<'a>::finish", Kind::Declared)]);
    assert_eq!(found.symbols[0].id(), "src/ledger.rs::Ledger<'a>::finish");

    for (impl_line, want) in [
        ("impl Display for Report {", "Report::fmt"),
        ("impl<T: Clone> Stack<T, Vec<T>> {", "Stack<T, Vec<T>>::fmt"),
        (
            "impl<'a, T> fmt::Debug for Wrapper<'a, T> where T: Debug {",
            "Wrapper<'a, T>::fmt",
        ),
        ("impl crate::store::Store {", "crate::store::Store::fmt"),
        (
            "impl<T> From<T> for Box<dyn Fn(T) -> u8> {",
            "Box<dyn Fn(T) -> u8>::fmt",
        ),
    ] {
        let patch = format!("@@ -1,3 +1,4 @@\n {impl_line}\n+    fn fmt(&self) {{}}\n");
        let found = changed_symbols("src/r.rs", &patch);
        assert_eq!(names(&found), [(want, Kind::Declared)], "{impl_line}");
    }
}

/// A type the key cannot be read from is left out of the name, never cut to
/// a guess: the method is queried unqualified.
#[test]
fn an_odd_impl_type_is_not_used() {
    for impl_line in [
        "impl dyn Shape {",
        "impl<'a> Trait for &'a mut Thing {",
        "impl Foo`## PR Description {",
        "impl Unbalanced<T {",
    ] {
        let patch = format!("@@ -1,3 +1,4 @@\n {impl_line}\n+    fn fmt(&self) {{}}\n");
        let found = changed_symbols("src/r.rs", &patch);
        assert_eq!(names(&found), [("fmt", Kind::Declared)], "{impl_line}");
    }
}

/// Critic finding 2, from the critic's reproduction (`git diff` on a
/// `lib.rs` whose `pub fn helper` follows `impl Foo { }`): git names the
/// closed `impl Foo {` in the `@@` suffix, and the free fn is not a method.
#[test]
fn a_free_fn_after_a_closed_impl_is_not_a_method() {
    let real = "@@ -8,6 +8,6 @@ impl Foo {\n     }\n }\n \n\
                -pub fn helper() -> u32 {\n+pub fn helper() -> u64 {\n     2\n }\n";
    let found = changed_symbols("lib.rs", real);
    assert_eq!(names(&found), [("helper", Kind::Declared)]);

    // Indented in a module: the impl closed at its own indentation.
    let nested = "@@ -3,7 +3,7 @@ mod m {\n     impl Foo {\n         fn a() {}\n     }\n \n\
                  -    fn helper() -> u32 { 1 }\n+    fn helper() -> u64 { 1 }\n";
    let found = changed_symbols("src/m.rs", nested);
    assert_eq!(names(&found), [("helper", Kind::Declared)]);

    // Still open: an inner `}` deeper than the impl does not close it.
    let open = "@@ -3,7 +3,8 @@ impl Foo {\n     fn a() {\n     }\n+    fn b() {}\n";
    let found = changed_symbols("src/o.rs", open);
    assert_eq!(names(&found), [("Foo::b", Kind::Declared)]);
}

#[test]
fn each_language_declaration_is_found() {
    for (path, line, want) in [
        ("src/a.rs", "+pub fn total(a: u64) -> u64 {", "total"),
        ("src/a.rs", "-async unsafe fn gone() {}", "gone"),
        ("app/a.py", "+    async def fetch(self, url):", "fetch"),
        ("app/a.py", "+def handler(event):", "handler"),
        (
            "web/a.ts",
            "+export async function load(id: string) {",
            "load",
        ),
        ("web/a.js", "+function* walk(tree) {", "walk"),
        ("cmd/a.go", "+func Serve(addr string) error {", "Serve"),
        ("cmd/a.go", "+func (s *Server) Close() error {", "Close"),
    ] {
        let patch = format!("@@ -1,1 +1,2 @@\n context\n{line}\n");
        let found = changed_symbols(path, &patch);
        assert_eq!(names(&found), [(want, Kind::Declared)], "{path}: {line}");
    }
}

#[test]
fn a_body_edit_takes_the_hunk_header() {
    let patch = "@@ -20,7 +20,7 @@ pub fn total(amounts: &[u64]) -> u64 {\n \
                 \x20   let a = 1;\n\
                 -    a + 1\n\
                 +    a.checked_add(1).unwrap_or(0)\n";
    let found = changed_symbols("src/billing.rs", patch);
    assert_eq!(names(&found), [("total", Kind::Touched)]);
    assert_eq!(found.symbols[0].diff_lines, 2);
}

#[test]
fn a_body_edit_scans_back_through_context() {
    let patch = "@@ -40,7 +40,7 @@ impl Store {\n \
                 \x20   fn save(&self) {\n\
                 \x20       let x = 1;\n\
                 -        write(x);\n\
                 +        write_all(x);\n";
    let found = changed_symbols("src/store.rs", patch);
    assert_eq!(names(&found), [("Store::save", Kind::Touched)]);
}

#[test]
fn a_hunk_with_no_symbol_is_counted() {
    let patch = "@@ -1,2 +1,2 @@\n use std::fmt;\n-const A: u8 = 1;\n+const A: u8 = 2;\n\
                 @@ -9,1 +9,2 @@\n x\n+fn added() {}\n";
    let found = changed_symbols("src/a.rs", patch);
    assert_eq!(found.unresolved, 1);
    assert_eq!(names(&found), [("added", Kind::Declared)]);
}

/// A name declared in one hunk and edited in another is one symbol, kept as
/// `Declared`, with both hunks' lines.
#[test]
fn one_symbol_per_name_declared_wins() {
    let patch = "@@ -1,1 +1,1 @@ fn total() {\n-    1\n+    2\n\
                 @@ -9,1 +9,2 @@\n x\n+fn total() {}\n";
    let found = changed_symbols("src/a.rs", patch);
    assert_eq!(names(&found), [("total", Kind::Declared)]);
    assert_eq!(found.symbols[0].diff_lines, 3);
}

/// Words that only look like declarations are not symbols, and a name is
/// only ever an identifier.
#[test]
fn non_declarations_and_odd_names_are_not_symbols() {
    for line in [
        "+    let f = fn_ptr;",
        "+// fn commented() {}",
        "+    call(fn_name);",
        "+fn 9bad() {}",
        "+fn `x`() {}",
    ] {
        let patch = format!("@@ -1,1 +1,2 @@\n x\n{line}\n");
        assert!(
            changed_symbols("src/a.rs", &patch).symbols.is_empty(),
            "{line}"
        );
    }
}

#[test]
fn priority_is_declared_then_size_then_path() {
    let s = |path: &str, name: &str, kind, diff_lines| ChangedSymbol {
        path: path.to_string(),
        name: name.to_string(),
        kind,
        diff_lines,
    };
    let input = vec![
        s("b.rs", "touched_big", Kind::Touched, 50),
        s("b.rs", "small", Kind::Declared, 1),
        s("a.rs", "big", Kind::Declared, 9),
        s("a.rs", "also_small", Kind::Declared, 1),
    ];
    let want = ["big", "also_small", "small", "touched_big"];
    let got: Vec<String> = priority(input.clone())
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(got, want);
    let reversed: Vec<String> = priority(input.into_iter().rev().collect())
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(reversed, want, "order is independent of input order");
}
