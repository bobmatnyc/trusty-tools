//! A workspace holding only what a real spawn provisioned passes both removal
//! checks; anything else still keeps it (#11221).
//!
//! Why: a spawn writes `.trusty-mpm/sessions/<id>/INSTRUCTIONS-COMPILED.md`
//! under a `.trusty-mpm/sessions/` exclude, and `CLAUDE.md`, which a project
//! may gitignore. The `.trusty-mpm/` dirt pass read the collapsed
//! `!! .trusty-mpm/sessions/` line as work, and the gitignored-output check
//! read `!! CLAUDE.md` as run output, so every freshly spawned workspace was
//! refused by decommission, `--force` and the project-dir delete.
//! What: a real clone, provisioned by the real launch preparation under a
//! hermetic home, then the two checks, directly and through
//! `owned_workspace_keep_reason`.
//! Test: this file IS the test module.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::decommission_force::ProvisioningDirt;
use super::decommission_owned::owned_workspace_keep_reason;
use super::provisioning_ledger::{LEDGER_NAME, record_compiled_prompt};
use super::provisioning_ledger_fifo_tests::{answers_in_time, mkfifo};
use super::record::ManagedSessionId;
use super::worktree_git_fixture::GitWorktreeFixture;
use super::worktree_ignored_output::{ignored_output_refusal, kept_ignored_output};
use super::worktree_safety::{DirtyWorktree, count_dirty_files, inspect_dirt_excusing};

/// Run `git -C <dir> <args>` and assert it succeeded.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A clean, pushed clone of the fixture repo whose tracked `.gitignore` is
/// `gitignore`, at `<repos_root>/owner/clones/<name>`.
fn clone(fx: &GitWorktreeFixture, name: &str, gitignore: &str) -> PathBuf {
    std::fs::write(fx.repo.join(".gitignore"), gitignore).expect("write .gitignore");
    git(&fx.repo, &["add", ".gitignore"]);
    git(&fx.repo, &["commit", "-q", "-m", "track .gitignore"]);
    let ws = fx.repos_root.join("owner").join("clones").join(name);
    std::fs::create_dir_all(ws.parent().expect("parent")).expect("mkdir clones");
    let (src, dst) = (fx.repo.to_string_lossy(), ws.to_string_lossy());
    git(&fx.repos_root, &["clone", "-q", &src, &dst]);
    ws
}

/// Provision `ws` with the launch preparation a managed spawn runs, under a
/// hermetic home that must outlive the test.
fn spawn(ws: &Path) -> tempfile::TempDir {
    let home = crate::test_support::hermetic_temp_dir();
    let fw = crate::core::paths::FrameworkPaths::for_managed_workspace_under(home.path(), ws);
    crate::core::session_launch::prepare_session_with_home(&fw, ws, None, true, Some(home.path()))
        .expect("the spawn preparation succeeds");
    home
}

/// The compiled prompt the spawn wrote under `ws/.trusty-mpm/sessions/`.
fn compiled_prompt(ws: &Path) -> PathBuf {
    let sessions = ws.join(".trusty-mpm").join("sessions");
    let found: Vec<PathBuf> = std::fs::read_dir(&sessions)
        .expect("the spawn wrote .trusty-mpm/sessions/")
        .map(|e| e.expect("entry").path().join("INSTRUCTIONS-COMPILED.md"))
        .filter(|p| p.is_file())
        .collect();
    assert_eq!(found.len(), 1, "one compiled prompt: {found:?}");
    found[0].clone()
}

/// Why a plain (`Refuse`) or `--force` (`Discard`) decommission keeps `ws`.
fn keep(ws: &Path, policy: ProvisioningDirt) -> Option<String> {
    owned_workspace_keep_reason(ws, &ManagedSessionId::new(), None, policy)
}

/// The dirt check with every entry outside `.trusty-mpm/` excused, so only
/// the `.trusty-mpm/` pass (and the commit and nested checks) can answer.
fn trusty_mpm_dirt_only(ws: &Path) -> Option<DirtyWorktree> {
    inspect_dirt_excusing(ws, &|_: &str| true)
}

/// Append `text` to the file at `path`.
fn append(path: &Path, text: &str) {
    let mut body = std::fs::read_to_string(path).expect("read");
    body.push_str(text);
    std::fs::write(path, body).expect("write");
}

/// 🔴 #11221 check 1: the `.trusty-mpm/` dirt pass counted the spawn's own
/// compiled prompt, so decommission and `--force` kept the clone.
#[test]
fn a_spawn_provisioned_clone_passes_the_dirt_check_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "spawned", "target/\n");
    let _home = spawn(&ws);
    // Premise: the spawn wrote the compiled prompt and excluded its directory.
    let _ = compiled_prompt(&ws);
    let exclude = std::fs::read_to_string(ws.join(".git/info/exclude")).expect("info/exclude");
    assert!(exclude.contains(".trusty-mpm/sessions/"), "{exclude}");

    assert!(
        trusty_mpm_dirt_only(&ws).is_none(),
        "{:?}",
        trusty_mpm_dirt_only(&ws)
    );
    assert_eq!(keep(&ws, ProvisioningDirt::Refuse), None);
    assert_eq!(keep(&ws, ProvisioningDirt::Discard), None);
}

/// 🔴 #11221 check 2: a project that gitignores `CLAUDE.md` saw the spawn's
/// own `CLAUDE.md` refused as gitignored output that is not build output.
#[test]
fn a_gitignored_claude_md_tm_wrote_passes_the_ignored_output_check_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "ignores-claude-md", "target/\nCLAUDE.md\n");
    let _home = spawn(&ws);
    assert!(
        ws.join("CLAUDE.md").is_file(),
        "premise: the spawn wrote CLAUDE.md"
    );

    assert_eq!(ignored_output_refusal(&ws), None);
    assert_eq!(keep(&ws, ProvisioningDirt::Refuse), None);
}

/// #11221: a user file placed inside `.trusty-mpm/` still counts.
#[test]
fn a_user_file_under_trusty_mpm_still_keeps_the_clone_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "user-file", "target/\n");
    let _home = spawn(&ws);
    let notes = compiled_prompt(&ws).with_file_name("notes.md");
    std::fs::write(&notes, "my findings\n").expect("write notes");

    let dirt = trusty_mpm_dirt_only(&ws).expect("a user file is work");
    assert_eq!(dirt.dirty_files, 1, "{}", dirt.reason);
    assert!(keep(&ws, ProvisioningDirt::Discard).is_some());
    assert!(notes.is_file(), "nothing was deleted");
}

/// #11221: a compiled prompt edited after the spawn wrote it still counts.
#[test]
fn an_edited_compiled_prompt_still_keeps_the_clone_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "edited-prompt", "target/\n");
    let _home = spawn(&ws);
    append(&compiled_prompt(&ws), "\nan agent's note\n");

    let dirt = trusty_mpm_dirt_only(&ws).expect("an edited compiled prompt is work");
    assert_eq!(dirt.dirty_files, 1, "{}", dirt.reason);
    assert!(keep(&ws, ProvisioningDirt::Discard).is_some());
}

/// #11221 fail-safe: with the ledger missing, or unreadable as JSON, the
/// compiled prompt is counted again.
#[test]
fn a_missing_ledger_keeps_the_compiled_prompt_counted_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "no-ledger", "target/\n");
    let _home = spawn(&ws);
    let ledger = ws.join(".git").join(LEDGER_NAME);
    assert!(ledger.is_file(), "premise: the spawn recorded a ledger");

    std::fs::write(&ledger, "not json").expect("corrupt the ledger");
    let corrupt = trusty_mpm_dirt_only(&ws).expect("a corrupt ledger vouches for nothing");
    assert_eq!(corrupt.dirty_files, 1, "{}", corrupt.reason);

    std::fs::remove_file(&ledger).expect("remove the ledger");
    let missing = trusty_mpm_dirt_only(&ws).expect("no ledger vouches for nothing");
    assert_eq!(missing.dirty_files, 1, "{}", missing.reason);
    assert!(keep(&ws, ProvisioningDirt::Discard).is_some());
}

/// #11221 fail-safe: a directory under `.trusty-mpm/` git cannot open is a
/// failed check, never an absent one.
#[test]
fn an_unreadable_dir_under_trusty_mpm_keeps_the_clone_11221() {
    use std::os::unix::fs::PermissionsExt;
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "unreadable", "target/\n");
    let _home = spawn(&ws);
    let scope = compiled_prompt(&ws)
        .parent()
        .expect("scope dir")
        .to_path_buf();
    std::fs::write(scope.join("notes.md"), "my findings\n").expect("write notes");
    std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o000)).expect("chmod");

    let dirt = trusty_mpm_dirt_only(&ws);
    let kept = keep(&ws, ProvisioningDirt::Discard);

    std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o755)).expect("chmod back");
    let dirt = dirt.expect("an unreadable directory cannot be counted");
    assert!(
        dirt.reason.contains("dirty-check failed"),
        "{}",
        dirt.reason
    );
    assert!(kept.is_some());
}

/// #11221: a gitignored `CLAUDE.md` edited after the spawn still refuses.
#[test]
fn an_edited_gitignored_claude_md_still_keeps_the_clone_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "edited-claude-md", "target/\nCLAUDE.md\n");
    let _home = spawn(&ws);
    append(&ws.join("CLAUDE.md"), "\nmy own notes\n");

    let refusal = ignored_output_refusal(&ws).expect("an edited CLAUDE.md is kept");
    assert!(refusal.contains("CLAUDE.md"), "{refusal}");
    assert!(keep(&ws, ProvisioningDirt::Discard).is_some());
}

/// #11221: a gitignored file outside the provisioned set still refuses.
#[test]
fn a_gitignored_user_note_still_keeps_the_clone_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "user-note", "target/\nCLAUDE.md\nnotes.txt\n");
    let _home = spawn(&ws);
    std::fs::write(ws.join("notes.txt"), "mine\n").expect("write notes");

    let refusal = ignored_output_refusal(&ws).expect("a user note is kept");
    assert!(refusal.contains("notes.txt"), "{refusal}");
    assert!(keep(&ws, ProvisioningDirt::Discard).is_some());
}

/// #11221 fail-safe: with no ledger the gitignored `CLAUDE.md` refuses again.
#[test]
fn a_missing_ledger_keeps_a_gitignored_claude_md_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "claude-md-no-ledger", "target/\nCLAUDE.md\n");
    let _home = spawn(&ws);
    std::fs::remove_file(ws.join(".git").join(LEDGER_NAME)).expect("remove the ledger");

    let refusal = ignored_output_refusal(&ws).expect("no ledger vouches for CLAUDE.md");
    assert!(refusal.contains("CLAUDE.md"), "{refusal}");
}

/// Set `dir`'s mode bits.
fn chmod(dir: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// 🔴 #11221 round 2: git reports a directory it cannot open only as a stderr
/// warning on exit 0, so the gitignored file inside read as absent and the
/// ignored-output check answered "nothing kept".
#[test]
fn an_unreadable_dir_holding_a_gitignored_file_refuses_removal_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "unreadable-ignored", "target/\nsecret.env\n");
    let sub = ws.join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    std::fs::write(sub.join("secret.env"), "TOKEN=mine\n").expect("write secret.env");
    chmod(&sub, 0o000);

    let checked = kept_ignored_output(&ws);
    let refusal = ignored_output_refusal(&ws);

    chmod(&sub, 0o755);
    let err = checked.expect_err("an unreadable directory is a failed check");
    assert!(err.contains("sub/"), "{err}");
    let refusal = refusal.expect("a failed check refuses removal");
    assert!(refusal.contains("check failed"), "{refusal}");
}

/// 🔴 #11221 round 2: the same warning made pass 1 of the dirt count read an
/// untracked directory it cannot open as zero files.
#[test]
fn a_git_warning_fails_the_pass_one_count_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "unreadable-untracked", "target/\n");
    let sub = ws.join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    std::fs::write(sub.join("findings.md"), "mine\n").expect("write findings");
    chmod(&sub, 0o000);

    let counted = count_dirty_files(&ws);

    chmod(&sub, 0o755);
    let err = counted.expect_err("a git warning is a failed count, not 0");
    assert!(err.contains("sub/"), "{err}");
}

/// 🔴 #11221 round 2: a FIFO at the ledger path blocked every removal gate
/// and the launch-time record. Each must answer at once and vouch for
/// nothing; a relaunch replaces the FIFO with a regular ledger.
#[test]
fn a_fifo_ledger_vouches_for_nothing_and_does_not_block_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "fifo-ledger", "target/\nCLAUDE.md\n");
    let _home = spawn(&ws);
    let prompt = compiled_prompt(&ws);
    let ledger = ws.join(".git").join(LEDGER_NAME);
    std::fs::remove_file(&ledger).expect("remove the ledger");
    mkfifo(&ledger);

    let fifos = [ledger.clone()];
    let w = ws.clone();
    let answer = answers_in_time(&fifos, move || {
        let dirt = trusty_mpm_dirt_only(&w).map(|d| d.dirty_files);
        let refusal = ignored_output_refusal(&w);
        let recorded = record_compiled_prompt(&prompt, b"prompt").map_err(|e| e.to_string());
        (dirt, refusal, recorded)
    });
    let (dirt, refusal, recorded) = answer.expect("a FIFO ledger blocked a check or the record");
    assert_eq!(dirt, Some(1), "the compiled prompt is counted");
    assert!(
        refusal.as_deref().is_some_and(|r| r.contains("CLAUDE.md")),
        "{refusal:?}"
    );
    assert_eq!(recorded, Ok(false), "nothing is recorded into a FIFO");

    let w = ws.clone();
    let relaunched = answers_in_time(&fifos, move || spawn(&w));
    assert!(
        relaunched.is_some(),
        "a relaunch blocked on the FIFO ledger"
    );
    let meta = std::fs::symlink_metadata(&ledger).expect("ledger");
    assert!(meta.is_file(), "the relaunch wrote a regular ledger");
}

/// 🔴 #11221 round 2: a symlink at the ledger path was followed, so a ledger
/// outside the admin dir vouched for the tree.
#[test]
fn a_symlinked_ledger_is_not_followed_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "symlink-ledger", "target/\nCLAUDE.md\n");
    let _home = spawn(&ws);
    let prompt = compiled_prompt(&ws);
    let ledger = ws.join(".git").join(LEDGER_NAME);
    let real = ws.join(".git").join("ledger-elsewhere.json");
    std::fs::rename(&ledger, &real).expect("move the ledger");
    std::os::unix::fs::symlink(&real, &ledger).expect("symlink the ledger");

    let dirt = trusty_mpm_dirt_only(&ws).expect("a symlinked ledger vouches for nothing");
    assert_eq!(dirt.dirty_files, 1, "{}", dirt.reason);
    let refusal = ignored_output_refusal(&ws).expect("CLAUDE.md is not vouched for");
    assert!(refusal.contains("CLAUDE.md"), "{refusal}");
    let recorded = record_compiled_prompt(&prompt, b"prompt").map_err(|e| e.to_string());
    assert_eq!(recorded, Ok(false), "nothing is recorded through a symlink");
    let meta = std::fs::symlink_metadata(&ledger).expect("ledger");
    assert!(
        meta.file_type().is_symlink(),
        "the symlink was not replaced"
    );
}

/// #11221 round 2: a corrupt ledger vouches for no gitignored `CLAUDE.md`.
#[test]
fn a_corrupt_ledger_keeps_a_gitignored_claude_md_11221() {
    let fx = GitWorktreeFixture::new();
    let ws = clone(&fx, "claude-md-corrupt-ledger", "target/\nCLAUDE.md\n");
    let _home = spawn(&ws);
    std::fs::write(ws.join(".git").join(LEDGER_NAME), "not json").expect("corrupt the ledger");

    let refusal = ignored_output_refusal(&ws).expect("a corrupt ledger vouches for nothing");
    assert!(refusal.contains("CLAUDE.md"), "{refusal}");
}
