//! Committed snapshots of the fully composed PM prompt (#4183).
//!
//! Why: #4249 could gate itself on strict byte-equality against the previous
//! delivered prompt, because it was a mechanical re-sourcing that changed no
//! content. The section-authoring work cannot — it changes the delivered prompt
//! on purpose — and removing that gate with nothing in its place would leave the
//! single highest-blast-radius artifact in the product (the system prompt every
//! PM session receives) with no automatic diff at all. Every future edit to a
//! section source would land as an invisible change.
//!
//! What: one golden file per composition configuration, checked byte-for-byte.
//! A content edit does not "break" these tests in any meaningful sense — it
//! makes them print the change and requires the author to commit the updated
//! snapshot, so the delivered-prompt diff appears in the PR where a reviewer
//! reads it as MEANING. That is the acceptance artifact epic #4183 asks for.
//!
//! Two configurations, deliberately, because they exercise the two composers:
//!
//! | golden | configuration | composer |
//! |---|---|---|
//! | `pm-prompt-bundled-fallback.md` | no `.trusty-mpm/` override, roster present | `InstructionPackage` |
//! | `pm-prompt-legacy-delegation-override.md` | `.trusty-mpm/AGENT_DELEGATION.md` present | legacy assembly |
//!
//! The second is not redundant. It is the check that a project which overrides
//! one section still receives the *same* Core/Memory/Search/Workflow/floor text
//! as a project that overrides nothing — the split-brain that authoring sections
//! would otherwise have introduced, since the legacy path formerly read separate
//! monolithic assets.
//!
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden`. Review the
//! resulting `git diff` before committing it; that diff IS the deliverable.

use std::path::{Path, PathBuf};

use crate::core::bundled_pm_package::compose_bundled_fallback;
use crate::core::instruction_overrides::{
    FILE_AGENT_DELEGATION, OVERRIDE_DIR_NAME, resolve_pm_prompt_with_roster,
};
use tempfile::TempDir;

/// A fixed, deterministic roster.
///
/// The real roster is scanned from disk and varies per machine, which would make
/// the snapshot environment-dependent. Nothing here depends on its content.
const FIXED_ROSTER: &str = "## Delegation Authority\n\n\
     ### ticketing\n\nHandles ticketing work. Model: sonnet.\n\n\
     ### rust-engineer\n\nHandles Rust work. Model: sonnet.";

/// A fixed stack-profile block, standing in for `stack_profile_section`.
const FIXED_STACK: &str =
    "## Project Stack Profile\n\nDetected stack: Rust. Route implementation to `rust-engineer`.";

/// A fixed delegation override, standing in for a project's own routing rules.
const FIXED_DELEGATION_OVERRIDE: &str = "# Custom Routing\n\nROUTE_ALL_TO_ENGINEER\n";

/// Directory holding the committed snapshots.
fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("core")
        .join("testdata")
}

/// Compare `actual` against the committed snapshot `name`, or rewrite it.
///
/// Why: an `assert_eq!` on two ~40 KB strings is unreadable, and a snapshot with
/// no regeneration path gets deleted the first time it is inconvenient. This
/// reports the first differing byte with context and names the exact command
/// that updates it, so the correct response to an intentional content change is
/// obvious and cheap.
/// What: with `UPDATE_GOLDEN` set, writes `actual` and passes. Otherwise reads
/// the snapshot and asserts byte-equality.
/// Test: this IS the assertion helper; exercised by both golden tests below.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_dir().join(name);

    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden_dir()).expect("create testdata dir");
        std::fs::write(&path, actual).expect("write golden");
        return;
    }

    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("missing golden {}: {err}", path.display()));
    if expected == actual {
        return;
    }

    let (e, a) = (expected.as_bytes(), actual.as_bytes());
    let at = e
        .iter()
        .zip(a.iter())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| e.len().min(a.len()));

    /// Bytes of context shown either side of the divergence.
    const WINDOW: usize = 200;
    let from = at.saturating_sub(WINDOW);
    let window = |s: &[u8]| String::from_utf8_lossy(&s[from..s.len().min(at + WINDOW)]).to_string();

    panic!(
        "DELIVERED PM PROMPT CHANGED — {name}\n\
         first difference at byte offset {at} (golden {} bytes, composed {} bytes)\n\
         --- golden   [{from}..] ---\n{}\n\
         --- composed [{from}..] ---\n{}\n\
         If this change is intended, regenerate and review the diff:\n\
         \x20   UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden\n",
        e.len(),
        a.len(),
        window(e),
        window(a),
    );
}

#[test]
fn golden_bundled_fallback_prompt() {
    // Configuration 1: what every project with no `.trusty-mpm/` override
    // receives, composed through `InstructionPackage`.
    let composed =
        compose_bundled_fallback(FIXED_STACK, FIXED_ROSTER, None).expect("package composes");
    assert_golden("pm-prompt-bundled-fallback.md", &composed);
}

#[test]
fn golden_legacy_delegation_override_prompt() {
    // Configuration 2: the legacy assembly. Its Core/Memory/Search/Workflow and
    // floor text must track the same section sources — otherwise authoring
    // sections would have split the product in two, with overriding projects
    // frozen on the old instructions.
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join(OVERRIDE_DIR_NAME);
    std::fs::create_dir_all(&dir).expect("create .trusty-mpm");
    std::fs::write(dir.join(FILE_AGENT_DELEGATION), FIXED_DELEGATION_OVERRIDE)
        .expect("write override");

    let (prompt, _) = resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string()));
    assert_golden("pm-prompt-legacy-delegation-override.md", &prompt);
}
