//! Tree-sitter AST walking helpers shared by every analysis tool (#373).
//!
//! Why: Every analysis tool needs the same primitives — count branching
//! nodes, measure nesting depth, count parameters, find function bodies.
//! Centralising them here avoids per-tool duplication and keeps the
//! language-specific node-kind tables in one place.
//! What: `compute_function_complexity`, `count_parameters_for_function`,
//! plus internal helpers that walk a tree with explicit depth tracking.
//! Test: `complexity_simple_branchless` and `complexity_with_branches`
//! exercise both axes (cyclomatic + cognitive).

use tree_sitter::{Node, Parser};

/// Computed per-function complexity metrics.
#[derive(Debug, Clone, Copy, Default)]
pub struct FuncComplexity {
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub max_nesting: u32,
}

/// Branch-node kinds that contribute +1 cyclomatic complexity per occurrence.
///
/// Why: Different tree-sitter grammars use different node-kind names for
/// "if" / "for" / etc.; one switch per language keeps them legible.
/// What: Returns a `&'static [&'static str]` of node kinds.
fn branching_kinds(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &[
            "if_expression",
            "else_clause",
            "match_arm",
            "while_expression",
            "for_expression",
            "loop_expression",
            "while_let_expression",
            "for_let_expression",
            "try_expression", // ? operator
        ],
        "python" => &[
            "if_statement",
            "elif_clause",
            "for_statement",
            "while_statement",
            "except_clause",
            "conditional_expression",
            "boolean_operator",
        ],
        "javascript" | "typescript" => &[
            "if_statement",
            "else_clause",
            "switch_case",
            "for_statement",
            "for_in_statement",
            "for_of_statement",
            "while_statement",
            "do_statement",
            "ternary_expression",
            "catch_clause",
        ],
        "go" => &[
            "if_statement",
            "for_statement",
            "expression_case",
            "default_case",
            "type_case",
            "select_statement",
        ],
        "java" => &[
            "if_statement",
            "for_statement",
            "enhanced_for_statement",
            "while_statement",
            "do_statement",
            "switch_label",
            "ternary_expression",
            "catch_clause",
        ],
        _ => &[],
    }
}

/// Node kinds that increase nesting depth (control-flow blocks).
fn nesting_kinds(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &[
            "if_expression",
            "match_expression",
            "while_expression",
            "for_expression",
            "loop_expression",
        ],
        "python" => &[
            "if_statement",
            "for_statement",
            "while_statement",
            "try_statement",
            "with_statement",
        ],
        "javascript" | "typescript" => &[
            "if_statement",
            "for_statement",
            "for_in_statement",
            "for_of_statement",
            "while_statement",
            "do_statement",
            "try_statement",
            "switch_statement",
        ],
        "go" => &[
            "if_statement",
            "for_statement",
            "switch_statement",
            "select_statement",
        ],
        "java" => &[
            "if_statement",
            "for_statement",
            "enhanced_for_statement",
            "while_statement",
            "do_statement",
            "try_statement",
            "switch_statement",
        ],
        _ => &[],
    }
}

/// Node kinds that increment cognitive complexity (with depth bonus).
///
/// Why: Cognitive complexity differs from cyclomatic by adding a depth
/// bonus at each control-flow increment — `match_arm` (Rust) doesn't
/// trigger this, only the surrounding `match_expression` does, etc.
fn cognitive_increment_kinds(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &[
            "if_expression",
            "match_expression",
            "while_expression",
            "for_expression",
            "loop_expression",
        ],
        "python" => &[
            "if_statement",
            "for_statement",
            "while_statement",
            "except_clause",
            "conditional_expression",
        ],
        "javascript" | "typescript" => &[
            "if_statement",
            "for_statement",
            "for_in_statement",
            "for_of_statement",
            "while_statement",
            "do_statement",
            "ternary_expression",
            "catch_clause",
        ],
        "go" => &[
            "if_statement",
            "for_statement",
            "switch_statement",
            "select_statement",
        ],
        "java" => &[
            "if_statement",
            "for_statement",
            "enhanced_for_statement",
            "while_statement",
            "do_statement",
            "ternary_expression",
            "catch_clause",
        ],
        _ => &[],
    }
}

/// Get the tree-sitter `Language` for a language tag.
fn ts_language_for(tag: &str) -> Option<tree_sitter::Language> {
    Some(match tag {
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "go" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        _ => return None,
    })
}

/// Compute cyclomatic + cognitive + max-nesting for a function body source.
///
/// Why: Re-parses the function-body slice with the language's tree-sitter
/// grammar and walks the tree counting branching / nesting nodes. Reusing
/// the symbol's pre-extracted source (rather than the whole file) keeps
/// the count localised to the function.
/// What: Returns `FuncComplexity` with cyclomatic seeded at 1 (one path
/// always exists). Returns the default (`cyclomatic = 1`) if parsing fails
/// — a graceful degradation for unsupported language tags.
/// Test: `complexity_simple_branchless`, `complexity_with_branches`.
pub fn compute_function_complexity(source: &str, language: &str) -> FuncComplexity {
    let mut out = FuncComplexity {
        cyclomatic: 1,
        cognitive: 0,
        max_nesting: 0,
    };
    let Some(ts_lang) = ts_language_for(language) else {
        return out;
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return out;
    }
    let Some(tree) = parser.parse(source, None) else {
        return out;
    };

    let branch = branching_kinds(language);
    let nesting = nesting_kinds(language);
    let cog_inc = cognitive_increment_kinds(language);
    let bytes = source.as_bytes();

    walk(
        tree.root_node(),
        0,
        &mut out,
        branch,
        nesting,
        cog_inc,
        language,
    );

    let _ = bytes; // bytes were used historically to extract operator text; not needed now.
    out
}

#[allow(clippy::too_many_arguments)]
fn walk(
    node: Node,
    depth: u32,
    out: &mut FuncComplexity,
    branch: &[&str],
    nesting: &[&str],
    cog_inc: &[&str],
    language: &str,
) {
    let kind = node.kind();

    // Cyclomatic: +1 per branching node.
    if branch.contains(&kind) {
        out.cyclomatic = out.cyclomatic.saturating_add(1);
    }

    // Short-circuit && / ||: these add a path each.
    if (language == "rust" || language == "javascript" || language == "typescript")
        && kind == "binary_expression"
    {
        // peek operator child
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i as u32) {
                let ck = child.kind();
                if ck == "&&" || ck == "||" {
                    out.cyclomatic = out.cyclomatic.saturating_add(1);
                    break;
                }
            }
        }
    }

    // Cognitive: at increment kinds, add (1 + depth).
    let mut new_depth = depth;
    if cog_inc.contains(&kind) {
        out.cognitive = out.cognitive.saturating_add(1 + depth);
    }
    if nesting.contains(&kind) {
        new_depth = depth.saturating_add(1);
        if new_depth > out.max_nesting {
            out.max_nesting = new_depth;
        }
    }

    // Recurse — drop args through unchanged.
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, new_depth, out, branch, nesting, cog_inc, language);
    }
}

/// Count the parameters declared by a function symbol.
///
/// Why: `LongParameterList` smell needs an accurate parameter count from
/// the function's signature, not its body.
/// What: Re-parses the source, locates the first `parameters` /
/// `parameter_list` / `formal_parameters` node and counts its named
/// children that look like parameters (skips punctuation tokens).
/// Test: `count_parameters_simple`.
pub fn count_parameters_for_function(source: &str, language: &str) -> u32 {
    let Some(ts_lang) = ts_language_for(language) else {
        return 0;
    };
    let mut parser = Parser::new();
    if parser.set_language(&ts_lang).is_err() {
        return 0;
    }
    let Some(tree) = parser.parse(source, None) else {
        return 0;
    };

    let param_kinds: &[&str] = match language {
        "rust" => &["parameters"],
        "python" => &["parameters"],
        "javascript" | "typescript" => &["formal_parameters"],
        "go" => &["parameter_list"],
        "java" => &["formal_parameters"],
        "c" | "cpp" => &["parameter_list"],
        _ => &[],
    };

    // BFS for first matching node.
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if param_kinds.contains(&node.kind()) {
            // Count named children; skip self-receivers like Rust's `self_parameter`.
            let mut count = 0u32;
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                let k = child.kind();
                // Filter out the comma punctuation and similar — named_children
                // already excludes anonymous tokens.
                if k.contains("parameter")
                    || k == "identifier"
                    || k == "typed_parameter"
                    || k == "default_parameter"
                    || k == "typed_default_parameter"
                    || k == "list_splat_pattern"
                    || k == "dictionary_splat_pattern"
                {
                    count += 1;
                }
            }
            return count;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complexity_simple_branchless() {
        let src = "fn add(a: i32, b: i32) -> i32 { a + b }";
        let c = compute_function_complexity(src, "rust");
        assert_eq!(c.cyclomatic, 1);
        assert_eq!(c.cognitive, 0);
        assert_eq!(c.max_nesting, 0);
    }

    #[test]
    fn complexity_with_branches() {
        let src = r#"
fn pick(x: i32, y: i32) -> i32 {
    if x > 0 {
        if y > 0 {
            x + y
        } else {
            x - y
        }
    } else {
        for i in 0..10 {
            if i == 5 { return i; }
        }
        0
    }
}
"#;
        let c = compute_function_complexity(src, "rust");
        assert!(c.cyclomatic > 3, "cyclomatic = {}", c.cyclomatic);
        assert!(c.cognitive > 0);
        assert!(c.max_nesting >= 2);
    }

    #[test]
    fn count_parameters_simple() {
        let n = count_parameters_for_function("fn f(a: i32, b: i32, c: i32) {}", "rust");
        assert_eq!(n, 3);
    }

    #[test]
    fn count_parameters_python() {
        let n = count_parameters_for_function("def f(a, b, c=1, *args, **kwargs): pass", "python");
        assert_eq!(n, 5);
    }
}
