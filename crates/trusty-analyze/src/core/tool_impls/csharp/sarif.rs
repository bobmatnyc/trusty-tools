//! Roslyn SARIF parser and file-matching helpers.
//!
//! Why: SARIF parsing, percent-decoding, file-path matching, and severity
//! normalization are cohesive parsing concerns that are independently testable.
//! Keeping them in a dedicated submodule keeps `mod.rs` focused on the
//! `StaticTool` implementation and the compiler invocation.
//! What: exposes `parse_roslyn_sarif`, `roslyn_file_matches`, and related
//! helpers used by both `run()` (single-file filter) and `run_project()`
//! (multi-file filter).
//! Test: all helpers are covered by unit tests in this module.

use serde_json::Value;

use crate::core::tools::{Severity, ToolDiagnostic};

/// Parse a Roslyn/MSBuild SARIF 2.1 document into diagnostics.
///
/// Why: SARIF is the standard output format for Roslyn analyzers when using
/// `-p:ErrorLog`; this parser extracts the normalized findings.
/// What: reads `runs[0].results[]`, converts each result via
/// `roslyn_result_to_diag`, and collects the non-None results.
/// Test: `parse_roslyn_sarif_extracts_result` exercises this against a
/// captured real fixture string.
pub fn parse_roslyn_sarif(sarif: &str) -> Vec<ToolDiagnostic> {
    let root = match serde_json::from_str::<Value>(sarif.trim()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let results = match root
        .get("runs")
        .and_then(Value::as_array)
        .and_then(|r| r.first())
        .and_then(|run| run.get("results"))
        .and_then(Value::as_array)
    {
        Some(r) => r,
        None => return Vec::new(),
    };
    results.iter().filter_map(roslyn_result_to_diag).collect()
}

/// Convert one SARIF result object into a `ToolDiagnostic`.
///
/// Why: centralises the field-extraction logic so `parse_roslyn_sarif` stays
/// readable and the normalization is independently testable.
/// What: extracts `ruleId`, `level`, `message.text`, and the first
/// `physicalLocation` (uri + region); normalizes the file:// URI.
/// Test: tested indirectly through `parse_roslyn_sarif` in the unit tests.
fn roslyn_result_to_diag(result: &Value) -> Option<ToolDiagnostic> {
    let code = result
        .get("ruleId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let message = result
        .get("message")
        .and_then(|m| m.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let physical = result
        .get("locations")
        .and_then(Value::as_array)
        .and_then(|l| l.first())
        .and_then(|loc| loc.get("physicalLocation"))?;
    let raw_uri = physical
        .get("artifactLocation")
        .and_then(|a| a.get("uri"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let file = strip_file_scheme(raw_uri);
    let region = physical.get("region");
    let line = region
        .and_then(|r| r.get("startLine"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let col = region
        .and_then(|r| r.get("startColumn"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let severity = severity_from_str(result.get("level").and_then(Value::as_str).unwrap_or(""));
    Some(ToolDiagnostic {
        tool: "roslyn".into(),
        file,
        line,
        col,
        severity,
        code,
        message,
    })
}

/// Strip a leading `file://` scheme and percent-decode a SARIF URI.
///
/// Why: Roslyn emits absolute, percent-encoded `file:///Users/.../My%20Proj/
/// File.cs` URIs, but downstream code matches against plain filesystem paths
/// that contain literal spaces (real .NET estates have paths like
/// `RestAPI Test Harness/`). Without decoding, those never match.
/// What: removes a `file://` prefix if present (leaving the third `/` as the
/// absolute-path root), then decodes `%XX` escapes.
/// Test: `strip_file_scheme_decodes_and_removes_prefix` in unit tests.
fn strip_file_scheme(uri: &str) -> String {
    let without_scheme = uri.strip_prefix("file://").unwrap_or(uri);
    percent_decode(without_scheme)
}

/// Decode `%XX` percent-escapes in a path string.
///
/// Why: SARIF `artifactLocation.uri` percent-encodes characters such as the
/// space in `My%20Project`; matching against on-disk paths needs the decoded
/// form.
/// What: replaces each `%` followed by two hex digits with the decoded byte;
/// a malformed or truncated escape is copied through verbatim.
/// Test: covered by `strip_file_scheme_decodes_and_removes_prefix`.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Return true if `diag_file` (scheme-stripped, decoded) refers to `want`.
///
/// Why: Roslyn emits absolute paths while the caller may pass a relative or
/// absolute path; matching must be on a path-*component* suffix, not a raw
/// string suffix — otherwise `/abs/NotFoo.cs` would falsely match `Foo.cs`.
/// On Windows a SARIF `file:///C:\Users\...\Foo.cs` URI yields backslash
/// paths after `strip_file_scheme`, so all matching silently fails unless we
/// normalise both sides to forward slashes before comparing.
/// What: normalises `\` → `/` on both arguments, then checks direct equality
/// or `/`-anchored suffix on either side.
/// Test: `roslyn_file_matches_anchors_on_separator` and
/// `roslyn_file_matches_windows_backslash_paths` in unit tests.
pub fn roslyn_file_matches(diag_file: &str, want: &str) -> bool {
    let diag = diag_file.replace('\\', "/");
    let w = want.replace('\\', "/");
    diag == w || diag.ends_with(&format!("/{w}")) || w.ends_with(&format!("/{diag}"))
}

/// Map a SARIF level string to a `Severity`.
///
/// Why: SARIF uses free-form level strings; `Severity` is the normalized enum.
/// What: maps `"error"` → Error, `"warning"` → Warning,
/// `"note"`/`"info"` → Info, everything else → Hint.
/// Test: `severity_from_str_maps_correctly` in unit tests.
pub fn severity_from_str(s: &str) -> Severity {
    match s {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        "note" | "info" => Severity::Info,
        _ => Severity::Hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"version":"2.1.0","$schema":"http://json.schemastore.org/sarif-2.1.0","runs":[{"tool":{"driver":{"name":"Microsoft (R) Visual C# Compiler"}},"results":[{"ruleId":"CA1052","level":"warning","message":{"text":"Type 'Crypto' is a static holder type but is neither static nor NotInheritable"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///Users/maui/dve/experiments/hotstats/HotStatsGeoAPI/HotStats.Crypto/Crypto.cs"},"region":{"startLine":15,"startColumn":18,"endLine":15,"endColumn":24}}}]}]}]}"#;

    #[test]
    fn parse_roslyn_sarif_extracts_result() {
        let diags = parse_roslyn_sarif(FIXTURE);
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.code.as_deref(), Some("CA1052"));
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.line, 15);
        assert_eq!(d.col, 18);
        // URI normalization: must not start with "file://"
        assert!(!d.file.starts_with("file://"));
        assert!(d.file.ends_with("Crypto.cs"));
    }

    #[test]
    fn parse_roslyn_sarif_tolerates_garbage() {
        assert!(parse_roslyn_sarif("not json").is_empty());
        assert!(parse_roslyn_sarif("{}").is_empty());
    }

    #[test]
    fn parse_roslyn_sarif_tolerates_legacy_v1() {
        // SARIF v1 has a different shape — results live directly under the
        // top-level object, not nested under runs[].results.
        let v1 = r#"{"version":"1.0.0","results":[{"ruleId":"X","locations":[]}]}"#;
        // Must not panic; may return empty (no `runs` key).
        let diags = parse_roslyn_sarif(v1);
        assert!(diags.is_empty());
    }

    #[test]
    fn file_filtering_keeps_matching_drops_other() {
        // Two-result fixture: one for Crypto.cs, one for Other.cs.
        let two = r#"{"version":"2.1.0","runs":[{"results":[
            {"ruleId":"CA1","level":"warning","message":{"text":"m1"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///abs/Crypto.cs"},"region":{"startLine":1,"startColumn":1}}}]},
            {"ruleId":"CA2","level":"error","message":{"text":"m2"},"locations":[{"physicalLocation":{"artifactLocation":{"uri":"file:///abs/Other.cs"},"region":{"startLine":2,"startColumn":3}}}]}
        ]}]}"#;
        let all = parse_roslyn_sarif(two);
        assert_eq!(all.len(), 2);

        // Simulate the filter in `run`: keep only the Crypto.cs result.
        let want = "/abs/Crypto.cs";
        let kept: Vec<_> = all
            .iter()
            .filter(|d| roslyn_file_matches(&d.file, want))
            .collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].code.as_deref(), Some("CA1"));

        let want2 = "/abs/Other.cs";
        let kept2: Vec<_> = all
            .iter()
            .filter(|d| roslyn_file_matches(&d.file, want2))
            .collect();
        assert_eq!(kept2.len(), 1);
        assert_eq!(kept2[0].code.as_deref(), Some("CA2"));
    }

    #[test]
    fn severity_from_str_maps_correctly() {
        assert_eq!(severity_from_str("error"), Severity::Error);
        assert_eq!(severity_from_str("warning"), Severity::Warning);
        assert_eq!(severity_from_str("note"), Severity::Info);
        assert_eq!(severity_from_str("info"), Severity::Info);
        assert_eq!(severity_from_str("none"), Severity::Hint);
        assert_eq!(severity_from_str(""), Severity::Hint);
    }

    #[test]
    fn strip_file_scheme_decodes_and_removes_prefix() {
        assert_eq!(
            strip_file_scheme("file:///Users/x/Foo.cs"),
            "/Users/x/Foo.cs"
        );
        // Percent-encoded spaces (real .NET estates: "RestAPI Test Harness/").
        assert_eq!(
            strip_file_scheme("file:///Users/x/My%20Proj/Foo.cs"),
            "/Users/x/My Proj/Foo.cs"
        );
        assert_eq!(strip_file_scheme("/already/plain.cs"), "/already/plain.cs");
        assert_eq!(strip_file_scheme("relative/path.cs"), "relative/path.cs");
        // Malformed escape is copied through verbatim, never panics.
        assert_eq!(strip_file_scheme("a%2zb.cs"), "a%2zb.cs");
    }

    #[test]
    fn roslyn_file_matches_anchors_on_separator() {
        // Exact match
        assert!(roslyn_file_matches("/a/b/Foo.cs", "/a/b/Foo.cs"));
        // Suffix match (diag_file is absolute, want is just the filename)
        assert!(roslyn_file_matches("/a/b/Foo.cs", "Foo.cs"));
        // Suffix match (want is absolute, diag_file is a suffix)
        assert!(roslyn_file_matches("b/Foo.cs", "/a/b/Foo.cs"));
        // No match: different file
        assert!(!roslyn_file_matches("/a/b/Foo.cs", "/a/b/Bar.cs"));
        // No false positive: must anchor on a path separator
        assert!(!roslyn_file_matches("/abs/NotFoo.cs", "Foo.cs"));
    }

    #[test]
    fn roslyn_file_matches_windows_backslash_paths() {
        // Why: on Windows, a SARIF `file:///C:\Users\x\src\Foo.cs` URI
        // yields a backslash path after strip_file_scheme; without
        // normalisation the forward-slash suffix anchors never fire and
        // all C# results are silently dropped on the primary C# platform.
        // What: both sides are normalised to forward slashes before comparing.
        // Test: assert that Windows-style backslash paths match the expected
        // file name and path suffixes, and do not false-positive on unrelated files.

        // Windows absolute path with backslashes matches just the filename.
        assert!(roslyn_file_matches("C:\\Users\\x\\src\\Foo.cs", "Foo.cs"));
        // Windows absolute path matches a forward-slash relative segment.
        assert!(roslyn_file_matches(
            "C:\\Users\\x\\src\\Foo.cs",
            "src/Foo.cs"
        ));
        // Mixed: want has backslashes, diag_file has forward slashes.
        assert!(roslyn_file_matches("/abs/src/Foo.cs", "src\\Foo.cs"));
        // No false positive across different filenames.
        assert!(!roslyn_file_matches("C:\\Users\\x\\src\\Foo.cs", "Bar.cs"));
        // No false positive on a name that is a suffix of another (NotFoo.cs vs Foo.cs).
        assert!(!roslyn_file_matches("C:\\Users\\x\\NotFoo.cs", "Foo.cs"));
    }
}
