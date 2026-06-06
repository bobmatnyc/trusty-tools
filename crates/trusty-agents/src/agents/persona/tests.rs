//! Unit tests for persona detection and skill-pack injection.
//!
//! Why: Pins the persona keyword heuristics (especially the #219 whole-word
//! tightening) and the layered task-context assembly so refactors can't
//! silently re-broaden hacker activation or reorder the injected sections.
//! What: Exercises `detect_persona`, `detect_persona_matched`,
//! `strip_persona_tag`, `language_for_agent`, the whole-word matcher, and
//! `assemble_task_with_context` against temp skill-file fixtures.
//! Test: This module IS the test surface.

use super::*;
use std::sync::Mutex;

// Tests in this module mutate `TAGENT_CONFIG_DIR`, so they share a lock
// to avoid clobbering each other when run in parallel under cargo test.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn fresh_tempdir() -> PathBuf {
    let base = std::env::temp_dir().join(format!("trusty-agents-persona-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(base.join("skills/personas")).unwrap();
    std::fs::create_dir_all(base.join("skills/languages")).unwrap();
    base
}

#[test]
fn detect_persona_explicit_tag_wins() {
    // Explicit tags must win even when keyword heuristics would say otherwise.
    assert_eq!(
        detect_persona("[hacker] write a quick script"),
        PERSONA_HACKER
    );
    assert_eq!(
        detect_persona("[engineer] just a one-off thing"),
        PERSONA_ENGINEER
    );
    assert_eq!(detect_persona("persona:novice fast"), PERSONA_NOVICE);
    assert_eq!(detect_persona("persona:vibe iterate"), PERSONA_VIBE_CODER);
}

#[test]
fn detect_persona_keyword_hacker() {
    // #219: hacker activation now requires explicit exploratory keywords
    // matched as whole words. The canonical opt-in vocabulary is:
    // hacker, quick, prototype, throwaway, spike.
    assert_eq!(detect_persona("write a quick script"), PERSONA_HACKER);
    assert_eq!(detect_persona("just a quick fix"), PERSONA_HACKER);
    assert_eq!(
        detect_persona("build a prototype dashboard"),
        PERSONA_HACKER
    );
    assert_eq!(detect_persona("throwaway exploration"), PERSONA_HACKER);
    assert_eq!(
        detect_persona("a spike to validate the idea"),
        PERSONA_HACKER
    );
    assert_eq!(detect_persona("hacker mode please"), PERSONA_HACKER);
}

// #205: bare "script" alone must NOT trigger hacker — it appears in CLI
// examples embedded in technical task specifications.
#[test]
fn detect_persona_bare_script_not_hacker() {
    // Technical spec with "script" in a CLI invocation path.
    assert_eq!(
        detect_persona(
            "Build a REST API service with CLI example: python3 script.py --format json"
        ),
        PERSONA_ENGINEER
    );
    // Task that contains the word "script" without a qualifying adjective.
    assert_eq!(
        detect_persona("Build a git_analyzer tool that runs as a script"),
        PERSONA_ENGINEER
    );
    // A realistic task from the issue report.
    assert_eq!(
        detect_persona("implement git_analyzer — run with: python3 -m git_analyzer --format json"),
        PERSONA_ENGINEER
    );
}

// #219: regression — substantive build tasks must NOT trigger hacker just
// because a colloquial English word like "from scratch" appears, or a
// technology name happens to contain a substring like "fast" (FastAPI).
// This is the exact failure mode reported in the issue: a genealogy
// build task got classified as hacker, skipping research + qa phases and
// leaving generated code unvalidated.
#[test]
fn detect_persona_substantive_build_not_hacker() {
    // The genealogy task from the issue report.
    assert_eq!(
        detect_persona(
            "build a genealogy tree viewer from scratch with TypeScript, GitHub issues and PRs, tests, data model"
        ),
        PERSONA_ENGINEER,
        "substantive build task with quality signals must NOT select hacker persona"
    );
    // "from scratch" is a normal phrase in production specs — not a hacker signal.
    assert_eq!(
        detect_persona("Implement the auth service from scratch with full test coverage"),
        PERSONA_ENGINEER
    );
    // "fast" inside FastAPI / breakfast / steadfast must not trigger hacker.
    assert_eq!(
        detect_persona("Build a Python FastAPI service with pytest tests"),
        PERSONA_ENGINEER
    );
    assert_eq!(
        detect_persona("Steadfast adherence to the spec is required"),
        PERSONA_ENGINEER
    );
    // "iterate" is a normal engineering verb — must not skip phases.
    assert_eq!(
        detect_persona("Iterate on the design until it converges"),
        PERSONA_ENGINEER
    );
    // "one-off" embedded in identifiers / phrases must not trigger.
    assert_eq!(
        detect_persona("Add a one_off_migrations table with proper schema"),
        PERSONA_ENGINEER
    );
    // "just make it work" is no longer a hacker keyword — substantive
    // tasks with retry semantics, CI work, etc. shouldn't fire hacker.
    assert_eq!(
        detect_persona("The pipeline should retry until it works correctly"),
        PERSONA_ENGINEER
    );
}

#[test]
fn detect_persona_keyword_novice() {
    assert_eq!(detect_persona("explain how iterators work"), PERSONA_NOVICE);
    assert_eq!(
        detect_persona("how do i parse json in python"),
        PERSONA_NOVICE
    );
    assert_eq!(
        detect_persona("teach me about borrow checker"),
        PERSONA_NOVICE
    );
}

#[test]
fn detect_persona_keyword_vibe() {
    // #219: `prototype` moved from vibe to hacker (more accurate semantics:
    // prototypes are throwaway). `iterate` is no longer a vibe keyword
    // because it appears in too many substantive specs. `poc` remains
    // (with whole-word matching) and the "just show" / "no explanation"
    // phrases still signal vibe-coder.
    assert_eq!(detect_persona("just show me a poc"), PERSONA_VIBE_CODER);
    assert_eq!(detect_persona("just show me how"), PERSONA_VIBE_CODER);
    assert_eq!(
        detect_persona("no explanation needed, give me the answer"),
        PERSONA_VIBE_CODER
    );
}

#[test]
fn detect_persona_default_engineer() {
    // No signals → default engineer.
    assert_eq!(
        detect_persona("Implement a CSV-to-Markdown converter."),
        PERSONA_ENGINEER
    );
    assert_eq!(
        detect_persona("Refactor the auth middleware to use a trait."),
        PERSONA_ENGINEER
    );
}

#[test]
fn strip_persona_tag_removes_bracketed_form() {
    assert_eq!(
        strip_persona_tag("[hacker] write a quick script"),
        "write a quick script"
    );
    assert_eq!(strip_persona_tag("hello [engineer] world"), "hello world");
}

#[test]
fn strip_persona_tag_removes_colon_form() {
    assert_eq!(strip_persona_tag("persona:novice teach me"), "teach me");
    assert_eq!(
        strip_persona_tag("persona:vibe iterate fast"),
        "iterate fast"
    );
}

#[test]
fn strip_persona_tag_no_op() {
    assert_eq!(
        strip_persona_tag("plain task with no persona tag"),
        "plain task with no persona tag"
    );
}

#[test]
fn language_for_agent_python() {
    assert_eq!(
        language_for_agent("python-engineer"),
        Some("python-idiomatic")
    );
    assert_eq!(language_for_agent("Python"), Some("python-idiomatic"));
}

#[test]
fn language_for_agent_unknown() {
    assert_eq!(language_for_agent("plan-agent"), None);
    assert_eq!(language_for_agent("docs-agent"), None);
    assert_eq!(language_for_agent("research-agent"), None);
}

#[test]
fn language_for_agent_other_languages() {
    assert_eq!(language_for_agent("rust-engineer"), Some("rust-idiomatic"));
    assert_eq!(
        language_for_agent("typescript-engineer"),
        Some("typescript-idiomatic")
    );
    assert_eq!(
        language_for_agent("react-engineer"),
        Some("react-idiomatic")
    );
    assert_eq!(language_for_agent("java-backend"), Some("java-idiomatic"));
    assert_eq!(language_for_agent("go-engineer"), Some("go-idiomatic"));
    // `java` should not match `javascript`.
    assert_eq!(language_for_agent("javascript-engineer"), None);
}

#[test]
fn strip_frontmatter_local_removes_block() {
    let content = "---\nname: x\ntags: [a]\n---\n# Body\ntext\n";
    let stripped = strip_frontmatter(content);
    assert!(stripped.starts_with("# Body"));
}

#[test]
fn strip_frontmatter_local_passthrough() {
    let content = "no frontmatter here\n";
    assert_eq!(strip_frontmatter(content), content);
}

#[test]
fn persona_path_uses_env_override() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = fresh_tempdir();
    // Caller normally sets TAGENT_CONFIG_DIR to <root>/agents; we
    // replicate that here so we can verify the parent-stripping logic.
    let agents_dir = dir.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", &agents_dir);
    }
    let p = persona_skill_path("hacker");
    assert_eq!(p, dir.join("skills/personas/hacker.md"));
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
}

#[test]
fn assemble_task_with_context_includes_both_blocks() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = fresh_tempdir();
    let agents_dir = dir.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    std::fs::write(
        dir.join("skills/personas/hacker.md"),
        "---\nname: hacker\n---\nHACKER BODY",
    )
    .unwrap();
    std::fs::write(
        dir.join("skills/languages/python-idiomatic.md"),
        "---\nname: python-idiomatic\n---\nPYTHON BODY",
    )
    .unwrap();

    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", &agents_dir);
    }
    let out = assemble_task_with_context("python-engineer", "[hacker] write a quick csv reader");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    assert!(
        out.contains("## Language Conventions"),
        "missing language block: {out}"
    );
    assert!(out.contains("PYTHON BODY"), "missing python body: {out}");
    assert!(
        out.contains("## Persona Directive"),
        "missing persona block: {out}"
    );
    assert!(out.contains("HACKER BODY"), "missing hacker body: {out}");
    assert!(out.contains("## Task"), "missing task block: {out}");
    assert!(
        out.contains("write a quick csv reader"),
        "task content missing: {out}"
    );
    // Persona tag must be stripped from the task body section.
    // Use a region check — the marker may appear in the persona body itself.
    let task_idx = out.find("## Task").unwrap();
    assert!(
        !out[task_idx..].contains("[hacker]"),
        "persona tag leaked into task body: {out}"
    );

    // Language must come before persona so persona overrides language.
    let lang_pos = out.find("## Language Conventions").unwrap();
    let pers_pos = out.find("## Persona Directive").unwrap();
    assert!(
        lang_pos < pers_pos,
        "language block must precede persona block"
    );
}

#[test]
fn assemble_task_with_context_strips_tag() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = fresh_tempdir();
    let agents_dir = dir.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    std::fs::write(
        dir.join("skills/personas/engineer.md"),
        "---\nname: engineer\n---\nENGINEER BODY",
    )
    .unwrap();

    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", &agents_dir);
    }
    let out = assemble_task_with_context("plan-agent", "[engineer] design the API");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    // plan-agent has no language pack, so only persona block is injected.
    assert!(out.contains("## Persona Directive"));
    assert!(!out.contains("## Language Conventions"));
    // The user-facing task no longer carries the explicit tag.
    let task_idx = out.find("## Task").unwrap();
    assert!(!out[task_idx..].contains("[engineer]"));
}

#[test]
fn assemble_task_with_context_no_language_for_unknown_agent() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = fresh_tempdir();
    let agents_dir = dir.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();

    std::fs::write(
        dir.join("skills/personas/engineer.md"),
        "---\n---\nENGINEER ONLY",
    )
    .unwrap();

    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", &agents_dir);
    }
    let out = assemble_task_with_context("docs-agent", "Update the README");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    assert!(out.contains("## Persona Directive"));
    assert!(!out.contains("## Language Conventions"));
}

#[test]
fn assemble_task_falls_back_to_plain_task_when_no_skills_present() {
    let _g = ENV_LOCK.lock().unwrap();
    let dir = fresh_tempdir();
    let agents_dir = dir.join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    // Note: no persona or language file written.

    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", &agents_dir);
    }
    let out = assemble_task_with_context("docs-agent", "[engineer] write something");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
    // No skill files → graceful degradation: tag stripped, task returned plain.
    assert_eq!(out, "write something");
}

// #205: detect_persona_matched returns (persona, matched_keyword).
#[test]
fn detect_persona_matched_explicit_tag() {
    let (p, m) = detect_persona_matched("[hacker] do stuff");
    assert_eq!(p, PERSONA_HACKER);
    assert_eq!(m, "[hacker]");

    let (p, m) = detect_persona_matched("[engineer] do stuff");
    assert_eq!(p, PERSONA_ENGINEER);
    assert_eq!(m, "[engineer]");

    let (p, m) = detect_persona_matched("persona:novice explain");
    assert_eq!(p, PERSONA_NOVICE);
    assert_eq!(m, "persona:novice");
}

#[test]
fn detect_persona_matched_keyword() {
    // #219: the canonical hacker keyword set is now whole-word: hacker,
    // quick, prototype, throwaway, spike. `detect_persona_matched`
    // returns the matched word verbatim for operator visibility.
    let (p, m) = detect_persona_matched("write a quick script");
    assert_eq!(p, PERSONA_HACKER);
    assert_eq!(m, "quick");

    let (p, m) = detect_persona_matched("build a prototype dashboard");
    assert_eq!(p, PERSONA_HACKER);
    assert_eq!(m, "prototype");

    let (p, m) = detect_persona_matched("a spike to validate the idea");
    assert_eq!(p, PERSONA_HACKER);
    assert_eq!(m, "spike");
}

// #219: regression — the matched-keyword reporter must NOT classify
// substantive tasks as hacker. A genealogy build task with "from scratch"
// should land on PERSONA_ENGINEER with "(default)" as the match reason.
#[test]
fn detect_persona_matched_substantive_build_is_default() {
    let (p, m) = detect_persona_matched(
        "build a genealogy tree viewer from scratch with TypeScript, GitHub issues and PRs, tests, data model",
    );
    assert_eq!(p, PERSONA_ENGINEER);
    assert_eq!(m, "(default)");

    // "FastAPI" must not surface as a "fast" match.
    let (p, m) = detect_persona_matched("Build a Python FastAPI service with pytest tests");
    assert_eq!(p, PERSONA_ENGINEER);
    assert_eq!(m, "(default)");
}

// #219: unit tests for the whole-word matcher itself.
#[test]
fn contains_word_matches_standalone() {
    assert!(contains_word("just a quick fix", "quick"));
    assert!(contains_word("quick", "quick"));
    assert!(contains_word("a quick.", "quick"));
    assert!(contains_word("[hacker] do x", "hacker"));
}

#[test]
fn contains_word_rejects_substring_matches() {
    // The whole point of the helper: don't match inside identifiers / words.
    assert!(!contains_word("fastapi service", "fast"));
    assert!(!contains_word("breakfast meeting", "fast"));
    assert!(!contains_word("steadfast", "fast"));
    // Sanity: word-bounded "scratch" *does* appear in "from scratch
    // implementation" — that's the exact case that motivated #219.
    // contains_word matches it; we just no longer treat "scratch" as a
    // hacker keyword. The overall persona resolution returns engineer
    // (covered by `detect_persona_substantive_build_not_hacker`).
    assert!(contains_word("from scratch implementation", "scratch"));
    // Underscore- and digit-bounded substrings must not match.
    assert!(!contains_word("one_off_migrations", "one"));
    assert!(!contains_word("spiked drink", "spike"));
    assert!(!contains_word("prototyping", "prototype"));
}

#[test]
fn detect_persona_matched_default() {
    let (p, m) = detect_persona_matched("Implement a REST API with OpenAPI spec");
    assert_eq!(p, PERSONA_ENGINEER);
    assert_eq!(m, "(default)");
}

// Verify bare "script" alone resolves to engineer with "(default)" match.
#[test]
fn detect_persona_matched_bare_script_is_default() {
    let (p, m) = detect_persona_matched("Build a tool that outputs results via a script wrapper");
    assert_eq!(p, PERSONA_ENGINEER, "bare 'script' should not match hacker");
    assert_eq!(m, "(default)");
}
