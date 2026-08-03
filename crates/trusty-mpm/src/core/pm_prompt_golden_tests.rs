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
//! | `pm-prompt-roster-absent.md` | no agent deployed in any tier | legacy assembly |
//! | `pm-prompt-claude-md-override.md` | `CLAUDE.md` named sections (#4286) | `InstructionPackage` |
//!
//! The second is not redundant. It is the check that a project composing through
//! the string assembly still receives the *same* Core/Memory/Search/Workflow and
//! floor text as one composing through the package — the split-brain that
//! authoring sections would otherwise have introduced, since the legacy path
//! formerly read separate monolithic assets.
//!
//! Regenerate with `UPDATE_GOLDEN=1 cargo test -p trusty-mpm golden`. Review the
//! resulting `git diff` before committing it; that diff IS the deliverable.

use std::path::{Path, PathBuf};

use crate::core::bundled_pm_package::compose_bundled_fallback_with_overrides;
use crate::core::instruction_overrides::resolve_pm_prompt_with_roster;
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
    let composed = compose_bundled_fallback_with_overrides(FIXED_STACK, FIXED_ROSTER, None, &[])
        .0
        .expect("package composes");
    assert_golden("pm-prompt-bundled-fallback.md", &composed);
}

#[test]
fn golden_roster_absent_assembly_prompt() {
    // Configuration 2, REPLACING the retired `.trusty-mpm/AGENT_DELEGATION.md`
    // snapshot (#4286). That configuration no longer exists — no file can force
    // the string assembly any more — so the golden that covered it would have
    // pinned an unreachable code path.
    //
    // The string assembly is still reachable by exactly one route: no agent
    // deployed in any tier, so there is no roster for the packaged composer to
    // consume. That is the configuration snapshotted here, and it serves the
    // same purpose the old one did — proving a project on the non-packaged path
    // receives the same Core/Memory/Search/Workflow and floor text as everyone
    // else, rather than being frozen on a divergent copy.
    let tmp = TempDir::new().expect("tempdir");
    let (prompt, _) = resolve_pm_prompt_with_roster(tmp.path(), || None);
    assert_golden("pm-prompt-roster-absent.md", &prompt);
}

#[test]
fn golden_claude_md_override_prompt() {
    // Configuration 3 (#4286): named-section overrides marked out in the
    // project's own `CLAUDE.md`. The snapshot is what makes the DELIVERED shape
    // of an override reviewable — that WORKFLOW is replaced wholesale, that
    // AGENT-DELEGATION replaces the doctrine but NOT the live roster below it,
    // and that the framework floor is byte-for-byte the floor every other
    // configuration receives.
    let tmp = TempDir::new().expect("tempdir");
    std::fs::write(
        tmp.path().join("CLAUDE.md"),
        "# Project Instructions\n\n\
         Prose outside the markers is not instruction content and is ignored.\n\n\
         <!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n\
         # Workflow (project override)\n\n\
         Two phases only: implement, then verify.\n\
         <!-- TRUSTY-MPM: WORKFLOW END -->\n\n\
         <!-- TRUSTY-MPM: AGENT-DELEGATION START v=1 -->\n\
         # Routing (project override)\n\n\
         Route every implementation task to `rust-engineer`.\n\
         <!-- TRUSTY-MPM: AGENT-DELEGATION END -->\n",
    )
    .expect("write CLAUDE.md");

    let (prompt, _) = resolve_pm_prompt_with_roster(tmp.path(), || Some(FIXED_ROSTER.to_string()));
    assert_golden("pm-prompt-claude-md-override.md", &prompt);
}
