//! What KIND of region a measured chunk is (#6177).
//!
//! Why: a hotspot's remediation only makes sense once you know what was
//! measured. trusty-review already relabels a Rust region with no function name
//! as an `impl` block, because "extract the body of this function" is not an
//! action a reader can take against one. Python has the same shape and no way to
//! tell it apart: a `class Foo:` body and a `def foo():` body both arrive as a
//! chunk, and a class body with no function name looked exactly like a nameless
//! function. The lap-4 grader on the trusty-audit self-audit report raised it.
//!
//! What: [`RegionKind`] and [`classify`], which read the chunk's own opening
//! definition line. Python only, deliberately: every other language returns
//! `None` and leaves its consumers byte-identical.
//!
//! Scope: this is a lexical read of the first definition line, not a parse. A
//! chunk whose content does not open on a `class` or `def` line yields `None` —
//! the honest answer, and the one that keeps the field from asserting a shape it
//! did not see.
//!
//! Test: see `mod tests` below.

use serde::{Deserialize, Serialize};

/// What a measured region is, when the analyzer can tell (#6177).
///
/// Why: the consumer's question is whether the remediation should talk about a
/// function body or about a container's members. Two variants answer it; a
/// region the classifier cannot place is `None` rather than a third "unknown"
/// variant nobody can act on.
/// What: `ClassBody` for a Python `class` block, `Function` for a `def` or
/// `async def`.
/// Test: `classifies_a_python_class_body`, `classifies_a_python_function`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionKind {
    /// The region is a class body — its members are what would be split out.
    ClassBody,
    /// The region is one function or method body.
    Function,
}

impl RegionKind {
    /// The wire string, matching the `serde` rename.
    pub fn as_str(self) -> &'static str {
        match self {
            RegionKind::ClassBody => "class_body",
            RegionKind::Function => "function",
        }
    }
}

/// Classify one measured region from its file path and content.
///
/// Why/What: see the module doc. Reads the first line that is neither blank, a
/// comment, nor a decorator — the definition line a Python chunk opens on — and
/// answers from its keyword. `None` for a non-Python file, and for a Python
/// chunk that opens on anything else (module-level code, a continued
/// expression, a chunk that starts mid-body).
/// Test: `classifies_a_python_class_body`, `classifies_a_python_function`,
/// `skips_decorators_and_comments`, `a_non_python_file_is_unclassified`,
/// `module_level_code_is_unclassified`.
pub fn classify(file: &str, content: &str) -> Option<RegionKind> {
    if !is_python(file) {
        return None;
    }
    let line = content
        .lines()
        .map(str::trim_start)
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('@'))?;
    if starts_with_keyword(line, "class") {
        return Some(RegionKind::ClassBody);
    }
    if starts_with_keyword(line, "def") || starts_with_keyword(line, "async def") {
        return Some(RegionKind::Function);
    }
    None
}

/// True when `path` names a Python source file.
fn is_python(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".py") || lower.ends_with(".pyi")
}

/// True when `line` opens with `keyword` followed by a word boundary.
///
/// `classy_thing()` must not read as a `class`, so the character after the
/// keyword has to be whitespace — a definition always has a name after it.
fn starts_with_keyword(line: &str, keyword: &str) -> bool {
    line.strip_prefix(keyword)
        .is_some_and(|rest| rest.starts_with(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_python_class_body() {
        let k = classify(
            "app/models.py",
            "class Order:\n    def total(self):\n        return 0\n",
        );
        assert_eq!(k, Some(RegionKind::ClassBody));
    }

    #[test]
    fn classifies_a_python_function() {
        assert_eq!(
            classify("app/util.py", "def compute(a, b):\n    return a + b\n"),
            Some(RegionKind::Function)
        );
        assert_eq!(
            classify(
                "app/util.py",
                "async def fetch(url):\n    return await get(url)\n"
            ),
            Some(RegionKind::Function)
        );
    }

    /// A decorated class is still a class. Decorators and comments sit above the
    /// definition line, so the classifier reads past them.
    #[test]
    fn skips_decorators_and_comments() {
        let content = "# the order aggregate\n@dataclass\n@final\nclass Order:\n    pass\n";
        assert_eq!(
            classify("app/models.py", content),
            Some(RegionKind::ClassBody)
        );
    }

    /// Every other language keeps its pre-#6177 behaviour: no field, no change.
    #[test]
    fn a_non_python_file_is_unclassified() {
        assert_eq!(
            classify("src/lib.rs", "impl Store {\n    fn get() {}\n}\n"),
            None
        );
        assert_eq!(classify("app/models.ts", "class Order {}\n"), None);
    }

    /// A chunk that opens on something other than a definition states nothing.
    #[test]
    fn module_level_code_is_unclassified() {
        assert_eq!(
            classify("app/main.py", "ORDERS = []\nprint(ORDERS)\n"),
            None
        );
        assert_eq!(classify("app/main.py", "    self.total += 1\n"), None);
        assert_eq!(classify("app/main.py", ""), None);
    }

    /// A name that merely starts with a keyword is not a definition.
    #[test]
    fn a_keyword_prefix_is_not_a_definition() {
        assert_eq!(classify("app/main.py", "classify(x)\n"), None);
        assert_eq!(classify("app/main.py", "default = 1\n"), None);
    }

    #[test]
    fn the_wire_string_matches_the_serde_rename() {
        assert_eq!(RegionKind::ClassBody.as_str(), "class_body");
        assert_eq!(RegionKind::Function.as_str(), "function");
        assert_eq!(
            serde_json::to_string(&RegionKind::ClassBody).unwrap(),
            "\"class_body\""
        );
    }
}
