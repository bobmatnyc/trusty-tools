//! Tests for the instruction merge pipeline and bundled prompt assembly.
//!
//! Split out of `instruction_pipeline.rs` (#4318) so the production file stays
//! under the 500-SLOC cap enforced by `scripts/check_line_cap.sh`. The module is
//! included with `#[path]`, so `use super::*` still reaches the pipeline's items
//! exactly as it did inline; nothing about the assertions changed in the move.

use super::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Write `<name>.md` into `dir` with the given raw content.
fn write_file(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content).expect("write file");
}

// ── #4588 / #4589: one resolution path, two operator-visible numbers ────────

/// RAII override of every environment input
/// [`crate::core::delegation_authority::deployed_agent_dirs`] consults, so a
/// roster assertion is a property of the TEST rather than of the developer's
/// machine.
///
/// Why: the roster is a three-tier union and two of those tiers are
/// machine-global (`$CLAUDE_CONFIG_DIR/agents` and `$HOME/.claude/agents`).
/// Without this, the exact-count assertions below would read whatever the
/// developer happens to have deployed — and live `tm` sessions rewrite those
/// directories mid-run, which is a documented 1-in-4 flake in
/// `bundled_pm_package_tests`.
/// What: points `$HOME` and `$CLAUDE_CONFIG_DIR` at a fresh temp root and
/// restores both on drop (including on a panic-driven unwind).
/// Test: used by `session_start_count_matches_the_delivered_delegation_roster`.
struct RosterTiers {
    tmp: TempDir,
    prev_home: Option<std::ffi::OsString>,
    prev_config: Option<std::ffi::OsString>,
}

impl RosterTiers {
    /// Callers MUST be tagged `#[serial_test::serial]` — this mutates
    /// process-global environment state.
    fn new() -> Self {
        let tmp = TempDir::new().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        let prev_config = std::env::var_os("CLAUDE_CONFIG_DIR");
        // SAFETY: every caller is `#[serial]`, so no other test thread races
        // this set/restore.
        unsafe {
            std::env::set_var("HOME", tmp.path().join("home"));
            std::env::set_var("CLAUDE_CONFIG_DIR", tmp.path().join("claude-config"));
        }
        Self {
            tmp,
            prev_home,
            prev_config,
        }
    }

    /// The project whose roster is under test.
    fn project(&self) -> PathBuf {
        self.tmp.path().join("project")
    }

    /// Tier 1 — `<project>/.claude/agents`, the operator's hand-placed agents.
    fn project_tier(&self) -> PathBuf {
        self.project().join(".claude").join("agents")
    }

    /// Tier 2 — the tm-managed `$CLAUDE_CONFIG_DIR/agents` bundled agents
    /// deploy into since #4409. This is the ONE directory the pre-#4588
    /// session-start count was derived from.
    fn managed_tier(&self) -> PathBuf {
        self.tmp.path().join("claude-config").join("agents")
    }

    /// Tier 3 — the operator's own generic `~/.claude/agents`, which tm never
    /// writes to but a launched session still resolves agents from. This tier
    /// held the five agents behind #4588's observed 34-vs-39 delta.
    fn generic_tier(&self) -> PathBuf {
        self.tmp.path().join("home").join(".claude").join("agents")
    }
}

impl Drop for RosterTiers {
    fn drop(&mut self) {
        // SAFETY: caller is `#[serial]`.
        unsafe {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match self.prev_config.take() {
                Some(v) => std::env::set_var("CLAUDE_CONFIG_DIR", v),
                None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
            }
        }
    }
}

/// Write a minimal composed agent named `name` into `dir`.
fn write_roster_agent(dir: &Path, name: &str) {
    fs::create_dir_all(dir).expect("create tier");
    fs::write(
        dir.join(format!("{name}.md")),
        format!("---\nname: {name}\nrole: {name}\ndescription: Handles {name} work.\n---\n"),
    )
    .expect("write agent");
}

/// Build a `PipelineInput` for the isolated project `tiers` describes.
///
/// #4588: the pipeline resolves the roster from the PROJECT, not from a
/// caller-named directory, so every test that asserts an exact `agent_count`
/// must pin all three tiers — hence [`RosterTiers`] rather than a bare
/// `TempDir`.
fn input_in(tiers: &RosterTiers) -> PipelineInput {
    PipelineInput {
        project_dir: tiers.project(),
        claude_md_path: tiers.project().join("CLAUDE.md"),
    }
}

#[serial_test::serial]
#[test]
fn pipeline_full() {
    // The pipeline reports the roster it resolved and leaves an existing
    // CLAUDE.md alone. #4832 removed the `merged` text this used to assert on.
    let tiers = RosterTiers::new();
    let input = input_in(&tiers);

    write_roster_agent(&tiers.project_tier(), "engineer");
    write_file(&input.claude_md_path, "# Project\n\nPROJECT SECTION\n");

    let out = build_instructions(&input).unwrap();
    assert_eq!(out.agent_count, 1);
    assert!(
        !out.claude_md_created,
        "existing CLAUDE.md is not recreated"
    );
}

#[serial_test::serial]
#[test]
fn pipeline_does_not_read_the_retired_framework_instructions() {
    // Why (#4832): `build_instructions` used to read
    // `<framework_root>/instructions/INSTRUCTIONS.md`, and ANY error other than
    // `NotFound` — a directory planted at that path is the cheapest one to
    // produce — became `PrepError::Instructions`, the sole fatal variant. The
    // content was never used, so a session died over unread bytes.
    // FAILS BEFORE THIS CHANGE: the read returned `IsADirectory` and the
    // pipeline returned `Err`.
    let tiers = RosterTiers::new();
    let input = input_in(&tiers);
    let retired =
        crate::core::paths::FrameworkPaths::under(tiers.tmp.path()).framework_instructions_path();
    fs::create_dir_all(&retired).expect("plant a directory at the retired path");
    write_roster_agent(&tiers.project_tier(), "qa");
    write_file(&input.claude_md_path, "# Project\n\nPROJECT NOTES\n");

    let out =
        build_instructions(&input).expect("an unreadable retired path must not fail the pipeline");
    assert_eq!(out.agent_count, 1);
}

#[serial_test::serial]
#[test]
fn pipeline_creates_claude_md() {
    // CLAUDE.md absent → it is created as a side effect, claude_md_created
    // is true, and the file on disk contains the stub — but the stub text
    // is NOT folded into `merged` (dead code removed).
    let tiers = RosterTiers::new();
    let input = input_in(&tiers);

    assert!(!input.claude_md_path.exists());
    let out = build_instructions(&input).unwrap();
    assert!(out.claude_md_created);
    assert!(input.claude_md_path.exists());

    let on_disk = fs::read_to_string(&input.claude_md_path).unwrap();
    assert!(on_disk.contains("# Project Instructions"));
    assert!(on_disk.contains("trusty-mpm will never modify this file again"));
    // The seeded stub carries the attribution convention so every new
    // trusty-mpm project inherits the footer override at the framework
    // level (issue #2876) rather than relying on a per-project hand-edit.
    assert!(
        on_disk.contains("🤖🤖🤖 Generated with trusty-mpm"),
        "seeded stub must carry the attribution footer: {on_disk}"
    );
}

#[serial_test::serial]
#[test]
fn pipeline_claude_md_left_byte_identical() {
    // Why (#2170): trusty-mpm must NEVER modify a target project's
    // CLAUDE.md. An existing file — including one that happens to
    // contain the literal delegation-block markers as ordinary operator
    // text — must come back out of `build_instructions` completely
    // untouched on disk, and its content must not leak into `merged`
    // (dead code removed).
    let tiers = RosterTiers::new();
    let input = input_in(&tiers);
    let custom = "# My Project\n\nCUSTOM HAND-WRITTEN CONTENT\n";
    write_file(&input.claude_md_path, custom);

    let out = build_instructions(&input).unwrap();
    assert!(!out.claude_md_created);
    let on_disk = fs::read_to_string(&input.claude_md_path).unwrap();
    assert_eq!(
        on_disk, custom,
        "pre-existing CLAUDE.md must be left byte-identical: {on_disk}"
    );
    assert!(
        !on_disk.contains(DELEGATION_BLOCK_BEGIN),
        "no delegation-directive block may be injected: {on_disk}"
    );
    let _ = out;
}

// ── #5228: absent CLAUDE.md ≠ new project ───────────────────────────────────

/// A REAL git repository whose `main` gained `CLAUDE.md` after a branch was cut
/// from it — the exact shape #5228 reproduced in a live worktree.
///
/// Why a real repo and not a mocked filesystem: the bug is entirely about git
/// history. What separates "new project" from "stale branch" is whether a
/// remote-tracking ref carries the file, which only git can answer; a fake
/// filesystem would assert against the test's own stub of that answer and would
/// have passed against the pre-fix code.
/// What: a bare `origin` whose `main` holds commit 1 (no `CLAUDE.md`) then
/// commit 2 (adds it), plus a real `git worktree` checked out on a branch cut
/// at commit 1 with its upstream pointed at `origin/main` — mirroring what
/// `configure_session_branch_tracking` (#2189) sets up in production.
/// Test: the four `#5228` tests below.
struct StaleWorktreeFixture {
    _tmp: TempDir,
    /// The per-session worktree, on a branch that predates `CLAUDE.md`.
    worktree: PathBuf,
    /// The tracked `CLAUDE.md` content on `origin/main`.
    upstream_content: &'static str,
}

/// The upstream `CLAUDE.md` body — deliberately unlike [`CLAUDE_MD_STUB`], so
/// an assertion cannot pass on a stub that happens to share a heading.
const UPSTREAM_CLAUDE_MD: &str = "# Real Project Instructions\n\nDo the real thing.\n";

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_identity(dir: &Path) {
    assert!(git_ok(dir, &["config", "user.email", "ci@test.invalid"]));
    assert!(git_ok(dir, &["config", "user.name", "CI"]));
    assert!(git_ok(dir, &["config", "commit.gpgsign", "false"]));
}

impl StaleWorktreeFixture {
    /// Returns `None` when git is unavailable — callers skip rather than fail,
    /// matching `session_launch::worktree_sync`'s fixture.
    ///
    /// `add_claude_md_upstream`: when false, `origin/main` never gains a
    /// `CLAUDE.md`, which is the genuinely-new-project control.
    fn build(add_claude_md_upstream: bool) -> Option<Self> {
        let tmp = crate::test_support::hermetic_temp_dir();
        let root = std::fs::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().into());
        let origin = root.join("origin.git");
        let seed = root.join("seed");
        let base = root.join("base");
        let worktree = root.join("worktree");
        fs::create_dir_all(&origin).ok()?;
        fs::create_dir_all(&seed).ok()?;

        if !git_ok(&origin, &["init", "--bare", "--initial-branch=main"]) {
            eprintln!("#5228 tests: git unavailable, skipping");
            return None;
        }
        assert!(git_ok(&seed, &["init", "--initial-branch=main"]));
        git_identity(&seed);
        // Commit 1: the state the stale branch is cut from — no CLAUDE.md.
        fs::write(seed.join("README.md"), "base\n").ok()?;
        assert!(git_ok(&seed, &["add", "-A"]));
        assert!(git_ok(&seed, &["commit", "-m", "base"]));
        assert!(git_ok(
            &seed,
            &["remote", "add", "origin", origin.to_str()?]
        ));
        assert!(git_ok(&seed, &["push", "origin", "main"]));
        assert!(git_ok(
            &origin,
            &["symbolic-ref", "HEAD", "refs/heads/main"]
        ));

        // The checkout the worktree is cut from, at commit 1.
        // Cloned from the fixture's own root, never the test process's cwd —
        // that cwd is this repository's checkout.
        assert!(git_ok(&root, &["clone", origin.to_str()?, base.to_str()?]));
        git_identity(&base);
        assert!(git_ok(
            &base,
            &["worktree", "add", "-b", "stale-branch", worktree.to_str()?]
        ));
        git_identity(&worktree);
        assert!(git_ok(
            &worktree,
            &["branch", "--set-upstream-to=origin/main", "stale-branch"]
        ));

        if add_claude_md_upstream {
            // Commit 2 — CLAUDE.md becomes tracked upstream (#4660/#4661), out
            // from under the already-cut branch.
            fs::write(seed.join("CLAUDE.md"), UPSTREAM_CLAUDE_MD).ok()?;
            assert!(git_ok(&seed, &["add", "-A"]));
            assert!(git_ok(&seed, &["commit", "-m", "track CLAUDE.md"]));
            assert!(git_ok(&seed, &["push", "origin", "main"]));
            // The worktree learns about it the way a live one does: a fetch of
            // the shared remote-tracking refs, with no merge.
            assert!(git_ok(&worktree, &["fetch", "origin"]));
        }

        assert!(
            !worktree.join("CLAUDE.md").exists(),
            "fixture invariant: the stale worktree must not have CLAUDE.md on disk"
        );
        Some(Self {
            _tmp: tmp,
            worktree,
            upstream_content: UPSTREAM_CLAUDE_MD,
        })
    }

    fn claude_md(&self) -> PathBuf {
        self.worktree.join("CLAUDE.md")
    }
}

#[test]
fn load_or_create_claude_md_refuses_to_stub_a_branch_predating_the_tracked_file() {
    // FAILS BEFORE THIS CHANGE (#5228): the pre-fix writer read `NotFound`,
    // wrote CLAUDE_MD_STUB, and returned `Ok((stub, true))` — handing the
    // session boilerplate in place of the project's only non-dynamic
    // instruction source, and leaving an untracked file that looks authored to
    // every later run.
    let Some(fx) = StaleWorktreeFixture::build(true) else {
        return;
    };
    let path = fx.claude_md();

    let err = load_or_create_claude_md(&path)
        .expect_err("a branch that merely predates the tracked CLAUDE.md must not be stubbed");

    assert!(
        !path.exists(),
        "the refusal must leave nothing behind: a stub on disk is the permanent half of the bug"
    );
    let PipelineError::Io { source, .. } = &err;
    let msg = source.to_string();
    assert!(msg.contains("stale-branch"), "must name the branch: {msg}");
    assert!(msg.contains("origin/main"), "must name the ref: {msg}");
    assert!(
        msg.contains("merge --ff-only"),
        "must carry a recovery command: {msg}"
    );
    let _ = fx.upstream_content;
}

#[test]
fn build_instructions_refuses_a_stale_worktree_rather_than_seeding_a_stub() {
    // The refusal has to reach the pipeline, because that is where it becomes
    // fatal: `prepare_session` maps a `PipelineError` onto
    // `PrepError::Instructions`, the one condition #4752 rules must stop a
    // launch. A writer that refused while `build_instructions` still returned
    // `Ok` would start the session anyway.
    let Some(fx) = StaleWorktreeFixture::build(true) else {
        return;
    };
    let input = PipelineInput {
        project_dir: fx.worktree.clone(),
        claude_md_path: fx.claude_md(),
    };

    let err = build_instructions(&input).expect_err("a stale worktree must refuse the pipeline");
    assert!(
        err.to_string().contains("CLAUDE.md"),
        "the pipeline error must name the file: {err}"
    );
    assert!(!fx.claude_md().exists(), "no stub may be written");
}

#[test]
fn load_or_create_claude_md_still_seeds_when_upstream_has_no_claude_md() {
    // The ordinary path inside a real repository: `origin/main` never tracked a
    // CLAUDE.md, so this project genuinely is new and the stub is correct. The
    // #5228 probe must not turn "in a git repo" into "refuse".
    let Some(fx) = StaleWorktreeFixture::build(false) else {
        return;
    };
    let path = fx.claude_md();

    let (content, created) =
        load_or_create_claude_md(&path).expect("a genuinely new project is still seeded");
    assert!(created);
    assert_eq!(content, CLAUDE_MD_STUB);
    assert_eq!(fs::read_to_string(&path).unwrap(), CLAUDE_MD_STUB);
}

#[test]
fn load_or_create_claude_md_reads_a_present_file_in_a_git_worktree() {
    // The ordinary path with the file present — the state a worktree reaches
    // after the recovery command the refusal prints. It must read back
    // byte-identical and report `created == false`, with no git probe involved.
    let Some(fx) = StaleWorktreeFixture::build(true) else {
        return;
    };
    assert!(git_ok(&fx.worktree, &["merge", "--ff-only", "origin/main"]));
    let path = fx.claude_md();
    assert_eq!(fs::read_to_string(&path).unwrap(), fx.upstream_content);

    let (content, created) = load_or_create_claude_md(&path).expect("a present file loads");
    assert!(!created, "an existing CLAUDE.md is never recreated");
    assert_eq!(content, fx.upstream_content);
    assert_eq!(fs::read_to_string(&path).unwrap(), fx.upstream_content);
}

#[test]
fn load_or_create_claude_md_still_seeds_in_a_repo_with_no_remote() {
    // The fall-open arm the fixture's `else { return; }` skip does NOT cover:
    // git answers, the directory IS a work tree, and no candidate ref exists at
    // all. `@{upstream}`, `origin/HEAD`, `origin/main` and `origin/master` must
    // all miss and the stub must still be seeded. An untested fall-open arm is
    // where the next regression lands.
    let tmp = crate::test_support::hermetic_temp_dir();
    let dir = tmp.path();
    if !git_ok(dir, &["init", "--initial-branch=main"]) {
        eprintln!("#5228 tests: git unavailable, skipping");
        return;
    }
    git_identity(dir);
    fs::write(dir.join("README.md"), "local only\n").unwrap();
    assert!(git_ok(dir, &["add", "-A"]));
    assert!(git_ok(dir, &["commit", "-m", "local only"]));

    let path = dir.join("CLAUDE.md");
    let (content, created) =
        load_or_create_claude_md(&path).expect("a repo with no remote is not stale, it is local");
    assert!(created);
    assert_eq!(content, CLAUDE_MD_STUB);
    assert_eq!(fs::read_to_string(&path).unwrap(), CLAUDE_MD_STUB);
}

#[test]
fn load_or_create_claude_md_ignores_an_abandoned_origin_main_when_upstream_answered() {
    // Why (#5228 review, MEDIUM 1): `origin/main`/`origin/master` are a LAST
    // RESORT, not an override. Default branch `develop` carries no CLAUDE.md, so
    // this project is genuinely new — but a stale `origin/main` still has one.
    // Consulting it would refuse a launch that should proceed AND print a remedy
    // telling the operator to merge an abandoned branch.
    let tmp = crate::test_support::hermetic_temp_dir();
    let root = std::fs::canonicalize(tmp.path()).unwrap_or_else(|_| tmp.path().into());
    let origin = root.join("origin.git");
    let seed = root.join("seed");
    let work = root.join("work");
    fs::create_dir_all(&origin).unwrap();
    fs::create_dir_all(&seed).unwrap();

    if !git_ok(&origin, &["init", "--bare", "--initial-branch=develop"]) {
        eprintln!("#5228 tests: git unavailable, skipping");
        return;
    }
    assert!(git_ok(&seed, &["init", "--initial-branch=main"]));
    git_identity(&seed);
    // The abandoned `main`, which DOES carry a CLAUDE.md.
    fs::write(seed.join("CLAUDE.md"), UPSTREAM_CLAUDE_MD).unwrap();
    assert!(git_ok(&seed, &["add", "-A"]));
    assert!(git_ok(&seed, &["commit", "-m", "old main with CLAUDE.md"]));
    assert!(git_ok(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()]
    ));
    assert!(git_ok(&seed, &["push", "origin", "main"]));
    // The live default branch, which does not.
    assert!(git_ok(&seed, &["checkout", "--orphan", "develop"]));
    assert!(git_ok(&seed, &["rm", "-q", "-f", "CLAUDE.md"]));
    fs::write(seed.join("README.md"), "develop\n").unwrap();
    assert!(git_ok(&seed, &["add", "-A"]));
    assert!(git_ok(&seed, &["commit", "-m", "develop line"]));
    assert!(git_ok(&seed, &["push", "origin", "develop"]));
    assert!(git_ok(
        &origin,
        &["symbolic-ref", "HEAD", "refs/heads/develop"]
    ));

    assert!(git_ok(
        &root,
        &["clone", origin.to_str().unwrap(), work.to_str().unwrap()]
    ));
    git_identity(&work);
    assert!(!work.join("CLAUDE.md").exists());
    // Both authoritative signals answer, and neither carries the file.
    assert!(git_ok(
        &work,
        &["cat-file", "-e", "origin/main:./CLAUDE.md"]
    ));
    assert!(!git_ok(
        &work,
        &["cat-file", "-e", "origin/develop:./CLAUDE.md"]
    ));

    let path = work.join("CLAUDE.md");
    let (content, created) = load_or_create_claude_md(&path)
        .expect("an abandoned origin/main must not override origin/develop");
    assert!(created);
    assert_eq!(content, CLAUDE_MD_STUB);
}

#[test]
fn stale_claude_md_refusal_names_the_branch_ref_and_recovery() {
    // Why: the refusal stops a launch, so its message is the whole interface —
    // a bare "file missing" would send the operator hunting the wrong problem.
    let msg = stale_claude_md_message(
        Path::new("/w/proj/CLAUDE.md"),
        &UpstreamTracked {
            reference: "origin/main".to_string(),
            position: "branch `fix/4061-example`".to_string(),
            detached: false,
        },
    );
    assert!(msg.contains("CLAUDE.md"), "{msg}");
    assert!(msg.contains("fix/4061-example"), "{msg}");
    assert!(msg.contains("origin/main"), "{msg}");
    assert!(
        msg.contains("git -C /w/proj merge --ff-only origin/main"),
        "{msg}"
    );
    assert!(
        msg.contains("git -C /w/proj checkout origin/main -- CLAUDE.md"),
        "{msg}"
    );
}

#[test]
fn stale_claude_md_refusal_offers_no_merge_on_a_detached_head() {
    // Why: `merge --ff-only` on a detached HEAD moves HEAD and drops the commit
    // the operator deliberately checked out. Printing it as the headline remedy
    // would make the refusal's own advice destructive.
    let msg = stale_claude_md_message(
        Path::new("/w/proj/CLAUDE.md"),
        &UpstreamTracked {
            reference: "origin/main".to_string(),
            position: "detached HEAD at 1a2b3c4".to_string(),
            detached: true,
        },
    );
    assert!(msg.contains("detached HEAD at 1a2b3c4"), "{msg}");
    assert!(
        msg.contains("git -C /w/proj checkout origin/main -- CLAUDE.md"),
        "{msg}"
    );
    assert!(
        !msg.contains("merge --ff-only"),
        "a detached HEAD must never be told to fast-forward: {msg}"
    );
}

#[test]
fn strip_delegation_block_removes_legacy_block() {
    // Why (#2170 cleanup helper): a workspace polluted by the old #2125
    // injection must have the fenced block removed, leaving the
    // operator's own content untouched.
    let polluted = format!(
        "{DELEGATION_BLOCK_BEGIN}\n\nSome injected directive text.\n\n{DELEGATION_BLOCK_END}\n\n# My Project\n\nOperator notes.\n"
    );
    let cleaned = strip_delegation_block(&polluted);
    assert!(!cleaned.contains(DELEGATION_BLOCK_BEGIN));
    assert!(!cleaned.contains(DELEGATION_BLOCK_END));
    assert!(!cleaned.contains("Some injected directive text."));
    assert_eq!(cleaned, "# My Project\n\nOperator notes.\n");
}

#[test]
fn strip_delegation_block_noop_when_absent() {
    // Why: a clean CLAUDE.md (the common case post-#2170) must be
    // returned byte-identical.
    let clean = "# My Project\n\nOperator notes.\n";
    assert_eq!(strip_delegation_block(clean), clean);
}

#[test]
fn pm_instructions_is_its_three_sections() {
    // #4183: the legacy PM body is RECONSTITUTED, never kept as a fourth copy
    // on disk. #4318 moved the source of that reconstitution from the
    // `include_str!` constants to the MANIFEST, because the manifest may now
    // author a rule inline — rebuilding from the constants would deliver such a
    // rule to the packaged composer and not to the legacy override assembly,
    // which is the split-brain this asserts against.
    let body = pm_instructions();
    let projected = crate::core::bundled_pm_package::authored_run(&[
        SectionId::Core,
        SectionId::Memory,
        SectionId::Search,
    ])
    .expect("the manifest is readable");
    assert_eq!(body, format!("{projected}\n"));

    // Every section source is still delivered in full, in order, and the
    // manifest-authored rules ride along.
    for expected in [
        SECTION_CORE.trim(),
        SECTION_MEMORY.trim(),
        SECTION_SEARCH.trim(),
    ] {
        assert!(body.contains(expected), "a section source went missing");
    }
    assert!(body.contains("### Clickable References"));
    assert!(body.contains("### Banned Word"));

    // The paragraph break is `Join::Blank`'s literal, which is what makes
    // this string byte-identical to what the packaged composer emits.
    assert!(body.contains("## Memory Protocol (Context-First)"));
    assert!(body.contains("## Code Search Protocol (Context-First)"));
    assert!(
        !body.contains("## Context-First Protocol\n"),
        "the merged Memory+Search block must not survive as a fourth copy"
    );
}

#[test]
fn base_pm_is_its_four_sections() {
    // Same no-second-copy property for the non-overridable floor, plus the
    // one reordering #4183 makes: the tool-priority mandate travels with the
    // other non-overridable rules and so now precedes the conventions.
    //
    // #4573 added `enforcement` (Prohibitions + Circuit Breakers) as the second
    // floor section. Asserting it HERE — over `base_pm()`, the string every
    // legacy branch appends, including the `PM_INSTRUCTIONS_DEPLOYED.md` full
    // replacement — is what proves the tier change alone did not leave the
    // legacy paths without the authority tables.
    let floor = base_pm();
    assert_eq!(
        floor,
        format!(
            "{}\n\n{}\n\n{}\n\n{}\n",
            SECTION_IDENTITY.trim(),
            SECTION_ENFORCEMENT.trim(),
            SECTION_NON_OVERRIDABLE_RULES.trim(),
            SECTION_FRAMEWORK_CONVENTIONS.trim()
        )
    );

    let tool = floor
        .find("## Trusty Tool Priority (Non-Overridable)")
        .expect("the tool-priority mandate stays in the floor");
    let conventions = floor
        .find("## Framework-Guaranteed Conventions (Non-Overridable)")
        .expect("the guaranteed conventions stay in the floor");
    assert!(
        tool < conventions,
        "tool priority now rides with the non-overridable rules it belongs to"
    );
}

#[test]
fn workflow_section_carries_the_opportunistic_fix_rule() {
    // The opportunistic-fix rule (owner amendment on #4183) is authored in the
    // manifest, not in `workflow.md`, so the constant alone does not carry it.
    // Every consumer must therefore read the section through the manifest —
    // otherwise a project with no override and a project on the legacy path
    // would receive different workflows.
    let section = workflow_section();
    assert!(section.starts_with(WORKFLOW.trim()));
    assert!(section.contains("## Opportunistic Fixes"));
    assert!(section.contains("noted on the CURRENT issue"));
    assert!(
        !WORKFLOW.contains("## Opportunistic Fixes"),
        "the rule is manifest-authored; finding it in workflow.md means it was \
         duplicated"
    );
}

#[test]
fn delegation_doctrine_carries_the_precedence_note() {
    // #4318 moved the roster-precedence note out of a Rust literal and into the
    // manifest. The doctrine string every composer uses must still be the asset
    // followed by that note, in that order.
    let doctrine = delegation_doctrine();
    assert!(doctrine.starts_with(AGENT_DELEGATION.trim()));
    assert!(doctrine.ends_with("do not retry the same agent."));
    assert!(doctrine.contains("trust the roster"));
}

#[test]
fn the_installed_prompt_carries_the_manifest_authored_rules() {
    // `assemble_system_prompt` writes
    // `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`, which `tm install`
    // and `tm launch` hand to `claude --append-system-prompt-file`. It has no
    // agent roster, so it cannot use the package composer — which is exactly why
    // it has to project the manifest rather than concatenate the constants. A
    // manifest-authored rule missing here is a rule half the launch paths never
    // see.
    let prompt = assemble_system_prompt();
    for marker in [
        "### Clickable References",
        "### Banned Word",
        "## Opportunistic Fixes",
    ] {
        assert!(
            prompt.contains(marker),
            "the installed prompt must carry {marker:?}"
        );
    }
}

#[test]
fn assemble_system_prompt_contains_all_sections() {
    // Why: the assembled prompt is the contract `claude` receives; every
    // bundled section must be present and joined with the `---` rule.
    let prompt = assemble_system_prompt();
    assert!(prompt.contains("# PM Agent -- Trusty MPM"));
    assert!(prompt.contains("# Framework Instructions"));
    assert!(prompt.contains("# PM Workflow Configuration"));
    assert!(prompt.contains("# Agent Delegation Routing"));
    // The Trusty tool-priority block now lives inside the BASE_PM floor.
    assert!(prompt.contains("## Trusty Tool Priority (Non-Overridable)"));
    assert!(prompt.contains("\n\n---\n\n"));
    // BASE_PM is the non-overridable floor: it must come last.
    let base = prompt.find("# Framework Instructions").expect("base_pm");
    let delegation = prompt
        .find("# Agent Delegation Routing")
        .expect("delegation");
    assert!(base > delegation, "BASE_PM floor must be appended last");
    // Ticketing-specific content was stripped from the bundled assets.
    assert!(!prompt.contains("mcp__mcp-ticketer__"));
    assert!(!prompt.contains("ticketing_agent"));
}

// ── #4752: the compiled prompt owns a path nothing else writes ─────────────

#[test]
fn compiled_prompt_path_is_project_local() {
    // Why (#4752, then #4832): the compiled prompt belongs to the PROJECT whose
    // session runs it, under that session's own directory.
    // What: pins `<project>/.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`.
    // `/some/project` is not a git repo, so the harness root is the path itself.
    let project = Path::new("/some/project");
    assert_eq!(
        compiled_prompt_path(project, "sess-1"),
        Path::new("/some/project/.trusty-mpm/sessions/sess-1/INSTRUCTIONS-COMPILED.md")
    );
}

#[test]
fn compiled_prompt_path_is_per_session() {
    // Why (#4832): project-scoping still let two concurrent sessions in ONE
    // project overwrite each other's compiled prompt. FAILS BEFORE THIS CHANGE
    // — `compiled_prompt_path` took only the project and returned one path.
    let project = Path::new("/some/project");
    assert_ne!(
        compiled_prompt_path(project, "sess-a"),
        compiled_prompt_path(project, "sess-b"),
        "two sessions in one project must not share one compiled-prompt file"
    );
}

#[test]
fn compiled_prompt_path_never_collides_across_projects() {
    // Why (#4752 review, MEDIUM 4): ONE global file used to serve every
    // project, so concurrent sessions overwrote each other.
    let a = compiled_prompt_path(Path::new("/projects/alpha"), "s");
    let b = compiled_prompt_path(Path::new("/projects/beta"), "s");
    assert_ne!(
        a, b,
        "two projects must not share one compiled-prompt file — that was the collision"
    );
}

#[test]
fn compiled_prompt_path_never_lands_inside_a_worktree() {
    // Why (#4832, owner ruling): a worktree carries code, config and docs —
    // never harness state. FAILS BEFORE THIS CHANGE: the path was
    // `<worktree>/.trusty-mpm/framework/INSTRUCTIONS-COMPILED.md`.
    let tmp = crate::test_support::hermetic_temp_dir();
    let repo = tmp.path().join("proj");
    let wt = tmp.path().join("wt");
    init_git_repo_with_worktree(&repo, &wt);

    let compiled = compiled_prompt_path(&wt, "sess-1");
    let wt_canon = std::fs::canonicalize(&wt).unwrap();
    assert!(
        !compiled.starts_with(&wt_canon) && !compiled.starts_with(&wt),
        "the compiled prompt must not be written inside the worktree: {}",
        compiled.display()
    );
    assert!(
        compiled.starts_with(std::fs::canonicalize(&repo).unwrap()),
        "it belongs to the main checkout: {}",
        compiled.display()
    );
}

/// Build a committed repo at `repo` plus a linked worktree at `wt`.
///
/// Why: the worktree assertions above are only meaningful against a REAL git
/// worktree — a plain temp directory cannot reproduce the shape.
fn init_git_repo_with_worktree(repo: &Path, wt: &Path) {
    let run = |dir: &Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    fs::create_dir_all(repo).unwrap();
    run(repo, &["init", "-q", "-b", "main"]);
    run(repo, &["config", "user.email", "t@example.com"]);
    run(repo, &["config", "user.name", "T"]);
    fs::write(repo.join("README.md"), "# t\n").unwrap();
    run(repo, &["add", "-A"]);
    run(repo, &["commit", "-qm", "init"]);
    run(
        repo,
        &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
    );
}

#[test]
fn compiled_prompt_path_is_not_the_bundled_instructions_path() {
    // The original #4752 defect: the compiled OUTPUT must not share a path with
    // the bundled INPUT the pipeline used to read.
    let tmp = TempDir::new().unwrap();
    let paths = crate::core::paths::FrameworkPaths::under(tmp.path());
    assert_ne!(
        compiled_prompt_path(tmp.path(), "s"),
        paths.framework_instructions_path()
    );
}

// ── #4832: migration off the pre-#4832 per-project path ────────────────────

#[test]
fn migrate_removes_a_legacy_compiled_prompt() {
    // Why (#4832): an upgraded install keeps the old file forever with no
    // writer left to refresh it, and it is exactly the file an operator opens
    // to answer "what is my session running".
    let tmp = TempDir::new().unwrap();
    let legacy = tmp
        .path()
        .join(".trusty-mpm")
        .join("framework")
        .join(COMPILED_PROMPT_FILE);
    write_file(&legacy, "OLD COMPILED PROMPT");

    assert!(
        remove_legacy_compiled_prompt(tmp.path()).expect("removal succeeds"),
        "an existing legacy file must be reported as removed"
    );
    assert!(!legacy.exists());
    assert!(
        !legacy.parent().unwrap().exists(),
        "the now-empty framework/ directory goes with it"
    );
}

#[test]
fn migrate_keeps_a_sibling_manifest() {
    // The operator-authored `manifest.toml` now LIVES in `framework/`, so the
    // migration must never take the directory with it when it is non-empty.
    let tmp = TempDir::new().unwrap();
    let framework = tmp.path().join(".trusty-mpm").join("framework");
    write_file(&framework.join(COMPILED_PROMPT_FILE), "OLD");
    write_file(&framework.join("manifest.toml"), "[style]\n");

    remove_legacy_compiled_prompt(tmp.path()).expect("removal succeeds");
    assert!(
        framework.join("manifest.toml").exists(),
        "an operator's manifest override must survive the migration"
    );
}

#[test]
fn migrate_is_a_noop_when_absent() {
    let tmp = TempDir::new().unwrap();
    assert!(
        !remove_legacy_compiled_prompt(tmp.path()).expect("absence is not an error"),
        "nothing was removed, so the call must report false"
    );
}

#[test]
fn migrate_keeps_an_empty_framework_dir_when_nothing_migrated() {
    // #4841 review: `tm project init` seeds an empty `framework/` and this
    // migration runs on EVERY launch. Ungated, init created the directory and
    // the next launch deleted it.
    let tmp = TempDir::new().unwrap();
    let framework = tmp
        .path()
        .join(crate::core::harness_root::HARNESS_DIR)
        .join(crate::core::harness_root::FRAMEWORK_DIR);
    std::fs::create_dir_all(&framework).unwrap();

    assert!(
        !remove_legacy_compiled_prompt(tmp.path()).expect("absence is not an error"),
        "there was no legacy prompt to migrate"
    );
    assert!(
        framework.is_dir(),
        "a scaffolded, empty framework/ must survive a launch that migrated nothing"
    );
}

#[test]
fn instructions_failure_message_names_the_path_and_a_remedy() {
    // Why (#4752 ruling follow-up 1): the write is fatal, so a refused launch
    // must tell the operator what was refused, where, and what to do — not
    // surface a bare io error.
    let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
    let msg = instructions_failure_message(Path::new("/p/.trusty-mpm/framework/X.md"), &err);
    assert!(
        msg.contains("/p/.trusty-mpm/framework/X.md"),
        "must name the path: {msg}"
    );
    assert!(msg.contains("denied"), "must carry the cause: {msg}");
    assert!(
        msg.contains("was NOT started"),
        "must say the session did not start: {msg}"
    );
    assert!(msg.contains("permissions"), "must point at a remedy: {msg}");
}

#[test]
fn no_bundled_artifact_targets_the_compiled_prompt_path() {
    // Why (#4752): the defect was two writers on one path. This pins the
    // bundle-installer half — no entry in the canonical artifact table may
    // resolve to the compiled prompt's filename, at any depth, so the
    // `install_to` pass can never write there. A future artifact re-adding a
    // stub under that name turns this red instead of silently reintroducing
    // last-writer-wins.
    // What: scans `bundle::ALL` for any `rel_path` whose file name is
    // `COMPILED_PROMPT_FILE`.
    let clashing: Vec<&str> = crate::core::bundle::ALL
        .iter()
        .map(|a| a.rel_path)
        .filter(|p| Path::new(p).file_name().and_then(|n| n.to_str()) == Some(COMPILED_PROMPT_FILE))
        .collect();
    assert!(
        clashing.is_empty(),
        "no bundled artifact may target {COMPILED_PROMPT_FILE}; found {clashing:?}"
    );
}

#[test]
fn remove_stale_bundled_instructions_deletes_a_leftover() {
    // Why (#4752): a pre-#4752 install left the compiled prompt at
    // `instructions/INSTRUCTIONS.md`. Nothing writes it now but
    // `build_instructions` still reads it, so an upgraded machine would fold a
    // frozen old prompt into the pipeline forever. `tm install` removes it.
    let tmp = TempDir::new().unwrap();
    let paths = crate::core::paths::FrameworkPaths::under(tmp.path());
    let stale = paths.framework_instructions_path();
    write_file(&stale, "# an old compiled prompt\n");

    assert!(
        remove_stale_bundled_instructions(&stale).expect("removal succeeds"),
        "an existing leftover must be reported as removed"
    );
    assert!(!stale.exists(), "the stale leftover must be gone");
    // The compiled prompt is a DIFFERENT, project-local file.
    assert_ne!(stale, compiled_prompt_path(tmp.path(), "s"));
}

#[test]
fn remove_stale_bundled_instructions_is_a_noop_when_absent() {
    // Why: the common case is a fresh machine that never had the file. That
    // must not be an error, and must not report a removal.
    let tmp = TempDir::new().unwrap();
    let paths = crate::core::paths::FrameworkPaths::under(tmp.path());
    assert!(
        !remove_stale_bundled_instructions(&paths.framework_instructions_path())
            .expect("absence is not an error"),
        "nothing was removed, so the call must report false"
    );
}

#[test]
fn write_compiled_prompt_to_creates_parent_dirs() {
    // Why: the launch path calls this against a framework root that may not
    // exist yet on a machine that never ran `tm install`; a missing parent must
    // not fail the launch.
    // What: writes into a two-deep absent directory and reads the bytes back
    // verbatim.
    let tmp = TempDir::new().unwrap();
    let dest = tmp.path().join("a").join("b").join(COMPILED_PROMPT_FILE);
    write_compiled_prompt_to(&dest, "COMPILED-BODY").expect("write succeeds");
    assert_eq!(fs::read_to_string(&dest).unwrap(), "COMPILED-BODY");
}

#[test]
fn compiled_prompt_write_is_the_full_assembled_prompt_never_a_stub() {
    // Why (#383 regression guard, retained through #4752's path move): the
    // compiled file must only ever hold a FULL composed prompt. #383 was a
    // 4-line stub winning a last-writer race on the shared path; the path is
    // now project-local and single-writer, but the content contract still
    // needs pinning.
    // What: writes the bundled assembly to a project's compiled path and
    // asserts it is neither empty nor the historical stub.
    let tmp = TempDir::new().unwrap();
    let dest = compiled_prompt_path(tmp.path(), "s");
    write_compiled_prompt_to(&dest, &assemble_system_prompt()).expect("write succeeds");

    let on_disk = fs::read_to_string(&dest).unwrap();
    assert_eq!(on_disk, assemble_system_prompt());
    assert!(!on_disk.trim().is_empty());
    assert!(
        !on_disk.trim().eq("# trusty-mpm Framework Instructions\n\nThis Claude Code instance is managed by trusty-mpm.\nDaemon endpoint: ${TRUSTY_MPM_URL:-http://localhost:7799}"),
        "stub content must never be written — full assembled prompt required"
    );
}

#[test]
fn primary_directive_mandate_not_duplicated_across_channels() {
    // Issue #2647 (R2): the FULL mandate table (Prohibitions, Circuit
    // Breakers, Delegation Map, PM Allowlist) must live in exactly ONE
    // channel — the appended system prompt (`assemble_system_prompt`,
    // always injected via `--append-system-prompt-file` on tm-driven
    // spawns). Before this fix, every bundled output-style file (loaded
    // independently via the Claude Code `outputStyle` settings key) ALSO
    // carried a full, near-identical restatement of that table —
    // duplicated tokens every session.
    //
    // Code-critic follow-up: `outputStyle` is deployed to
    // `<project>/.claude/output-styles/*.md` + `settings.json`
    // (`settings.rs`) and therefore applies to ANY `claude` launched in
    // that directory — including a manual `claude` invocation that never
    // receives `--append-system-prompt-file` at all
    // (`runtime/claude_code.rs`). A style with NOTHING but a pointer to
    // the appended prompt would leave such a launch with zero
    // enforcement. So each style keeps a short, SELF-CONTAINED minimal
    // mandate (PRIMARY DIRECTIVE statement, override-phrase list, a
    // prohibition summary) alongside the pointer to the full table —
    // this test asserts that block survived the dedup, not just that the
    // heading sentinel is de-duplicated.
    const SENTINEL: &str = "PRIMARY DIRECTIVE";
    let assembled = assemble_system_prompt();
    assert_eq!(
        assembled.matches(SENTINEL).count(),
        0,
        "the appended system prompt must not carry a literal PRIMARY \
         DIRECTIVE banner — the mandate lives there as Identity + \
         Prohibitions content"
    );

    // The appended prompt must still carry the substantive, canonical
    // mandate content — so the mandate can never silently vanish from
    // BOTH channels at once just because the banner text moved.
    assert!(
        assembled.contains("## Prohibitions"),
        "the appended prompt must carry the canonical Prohibitions table"
    );
    assert!(
        assembled.contains("## Circuit Breakers"),
        "the appended prompt must carry the canonical Circuit Breakers table"
    );
    assert!(
        assembled.contains("don't delegate"),
        "the appended prompt must carry at least one override phrase"
    );

    for style in crate::core::bundle::OUTPUT_STYLES {
        let combined =
            assembled.matches(SENTINEL).count() + style.content.matches(SENTINEL).count();
        assert_eq!(
            combined, 1,
            "{}: PRIMARY DIRECTIVE sentinel must appear exactly once across \
             the appended prompt + this output style (one heading, never a \
             full restatement of the mandate TABLE) — got {combined}",
            style.id
        );

        // Each style must still carry its OWN self-contained minimal
        // mandate — the override-phrase list and a prohibition summary —
        // so a manual `claude` launch (no --append-system-prompt-file)
        // in a tm-provisioned workspace is never left with zero
        // enforcement.
        assert!(
            style.content.contains("do this yourself"),
            "{}: must carry its own self-contained override-phrase list",
            style.id
        );
        assert!(
            style.content.contains("Minimum prohibitions"),
            "{}: must carry its own self-contained prohibition summary",
            style.id
        );
    }
}

#[serial_test::serial]
#[test]
fn pipeline_no_agents_still_succeeds() {
    // Every tier empty yields a zero agent_count, and the delegation section
    // the PM receives says so. #4832: the section is asserted through the
    // resolver that renders it, since the pipeline no longer returns text.
    let tiers = RosterTiers::new();
    let input = input_in(&tiers);

    let out = build_instructions(&input).unwrap();
    assert_eq!(out.agent_count, 0);
    let authority = crate::core::delegation_authority::generate_authority(&[]);
    assert!(authority.to_lowercase().contains("no delegatable agents"));
}

#[serial_test::serial]
#[test]
fn session_start_count_matches_the_delivered_delegation_roster() {
    // THE #4588 GATE. `tm session start` prints
    // `Instructions: {agent_count} agents in delegation authority` straight
    // from `PipelineOutput::agent_count`, while the roster the PM actually
    // receives is rendered by `deployed_roster_section`. Those were two
    // independent resolutions — a single-directory scan versus a three-tier
    // union — so the printed number understated the delivered roster (34 vs 39
    // measured live on tm 1.3.1). Both must now come from ONE resolver, which
    // is what this asserts: not that the number is plausible, but that it is
    // the SAME number, computed the same way, over a roster spanning all three
    // tiers with a deliberate cross-tier duplicate.
    let tiers = RosterTiers::new();

    write_roster_agent(&tiers.managed_tier(), "engineer");
    write_roster_agent(&tiers.managed_tier(), "qa");
    write_roster_agent(&tiers.generic_tier(), "writer");
    write_roster_agent(&tiers.project_tier(), "ticketing");
    // Deployed in two tiers at once — the union must advertise it exactly once,
    // so a naive concatenation cannot pass this test either.
    write_roster_agent(&tiers.generic_tier(), "qa");

    let input = PipelineInput {
        project_dir: tiers.project(),
        claude_md_path: tiers.project().join("CLAUDE.md"),
    };

    let out = build_instructions(&input).expect("pipeline");
    let delivered = crate::core::delegation_authority::deployed_roster_section(&tiers.project())
        .expect("a roster is deployed in three tiers, so a section must render");
    let delivered_entries = delivered.matches("\n### ").count();

    assert_eq!(
        out.agent_count, delivered_entries,
        "the count `tm session start` prints must equal the number of agents \
         the PM was actually given (#4588)\nprinted: {}\ndelivered roster:\n{delivered}",
        out.agent_count
    );
    assert_eq!(
        out.agent_count, 4,
        "engineer + qa + writer + ticketing, with the cross-tier `qa` counted once"
    );
}

// ── #4752 round 4: the shared fatal compiled-prompt refresh ─────────────────

#[test]
fn refresh_compiled_prompt_writes_the_project_local_file() {
    // Why (#4752 round 4): this is the single entry point all THREE pre-spawn
    // paths now route through — fresh start, daemon resume, and the bare-`tm`
    // in-place relaunch that round 3 missed entirely. Pinning it directly means
    // the guarantee is asserted on the implementation, not only through the
    // daemon's thin `refresh_resume_compiled_prompt` delegate.
    //
    // FIXTURE: a stale sentinel is pre-seeded so "the file exists" cannot pass;
    // only a real refresh overwrites it.
    let tmp = TempDir::new().expect("tempdir");
    let dest = compiled_prompt_path(tmp.path(), "sess-1");
    fs::create_dir_all(dest.parent().expect("has a parent")).expect("create parent");
    const STALE: &str = "STALE-FROM-A-PREVIOUS-LAUNCH";
    fs::write(&dest, STALE).expect("seed stale");

    refresh_compiled_prompt(tmp.path(), "sess-1").expect("refresh must succeed");

    let on_disk = fs::read_to_string(&dest).expect("readable");
    assert_ne!(
        on_disk, STALE,
        "a stale compiled prompt must be overwritten"
    );
    assert!(
        on_disk.contains("# PM Agent -- Trusty MPM"),
        "must hold the resolved PM prompt, not a fragment: {on_disk}"
    );
    assert!(
        dest.starts_with(tmp.path()),
        "the compiled prompt is PROJECT-LOCAL; a global path would restore the \
         cross-project collision #4752 removed"
    );
}

#[test]
fn refresh_compiled_prompt_reports_an_actionable_failure() {
    // Why (#4752): the write is fatal at all three call sites, so a refusal is
    // operator-facing — it must name the path and say the session did not
    // start, never surface a bare io error.
    //
    // FIXTURE: a directory planted at the exact destination, so only this write
    // fails and nothing else in the compose path does.
    let tmp = TempDir::new().expect("tempdir");
    let dest = compiled_prompt_path(tmp.path(), "sess-1");
    fs::create_dir_all(&dest).expect("plant a directory at the compiled path");

    let msg = refresh_compiled_prompt(tmp.path(), "sess-1")
        .expect_err("a failed compiled write must be reported as an error");
    assert!(
        msg.contains(&dest.display().to_string()),
        "must name the path: {msg}"
    );
    assert!(
        msg.contains("was NOT started"),
        "must say the session did not start: {msg}"
    );
    assert!(msg.contains("permissions"), "must point at a remedy: {msg}");
}
