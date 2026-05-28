//! `add_import` and `validate_syntax` — side-effecting and read-only AST tools.

use std::path::Path;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::ast::editor::{
    add_import as do_add_import, apply_patch as do_apply_patch, validate_syntax,
};
use crate::ast::symbol::detect_language;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// `add_import` — language-aware import insertion. Applied immediately
/// (low-risk, side-effect free).
pub struct AddImportTool;

#[async_trait]
impl ToolExecutor for AddImportTool {
    fn name(&self) -> &str {
        "add_import"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "add_import",
                "description": "Add an import statement to a source file at the language-appropriate location (after the last existing import, or at the top of the file). Duplicate imports are skipped. Applied immediately to disk.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string"},
                        "import_stmt": {"type": "string", "description": "Full import line, e.g. `use std::fs;` or `import os`."}
                    },
                    "required": ["file", "import_stmt"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("add_import: missing 'file'");
        };
        let Some(import_stmt) = args.get("import_stmt").and_then(Value::as_str) else {
            return ToolResult::err("add_import: missing 'import_stmt'");
        };
        crate::events::emit(crate::events::Event::AstOperation {
            session_id: String::new(),
            op: "import".into(),
            detail: format!("{import_stmt} → {file}"),
        });
        match do_add_import(Path::new(file), import_stmt) {
            Ok(p) => {
                if p.original == p.modified {
                    return ToolResult::ok(
                        json!({
                            "file": file,
                            "import_stmt": import_stmt,
                            "applied": false,
                            "reason": "import already present"
                        })
                        .to_string(),
                    );
                }
                if let Err(e) = do_apply_patch(&p) {
                    return ToolResult::err(format!("add_import: failed to write: {e}"));
                }
                ToolResult::ok(
                    json!({
                        "file": file,
                        "import_stmt": import_stmt,
                        "applied": true,
                        "diff": p.diff
                    })
                    .to_string(),
                )
            }
            Err(e) => ToolResult::err(format!("add_import: {e}")),
        }
    }
}

/// `validate_syntax` — parse a source string and report any syntax errors.
pub struct ValidateSyntaxTool;

#[async_trait]
impl ToolExecutor for ValidateSyntaxTool {
    fn name(&self) -> &str {
        "validate_syntax"
    }
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "validate_syntax",
                "description": "Parse `source` using the language detected from `file`'s extension. Returns {valid, errors}. Useful for sanity-checking generated code before writing it.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "file": {"type": "string", "description": "Path used only for language detection by extension."},
                        "source": {"type": "string"}
                    },
                    "required": ["file", "source"],
                    "additionalProperties": false
                }
            }
        })
    }
    async fn execute(&self, args: Value) -> ToolResult {
        let Some(file) = args.get("file").and_then(Value::as_str) else {
            return ToolResult::err("validate_syntax: missing 'file'");
        };
        let Some(source) = args.get("source").and_then(Value::as_str) else {
            return ToolResult::err("validate_syntax: missing 'source'");
        };
        let Some((lang, _)) = detect_language(Path::new(file)) else {
            return ToolResult::err(format!("validate_syntax: unsupported extension on {file}"));
        };
        match validate_syntax(source, lang) {
            Ok(()) => {
                crate::events::emit(crate::events::Event::AstOperation {
                    session_id: String::new(),
                    op: "validate".into(),
                    detail: format!("{file} → OK"),
                });
                ToolResult::ok(json!({"valid": true, "errors": []}).to_string())
            }
            Err(e) => {
                crate::events::emit(crate::events::Event::AstOperation {
                    session_id: String::new(),
                    op: "validate".into(),
                    detail: format!("{file} → error: {e}"),
                });
                ToolResult::ok(json!({"valid": false, "errors": [e]}).to_string())
            }
        }
    }
}
