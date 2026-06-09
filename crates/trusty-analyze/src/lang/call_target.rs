//! Shared helpers for building and recognising unresolved call-edge target ids.
//!
//! Why: The former per-adapter pattern `format!("{lang}:{kind}:{caller_file}:{bare_callee}")`
//! produced targets that could never match node ids (wrong file, missing class
//! qualifier). By centralising the target-id format here every adapter emits
//! the same shape and the linker's `resolve_calls` pass has a single sentinel
//! to key on.
//!
//! What: A call-edge target is written as `{lang}:{kind}::{callee}` (two
//! consecutive colons indicate an unresolved file).  The linker later replaces
//! these with real node ids using a name-keyed lookup.  Genuinely external
//! targets (callee not found in the node set) keep the sentinel form so
//! consumers can distinguish "internal but unresolved" from "external".
//!
//! Test: `unresolved_target_roundtrips` verifies that `build_call_target` and
//! `parse_call_target` are inverses.  Resolution behaviour is tested in
//! `core::linker`.

/// Build the unresolved call-edge target id for a callee.
///
/// Why: Every adapter must emit the *same* target format so the linker can
/// find and resolve them in one pass.  Having a single function prevents the
/// two id schemes (node id vs edge-target id) from drifting independently.
///
/// What: Produces `"{lang}:{kind}::{callee}"` — the double-colon marks the
/// file component as unresolved.  The kind string should match the `{kind:?}`
/// display used when minting node ids (e.g. `"Function"` or `"Method"`).
///
/// Test: See `unresolved_target_roundtrips` below; also exercised by
/// `linker::tests::resolve_calls_*` tests.
#[inline]
pub fn build_call_target(lang: &str, kind: &str, callee: &str) -> String {
    format!("{lang}:{kind}::{callee}")
}

/// A parsed unresolved call-edge target.
#[derive(Debug, PartialEq)]
pub struct UnresolvedTarget<'a> {
    pub lang: &'a str,
    pub kind: &'a str,
    pub callee: &'a str,
}

/// Try to parse an unresolved target produced by `build_call_target`.
///
/// Why: The linker needs to detect which edge targets are synthetic
/// (unresolved) vs already-canonical node ids.
///
/// What: Returns `Some(UnresolvedTarget)` if `s` has the form
/// `"{lang}:{kind}::{callee}"` (three colon-delimited components where the
/// second segment is empty), `None` otherwise.
///
/// Test: `unresolved_target_roundtrips` covers the happy path; non-matching
/// strings return `None`.
pub fn parse_call_target(s: &str) -> Option<UnresolvedTarget<'_>> {
    // Fast path: rule out strings that can't have "::" (two colons together)
    if !s.contains("::") {
        return None;
    }
    // Expected shape: "lang:kind::callee"
    // Split on ':' — we want exactly 4 components: lang, kind, "", callee.
    let parts: Vec<&str> = s.splitn(4, ':').collect();
    if parts.len() == 4 && parts[2].is_empty() {
        Some(UnresolvedTarget {
            lang: parts[0],
            kind: parts[1],
            callee: parts[3],
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_target_roundtrips() {
        let t = build_call_target("rust", "Function", "my_fn");
        assert_eq!(t, "rust:Function::my_fn");
        let parsed = parse_call_target(&t).expect("should parse");
        assert_eq!(parsed.lang, "rust");
        assert_eq!(parsed.kind, "Function");
        assert_eq!(parsed.callee, "my_fn");
    }

    #[test]
    fn real_node_id_does_not_parse() {
        // A real node id has a file component, not an empty one.
        assert!(parse_call_target("rust:Function:src/lib.rs:my_fn").is_none());
        assert!(parse_call_target("csharp:Method:Foo.cs:MyClass:MyMethod").is_none());
    }

    #[test]
    fn non_target_strings_return_none() {
        assert!(parse_call_target("").is_none());
        assert!(parse_call_target("hello").is_none());
        assert!(parse_call_target("a:b:c").is_none());
    }

    #[test]
    fn callee_with_colons_roundtrips() {
        // Edge case: callee that itself contains colons (e.g. a scoped Rust path)
        let t = build_call_target("rust", "Function", "std::mem::drop");
        let parsed = parse_call_target(&t).expect("should parse");
        assert_eq!(parsed.callee, "std::mem::drop");
    }
}
