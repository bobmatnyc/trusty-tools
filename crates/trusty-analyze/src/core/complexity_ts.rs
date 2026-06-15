//! Tree-sitter-backed complexity computation for Rust and TypeScript/JS.
//!
//! Why: The text-heuristic `compute_complexity` in `complexity.rs` is fast
//! but wrong on common idioms — it counts the substring `if ` inside strings,
//! attributes, and identifiers, and it conflates `else` chains with branches.
//! Walking a real AST gives accurate cyclomatic / cognitive numbers and
//! produces line-accurate smells from `start_position()` / `end_position()`.
//!
//! What: For Rust and TypeScript (covering `.ts`/`.tsx`/`.js`), parse the
//! source with tree-sitter and walk it recursively. Cyclomatic counts each
//! branching node once. Cognitive multiplies each branching node by its
//! enclosing nesting depth + 1. If parsing fails or produces an empty tree,
//! callers should fall back to the text heuristic.
//!
//! Test: see the `tests` module — covers a single-branch function, a
//! no-branch function, deep nesting, and the smart dispatcher.

use crate::types::complexity::{CodeSmell, ComplexityGrade, ComplexityMetrics, SmellThresholds};
use tree_sitter::{Node, Parser};

/// Compute `ComplexityMetrics` for Rust source using tree-sitter AST.
///
/// Returns `None` if parsing fails so the caller can fall back to the
/// text-heuristic implementation in `complexity.rs`.
///
/// Why: tree-sitter AST walk gives accurate branch counts unlike the text
/// heuristic; separating from TypeScript keeps file sizes manageable.
/// What: parses Rust source, walks the AST accumulating cyclomatic/cognitive
/// counts, then runs `detect_smells_rust` with default thresholds.
/// Test: `compute_complexity_rust_single_branch` checks cyclomatic >= 2 for
/// an if/else; `long_function_smell_fires_for_long_fn` tests smell detection.
pub fn compute_complexity_rust(content: &str) -> Option<ComplexityMetrics> {
    compute_complexity_rust_with_thresholds(content, &SmellThresholds::default())
}

/// Variant of `compute_complexity_rust` that uses caller-supplied thresholds.
///
/// Why: lets `compute_complexity_for_with_thresholds` propagate runtime
/// thresholds into the Rust AST smell detector without changing the public
/// signature of `compute_complexity_rust`.
/// What: identical to `compute_complexity_rust` but passes `thresholds` to
/// `detect_smells_rust`.
/// Test: covered transitively by `compute_complexity_for_with_thresholds` tests.
pub fn compute_complexity_rust_with_thresholds(
    content: &str,
    thresholds: &SmellThresholds,
) -> Option<ComplexityMetrics> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let src = content.as_bytes();

    let mut state = WalkState::default();
    walk_rust(root, src, 0, &mut state);

    let cyclomatic = state.cyclomatic.saturating_add(1);
    let cognitive = state.cognitive;
    let grade = ComplexityGrade::from_cyclomatic(cyclomatic);
    let smells = detect_smells_rust(root, src, &state, thresholds);

    tracing::debug!(
        cyclomatic,
        cognitive,
        ?grade,
        max_nesting = state.max_nesting,
        "compute_complexity_rust"
    );

    Some(ComplexityMetrics {
        cyclomatic,
        cognitive,
        grade,
        smells,
    })
}

/// Compute `ComplexityMetrics` for TypeScript/JavaScript source using
/// tree-sitter AST. Uses the TSX grammar, which is a superset that also
/// parses plain `.ts` and `.js`.
///
/// Returns `None` if parsing fails so the caller can fall back to the
/// text-heuristic implementation.
///
/// Why: TSX grammar is a superset of TS and JS, eliminating the need for a
/// separate JavaScript grammar when computing complexity metrics.
/// What: parses source, walks the AST, detects smells with default thresholds.
/// Test: `compute_complexity_typescript_single_branch` checks cyclomatic >= 2.
pub fn compute_complexity_typescript(content: &str) -> Option<ComplexityMetrics> {
    compute_complexity_typescript_with_thresholds(content, &SmellThresholds::default())
}

/// Variant of `compute_complexity_typescript` that uses caller-supplied thresholds.
///
/// Why: propagates runtime thresholds into the TS/JS AST smell detector.
/// What: identical to `compute_complexity_typescript` but passes `thresholds`
/// to `detect_smells_ts`.
/// Test: covered transitively by `compute_complexity_for_with_thresholds` tests.
pub fn compute_complexity_typescript_with_thresholds(
    content: &str,
    thresholds: &SmellThresholds,
) -> Option<ComplexityMetrics> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
        .ok()?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let src = content.as_bytes();

    let mut state = WalkState::default();
    walk_ts(root, src, 0, &mut state);

    let cyclomatic = state.cyclomatic.saturating_add(1);
    let cognitive = state.cognitive;
    let grade = ComplexityGrade::from_cyclomatic(cyclomatic);
    let smells = detect_smells_ts(root, src, &state, thresholds);

    tracing::debug!(
        cyclomatic,
        cognitive,
        ?grade,
        max_nesting = state.max_nesting,
        "compute_complexity_typescript"
    );

    Some(ComplexityMetrics {
        cyclomatic,
        cognitive,
        grade,
        smells,
    })
}

/// Accumulator threaded through the recursive walk.
#[derive(Default)]
struct WalkState {
    cyclomatic: u32,
    cognitive: u32,
    max_nesting: u8,
}

impl WalkState {
    fn note_branch(&mut self, depth: u8) {
        self.cyclomatic = self.cyclomatic.saturating_add(1);
        let weight = (depth as u32).saturating_add(1);
        self.cognitive = self.cognitive.saturating_add(weight);
    }
}

/// Recursive walker for Rust ASTs. Counts branching nodes and tracks nesting
/// depth for cognitive complexity.
fn walk_rust(node: Node, src: &[u8], depth: u8, state: &mut WalkState) {
    state.max_nesting = state.max_nesting.max(depth);
    let kind = node.kind();
    let mut nest_inc: u8 = 0;

    match kind {
        "if_expression" => {
            state.note_branch(depth);
            nest_inc = 1;
        }
        // Only count `else if` as a branch — a plain `else` block is not.
        "else_clause" if has_child_kind(node, "if_expression") => {
            state.note_branch(depth);
        }
        "else_clause" => {}
        // First arm adds nothing; each subsequent arm is a branch.
        "match_arm" if !is_first_match_arm(node) => {
            state.note_branch(depth);
        }
        "match_arm" => {}
        "match_expression" => {
            nest_inc = 1;
        }
        "while_expression" | "loop_expression" | "for_expression" => {
            state.note_branch(depth);
            nest_inc = 1;
        }
        "binary_expression" if is_short_circuit_op(node, src) => {
            state.note_branch(depth);
        }
        "binary_expression" => {}
        "try_expression" => {
            // The `?` operator introduces an early-return branch.
            state.note_branch(depth);
        }
        "closure_expression" => {
            state.note_branch(depth);
            nest_inc = 1;
        }
        _ => {}
    }

    let new_depth = depth.saturating_add(nest_inc);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_rust(child, src, new_depth, state);
    }
}

/// Recursive walker for TypeScript / JavaScript ASTs.
fn walk_ts(node: Node, src: &[u8], depth: u8, state: &mut WalkState) {
    state.max_nesting = state.max_nesting.max(depth);
    let kind = node.kind();
    let mut nest_inc: u8 = 0;

    match kind {
        "if_statement" => {
            state.note_branch(depth);
            nest_inc = 1;
        }
        "else_clause" if has_child_kind(node, "if_statement") => {
            state.note_branch(depth);
        }
        "else_clause" => {}
        // Subsequent cases each add a branch; first case is already counted.
        "switch_case" if !is_first_switch_case(node) => {
            state.note_branch(depth);
        }
        "switch_case" => {}
        "switch_statement" => {
            nest_inc = 1;
        }
        "while_statement" | "do_statement" | "for_statement" | "for_in_statement"
        | "for_of_statement" => {
            state.note_branch(depth);
            nest_inc = 1;
        }
        "binary_expression" if is_short_circuit_op(node, src) => {
            state.note_branch(depth);
        }
        "binary_expression" => {}
        "ternary_expression" => {
            state.note_branch(depth);
        }
        "arrow_function" | "function_expression" => {
            state.note_branch(depth);
            nest_inc = 1;
        }
        "catch_clause" => {
            state.note_branch(depth);
            nest_inc = 1;
        }
        _ => {}
    }

    let new_depth = depth.saturating_add(nest_inc);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_ts(child, src, new_depth, state);
    }
}

/// True if `node` is a `binary_expression` whose operator is `&&` or `||`.
fn is_short_circuit_op(node: Node, src: &[u8]) -> bool {
    if let Some(op) = node.child_by_field_name("operator") {
        let txt = op.utf8_text(src).unwrap_or("");
        return txt == "&&" || txt == "||";
    }
    false
}

/// True if `node` is the first `match_arm` child of its parent.
fn is_first_match_arm(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        if child.kind() == "match_arm" {
            return child.id() == node.id();
        }
    }
    true
}

/// True if `node` is the first `switch_case` child of its parent.
fn is_first_switch_case(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return true;
    };
    let mut cursor = parent.walk();
    for child in parent.children(&mut cursor) {
        if child.kind() == "switch_case" {
            return child.id() == node.id();
        }
    }
    true
}

/// True if any direct child of `node` has the given kind.
fn has_child_kind(node: Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return true;
        }
    }
    false
}

/// AST-driven smell detection for Rust.
///
/// Why: uses AST node positions for accurate line counts and parameter counting
/// rather than text heuristics.
/// What: checks `LongFunction`, `DeepNesting`, `TooManyParams`, and
/// `MissingDocstring` against the caller-supplied `thresholds`.
/// Test: `long_function_smell_fires_for_long_fn` and
/// `missing_docstring_smell_for_undocumented_rust_fn` in the tests module.
fn detect_smells_rust(
    root: Node,
    src: &[u8],
    state: &WalkState,
    thresholds: &SmellThresholds,
) -> Vec<CodeSmell> {
    let mut smells = Vec::new();

    let fn_node = find_first_kind(root, "function_item");
    let lines = if let Some(n) = fn_node {
        n.end_position().row.saturating_sub(n.start_position().row) + 1
    } else {
        line_count(src)
    };
    if lines > thresholds.long_function_lines {
        smells.push(CodeSmell::LongFunction { lines });
    }

    if state.max_nesting > thresholds.deep_nesting_depth {
        smells.push(CodeSmell::DeepNesting {
            max_depth: state.max_nesting,
        });
    }

    if let Some(fn_n) = fn_node {
        let params = fn_n
            .child_by_field_name("parameters")
            .map(|p| count_named_children_kind(p, "parameter"))
            .unwrap_or(0);
        if params > thresholds.too_many_params {
            smells.push(CodeSmell::TooManyParams { count: params });
        }
        if !has_rust_doc(fn_n, src) {
            smells.push(CodeSmell::MissingDocstring);
        }
    } else if !contains_doc_marker(src) {
        smells.push(CodeSmell::MissingDocstring);
    }

    smells
}

/// AST-driven smell detection for TypeScript / JavaScript.
///
/// Why: uses AST node positions for accurate line and parameter counts.
/// What: checks `LongFunction`, `DeepNesting`, `TooManyParams`, and
/// `MissingDocstring` against the caller-supplied `thresholds`.
/// Test: `compute_complexity_typescript_single_branch` and related tests.
fn detect_smells_ts(
    root: Node,
    src: &[u8],
    state: &WalkState,
    thresholds: &SmellThresholds,
) -> Vec<CodeSmell> {
    let mut smells = Vec::new();

    let fn_node = find_first_kind(root, "function_declaration")
        .or_else(|| find_first_kind(root, "method_definition"))
        .or_else(|| find_first_kind(root, "arrow_function"));
    let lines = if let Some(n) = fn_node {
        n.end_position().row.saturating_sub(n.start_position().row) + 1
    } else {
        line_count(src)
    };
    if lines > thresholds.long_function_lines {
        smells.push(CodeSmell::LongFunction { lines });
    }

    if state.max_nesting > thresholds.deep_nesting_depth {
        smells.push(CodeSmell::DeepNesting {
            max_depth: state.max_nesting,
        });
    }

    if let Some(fn_n) = fn_node {
        let params = fn_n
            .child_by_field_name("parameters")
            .map(count_param_children)
            .unwrap_or(0);
        if params > thresholds.too_many_params {
            smells.push(CodeSmell::TooManyParams { count: params });
        }
        if !has_jsdoc(fn_n, src) {
            smells.push(CodeSmell::MissingDocstring);
        }
    } else if !contains_doc_marker(src) {
        smells.push(CodeSmell::MissingDocstring);
    }

    smells
}

/// Count parameter-shaped children of a `formal_parameters` node.
fn count_param_children(params: Node) -> usize {
    let mut count = 0;
    let mut cursor = params.walk();
    for child in params.children(&mut cursor) {
        match child.kind() {
            "required_parameter" | "optional_parameter" | "rest_pattern" | "identifier"
            | "assignment_pattern" | "object_pattern" | "array_pattern" => count += 1,
            _ => {}
        }
    }
    count
}

/// Count direct children of `node` whose kind matches `kind`.
fn count_named_children_kind(node: Node, kind: &str) -> usize {
    let mut count = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            count += 1;
        }
    }
    count
}

/// First descendant of `root` whose kind matches `kind`, or `None`.
fn find_first_kind<'a>(root: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if n.kind() == kind {
            return Some(n);
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// True if a `///` line_comment immediately precedes the function item.
fn has_rust_doc(fn_node: Node, src: &[u8]) -> bool {
    let mut sib = fn_node.prev_sibling();
    while let Some(s) = sib {
        match s.kind() {
            "line_comment" => {
                let txt = s.utf8_text(src).unwrap_or("");
                if txt.starts_with("///") || txt.starts_with("//!") {
                    return true;
                }
                sib = s.prev_sibling();
            }
            "block_comment" => {
                let txt = s.utf8_text(src).unwrap_or("");
                if txt.starts_with("/**") || txt.starts_with("/*!") {
                    return true;
                }
                sib = s.prev_sibling();
            }
            "attribute_item" | "inner_attribute_item" => {
                sib = s.prev_sibling();
            }
            _ => break,
        }
    }
    false
}

/// True if a `/** ... */` block_comment immediately precedes the function node.
fn has_jsdoc(fn_node: Node, src: &[u8]) -> bool {
    let mut sib = fn_node.prev_sibling();
    while let Some(s) = sib {
        if s.kind() == "comment" {
            let txt = s.utf8_text(src).unwrap_or("");
            if txt.starts_with("/**") {
                return true;
            }
            sib = s.prev_sibling();
        } else {
            break;
        }
    }
    false
}

/// Best-effort doc-marker check used when no function-shaped node is present.
fn contains_doc_marker(src: &[u8]) -> bool {
    let s = std::str::from_utf8(src).unwrap_or("");
    s.contains("///") || s.contains("/**") || s.contains("\"\"\"") || s.contains("'''")
}

/// Total line count of `src` (1-based; an empty buffer reports 1).
fn line_count(src: &[u8]) -> usize {
    let s = std::str::from_utf8(src).unwrap_or("");
    s.lines().count().max(1)
}

/// Tree-sitter grammar selector for the generic complexity walker.
///
/// Why: Phase 2 ships adapters for 14 languages, but only Rust and TS/JS had
/// AST-backed complexity. The remaining languages were stuck on the text
/// heuristic. A single generic walker that takes a per-language set of
/// decision-point node kinds gives every language an accurate count without a
/// bespoke walker each.
/// What: maps a language tag to its loaded `tree_sitter::Language` and the
/// list of node kinds that count as a branch (a decision point).
/// Test: `generic_complexity_counts_python_branches` and the per-language
/// cases in `tests`.
fn generic_language(lang: &str) -> Option<(tree_sitter::Language, &'static [&'static str])> {
    // Decision-point node kinds per grammar. `binary_expression` is included
    // where the grammar uses it for `&&`/`||`; the walker filters those by
    // operator text so a non-short-circuit `+` does not inflate the count.
    match lang {
        "python" => Some((
            tree_sitter_python::LANGUAGE.into(),
            &[
                "if_statement",
                "elif_clause",
                "for_statement",
                "while_statement",
                "except_clause",
                "with_statement",
                "boolean_operator",
                "conditional_expression",
            ],
        )),
        "java" => Some((
            tree_sitter_java::LANGUAGE.into(),
            &[
                "if_statement",
                "for_statement",
                "enhanced_for_statement",
                "while_statement",
                "do_statement",
                "catch_clause",
                "switch_label",
                "binary_expression",
                "ternary_expression",
            ],
        )),
        "kotlin" => Some((
            tree_sitter_kotlin_ng::LANGUAGE.into(),
            &[
                "if_expression",
                "for_statement",
                "while_statement",
                "do_while_statement",
                "catch_block",
                "when_entry",
                "conjunction_expression",
                "disjunction_expression",
            ],
        )),
        "go" => Some((
            tree_sitter_go::LANGUAGE.into(),
            &[
                "if_statement",
                "for_statement",
                "type_switch_statement",
                "expression_switch_statement",
                "select_statement",
                "expression_case",
                "type_case",
                "communication_case",
                "binary_expression",
            ],
        )),
        "c" => Some((
            tree_sitter_c::LANGUAGE.into(),
            &[
                "if_statement",
                "for_statement",
                "while_statement",
                "do_statement",
                "case_statement",
                "binary_expression",
                "conditional_expression",
            ],
        )),
        "cpp" => Some((
            tree_sitter_cpp::LANGUAGE.into(),
            &[
                "if_statement",
                "for_statement",
                "for_range_loop",
                "while_statement",
                "do_statement",
                "case_statement",
                "catch_clause",
                "binary_expression",
                "conditional_expression",
            ],
        )),
        "ruby" => Some((
            tree_sitter_ruby::LANGUAGE.into(),
            &[
                "if",
                "unless",
                "while",
                "until",
                "for",
                "rescue",
                "when",
                "elsif",
                "if_modifier",
                "unless_modifier",
                "while_modifier",
                "until_modifier",
                "binary",
            ],
        )),
        "php" => Some((
            tree_sitter_php::LANGUAGE_PHP.into(),
            &[
                "if_statement",
                "else_if_clause",
                "foreach_statement",
                "for_statement",
                "while_statement",
                "do_statement",
                "catch_clause",
                "match_expression",
                "case_statement",
                "binary_expression",
                "conditional_expression",
            ],
        )),
        "csharp" => Some((
            tree_sitter_c_sharp::LANGUAGE.into(),
            &[
                "if_statement",
                "for_statement",
                "for_each_statement",
                "while_statement",
                "do_statement",
                "catch_clause",
                "switch_section",
                "case_switch_label",
                "binary_expression",
                "conditional_expression",
            ],
        )),
        "scala" => Some((
            tree_sitter_scala::LANGUAGE.into(),
            &[
                "if_expression",
                "for_expression",
                "while_expression",
                "do_while_expression",
                "catch_clause",
                "case_clause",
                "infix_expression",
            ],
        )),
        "swift" => Some((
            tree_sitter_swift::LANGUAGE.into(),
            &[
                "if_statement",
                "for_statement",
                "while_statement",
                "repeat_while_statement",
                "guard_statement",
                "catch_block",
                "switch_entry",
                "ternary_expression",
            ],
        )),
        _ => None,
    }
}

/// Compute `ComplexityMetrics` for any supported language using a generic
/// tree-sitter walk driven by `generic_language`.
///
/// Returns `None` if the language is unknown or parsing fails, so the caller
/// can fall back to the text heuristic.
///
/// Why: Phase 2 ships adapters for 14 languages; rather than a bespoke walker
/// per language, one generic walker with per-language branch-kind lists covers
/// all of them without code duplication.
/// What: delegates to `compute_complexity_generic_with_thresholds` with
/// `SmellThresholds::default()`.
/// Test: `generic_complexity_counts_python_branches` in the tests module.
pub fn compute_complexity_generic(content: &str, lang: &str) -> Option<ComplexityMetrics> {
    compute_complexity_generic_with_thresholds(content, lang, &SmellThresholds::default())
}

/// Variant of `compute_complexity_generic` that uses caller-supplied thresholds.
///
/// Why: propagates runtime thresholds into the generic AST smell detector.
/// What: identical to `compute_complexity_generic` but passes `thresholds` to
/// `detect_smells_generic`.
/// Test: covered transitively by `compute_complexity_for_with_thresholds` tests.
pub fn compute_complexity_generic_with_thresholds(
    content: &str,
    lang: &str,
    thresholds: &SmellThresholds,
) -> Option<ComplexityMetrics> {
    let (language, branch_kinds) = generic_language(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(content, None)?;
    let root = tree.root_node();
    let src = content.as_bytes();

    let mut state = WalkState::default();
    walk_generic(root, src, 0, branch_kinds, &mut state);

    let cyclomatic = state.cyclomatic.saturating_add(1);
    let cognitive = state.cognitive;
    let grade = ComplexityGrade::from_cyclomatic(cyclomatic);
    let smells = detect_smells_generic(src, &state, thresholds);

    tracing::debug!(
        lang,
        cyclomatic,
        cognitive,
        ?grade,
        max_nesting = state.max_nesting,
        "compute_complexity_generic"
    );

    Some(ComplexityMetrics {
        cyclomatic,
        cognitive,
        grade,
        smells,
    })
}

/// Generic recursive AST walker. Counts each node whose kind appears in
/// `branch_kinds`, filtering `binary_expression`/`binary`/`infix_expression`
/// nodes to short-circuit operators so arithmetic does not inflate the count.
fn walk_generic(node: Node, src: &[u8], depth: u8, branch_kinds: &[&str], state: &mut WalkState) {
    state.max_nesting = state.max_nesting.max(depth);
    let kind = node.kind();
    let mut nest_inc: u8 = 0;

    if branch_kinds.contains(&kind) {
        let is_binary = matches!(
            kind,
            "binary_expression" | "binary" | "infix_expression" | "boolean_operator"
        );
        if !is_binary || is_logical_op(node, src) {
            state.note_branch(depth);
            if !is_binary {
                nest_inc = 1;
            }
        }
    }

    let new_depth = depth.saturating_add(nest_inc);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_generic(child, src, new_depth, branch_kinds, state);
    }
}

/// True if a binary/boolean node represents a logical short-circuit operator.
///
/// Handles grammars that expose an `operator` field as well as Python's
/// `boolean_operator` (whose operator is an unnamed `and`/`or` child).
fn is_logical_op(node: Node, src: &[u8]) -> bool {
    if let Some(op) = node.child_by_field_name("operator") {
        let txt = op.utf8_text(src).unwrap_or("");
        return matches!(txt, "&&" | "||" | "and" | "or");
    }
    // Fallback: scan immediate children for a logical operator token.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let txt = child.utf8_text(src).unwrap_or("");
        if matches!(txt, "&&" | "||" | "and" | "or") {
            return true;
        }
    }
    false
}

/// Generic AST-driven smell detection: derives `DeepNesting` from the walk
/// state and `LongFunction` / `MissingDocstring` from cheap text checks.
///
/// Why: all non-Rust/TS languages share the same smell rules; a single
/// function avoids per-language duplication.
/// What: checks line count, nesting depth, and doc marker presence against
/// caller-supplied `thresholds`.
/// Test: covered by `generic_complexity_counts_python_branches` and
/// `compute_complexity_for_with_thresholds` tests in this module.
fn detect_smells_generic(
    src: &[u8],
    state: &WalkState,
    thresholds: &SmellThresholds,
) -> Vec<CodeSmell> {
    let mut smells = Vec::new();

    let lines = line_count(src);
    if lines > thresholds.long_function_lines {
        smells.push(CodeSmell::LongFunction { lines });
    }
    if state.max_nesting > thresholds.deep_nesting_depth {
        smells.push(CodeSmell::DeepNesting {
            max_depth: state.max_nesting,
        });
    }
    if !contains_doc_marker(src) {
        smells.push(CodeSmell::MissingDocstring);
    }

    smells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_complexity_rust_single_branch() {
        let src = "fn foo(a: i32, b: i32) -> i32 { if a > b { a } else { b } }";
        let m = compute_complexity_rust(src).expect("parse should succeed");
        assert!(
            m.cyclomatic >= 2,
            "expected cyclomatic >= 2, got {}",
            m.cyclomatic
        );
        assert!(
            matches!(m.grade, ComplexityGrade::A | ComplexityGrade::B),
            "expected grade A or B, got {:?}",
            m.grade
        );
    }

    #[test]
    fn compute_complexity_rust_no_branches() {
        let src = "fn foo() -> i32 { 42 }";
        let m = compute_complexity_rust(src).expect("parse should succeed");
        assert_eq!(
            m.cyclomatic, 1,
            "expected cyclomatic == 1, got {}",
            m.cyclomatic
        );
        assert_eq!(m.grade, ComplexityGrade::A);
    }

    #[test]
    fn compute_complexity_rust_match_arms_count() {
        let src = r#"
fn classify(n: i32) -> &'static str {
    match n {
        0 => "zero",
        1 => "one",
        2 => "two",
        _ => "many",
    }
}
"#;
        let m = compute_complexity_rust(src).expect("parse should succeed");
        // 3 arms after the first → +3, plus base 1 = 4.
        assert!(
            m.cyclomatic >= 3,
            "expected cyclomatic >= 3, got {}",
            m.cyclomatic
        );
    }

    #[test]
    fn compute_complexity_rust_short_circuit_counts() {
        let src = r#"fn f(a: bool, b: bool, c: bool) -> bool { a && b || c }"#;
        let m = compute_complexity_rust(src).expect("parse should succeed");
        // base(1) + && (1) + || (1) = 3
        assert!(m.cyclomatic >= 3);
    }

    #[test]
    fn compute_complexity_typescript_single_branch() {
        let src = "function foo(a: number, b: number): number { return a > b ? a : b; }";
        let m = compute_complexity_typescript(src).expect("parse should succeed");
        assert!(m.cyclomatic >= 2);
    }

    #[test]
    fn compute_complexity_typescript_no_branches() {
        let src = "function foo(): number { return 42; }";
        let m = compute_complexity_typescript(src).expect("parse should succeed");
        assert_eq!(m.cyclomatic, 1);
        assert_eq!(m.grade, ComplexityGrade::A);
    }

    #[test]
    fn long_function_smell_fires_for_long_fn() {
        let mut body = String::from("/// doc\nfn big(a: i32) -> i32 {\n");
        for _ in 0..60 {
            body.push_str("    let _ = 1;\n");
        }
        body.push_str("    a\n}\n");
        let m = compute_complexity_rust(&body).expect("parse should succeed");
        assert!(
            m.smells
                .iter()
                .any(|s| matches!(s, CodeSmell::LongFunction { .. })),
            "expected LongFunction smell, got {:?}",
            m.smells
        );
    }

    #[test]
    fn missing_docstring_smell_for_undocumented_rust_fn() {
        let m = compute_complexity_rust("fn f() {}").expect("parse should succeed");
        assert!(m
            .smells
            .iter()
            .any(|s| matches!(s, CodeSmell::MissingDocstring)));
    }

    #[test]
    fn doc_comment_suppresses_missing_docstring() {
        let m = compute_complexity_rust("/// hi\nfn f() {}").expect("parse should succeed");
        assert!(!m
            .smells
            .iter()
            .any(|s| matches!(s, CodeSmell::MissingDocstring)));
    }

    #[test]
    fn generic_complexity_counts_python_branches() {
        let src = "def f(a, b):\n    if a > b and b > 0:\n        return a\n    for x in range(b):\n        pass\n    return b\n";
        let m = compute_complexity_generic(src, "python").expect("python should parse");
        // base(1) + if(1) + boolean_operator(1) + for(1) = 4
        assert!(
            m.cyclomatic >= 4,
            "expected cyclomatic >= 4, got {}",
            m.cyclomatic
        );
    }

    #[test]
    fn generic_complexity_no_branches_is_one() {
        let src = "def f():\n    return 42\n";
        let m = compute_complexity_generic(src, "python").expect("python should parse");
        assert_eq!(m.cyclomatic, 1);
        assert_eq!(m.grade, ComplexityGrade::A);
    }

    #[test]
    fn generic_complexity_handles_go_and_ruby() {
        let go = "func f(a int) int {\n\tif a > 0 {\n\t\treturn a\n\t}\n\treturn 0\n}\n";
        let m = compute_complexity_generic(go, "go").expect("go should parse");
        assert!(m.cyclomatic >= 2, "go cyclomatic {}", m.cyclomatic);

        let ruby = "def f(a)\n  if a > 0\n    a\n  else\n    0\n  end\nend\n";
        let m = compute_complexity_generic(ruby, "ruby").expect("ruby should parse");
        assert!(m.cyclomatic >= 2, "ruby cyclomatic {}", m.cyclomatic);
    }

    #[test]
    fn generic_complexity_unknown_language_is_none() {
        assert!(compute_complexity_generic("anything", "klingon").is_none());
    }

    #[test]
    fn generic_complexity_handles_java_branches() {
        let src = "class A {\n  int f(int a) {\n    if (a > 0 && a < 10) { return a; }\n    return 0;\n  }\n}\n";
        let m = compute_complexity_generic(src, "java").expect("java should parse");
        // base(1) + if(1) + && (1) = 3
        assert!(m.cyclomatic >= 3, "java cyclomatic {}", m.cyclomatic);
    }
}
