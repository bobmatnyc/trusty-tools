//! Tests for [`super`] — the `bundled_asset_lag` doctor row (#8482).
//!
//! Why: this row is the only surface that can see a binary whose embedded
//! assets lag the repo, so both of its dishonest outcomes have to be pinned:
//! a false `Ok` (the Fail-Open Check — an unreadable source must never read as
//! clean) and a false fire in a foreign repo, where the comparison is
//! meaningless. The skip arm is proved with a reader that panics if called, so
//! "no git command runs against `origin/main`" is an assertion rather than a
//! claim.
//! What: real temp git repos seeded from the live [`bundle::ALL`] table — no
//! mocked git — plus two fold-level cases driven through [`super::report`].
//! Test: this file IS the test module.

use super::*;

/// The identity gate's own input, built by hand.
fn identity(owner: &str, repo: &str) -> GithubPath {
    GithubPath {
        owner: owner.to_string(),
        repo: repo.to_string(),
    }
}

/// Run one git command in `dir`, returning whether it succeeded.
fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A temp repo whose `crates/trusty-mpm/src/assets/skills` tree is committed at
/// `origin/main` and whose `origin` remote is `bobmatnyc/trusty-tools`.
///
/// `mutate` gets the chance to edit the tree before it is committed, so one
/// helper serves the matching, the drifted, and the extra-file cases. Returns
/// `None` when `git` is unavailable, which keeps the suite runnable on a host
/// without it.
fn seeded_repo(mutate: impl Fn(&Path)) -> Option<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = crate::test_support::hermetic_temp_dir();
    let path = dir.path().to_path_buf();
    if !git(&path, &["init", "-q", "-b", "main"]) {
        return None;
    }
    git(&path, &["config", "user.email", "t@example.invalid"]);
    git(&path, &["config", "user.name", "test"]);
    git(&path, &["config", "commit.gpgsign", "false"]);
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/bobmatnyc/trusty-tools.git",
        ],
    );

    let assets = path.join(ASSET_DIR);
    for artifact in bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("skills/"))
    {
        let key = artifact.rel_path.trim_start_matches("skills/");
        let dest = assets.join(key);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).expect("create asset parent");
        }
        std::fs::write(&dest, artifact.contents).expect("write asset");
    }
    mutate(&path);

    if !git(&path, &["add", "-A", "-f"]) || !git(&path, &["commit", "-q", "-m", "seed"]) {
        return None;
    }
    if !git(&path, &["update-ref", "refs/remotes/origin/main", "HEAD"]) {
        return None;
    }
    Some((dir, path))
}

/// The key this repo's `tm-epic` procedure reference lives under — the exact
/// file whose nine-hour lag (#8482) motivated the row.
const DRIFT_KEY: &str = "tm-epic/references/manual-procedure.md";

/// A tree identical to what this binary embeds must PASS.
///
/// Why: the control for every Warn case below. Without it, a row that warned
/// unconditionally would look correct.
#[test]
fn lag_passes_when_every_asset_matches() {
    let Some((_guard, repo)) = seeded_repo(|_| {}) else {
        return;
    };
    let check = check_bundled_asset_lag(Some(&repo));
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("match `origin/main`"),
        "{}",
        check.message
    );
}

/// One asset differing from the binary's embedded copy must WARN and NAME it.
///
/// Why: this is the #8482 defect in its measured shape — one merged edit to
/// `tm-epic/references/manual-procedure.md` that the installed binary kept
/// overwriting with pre-fix text for nine hours. Naming the file is the whole
/// value: "something drifted" does not tell an operator what is being reverted.
#[test]
fn lag_warns_and_names_the_one_differing_asset() {
    let Some((_guard, repo)) = seeded_repo(|root| {
        let target = root.join(ASSET_DIR).join(DRIFT_KEY);
        assert!(target.is_file(), "fixture must seed {DRIFT_KEY}");
        std::fs::write(
            &target,
            "the text that merged after this binary was built\n",
        )
        .expect("mutate one asset");
    }) else {
        return;
    };
    let check = check_bundled_asset_lag(Some(&repo));
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains(DRIFT_KEY),
        "the warning must name the lagging file; got: {}",
        check.message
    );
    assert!(
        check.message.contains("cargo install --path"),
        "the warning must carry the remedy; got: {}",
        check.message
    );
}

/// An asset the repo has and the binary does not embed is lag too.
///
/// Why: the measured incident was an EDIT, but a skill added to `main` after
/// this build is the same defect — the binary deploys a tree the repo has moved
/// past. A comparison folded only over bundle keys would report that clean.
#[test]
fn lag_warns_for_an_asset_the_binary_does_not_embed() {
    let Some((_guard, repo)) = seeded_repo(|root| {
        std::fs::write(
            root.join(ASSET_DIR).join("skill-added-after-this-build.md"),
            "new on main\n",
        )
        .expect("write the added asset");
    }) else {
        return;
    };
    let check = check_bundled_asset_lag(Some(&repo));
    assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
    assert!(
        check.message.contains("skill-added-after-this-build.md"),
        "{}",
        check.message
    );
}

/// A repo that is not `bobmatnyc/trusty-tools` must SKIP — and run no git.
///
/// Why: the comparison is meaningless in any other checkout, and firing there
/// would be pure noise. The reader panics if called, so this also proves no git
/// command is issued against `origin/main` on the skip path.
#[test]
fn lag_skips_without_the_trusty_tools_remote() {
    let other = identity("someone-else", "some-project");
    let check = report(Some(&other), &|| {
        unreachable!("a foreign repo must not be read at all")
    });
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("not applicable"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("someone-else/some-project"),
        "the skip must say what it saw; got: {}",
        check.message
    );
}

/// No `origin` remote at all is the same skip, with the same no-git guarantee.
#[test]
fn lag_skips_without_any_remote() {
    let check = report(None, &|| {
        unreachable!("an unidentifiable repo must not be read at all")
    });
    assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("no `origin` remote"),
        "{}",
        check.message
    );
}

/// THE FAIL-OPEN CHECK: a source tree that cannot be read is UNKNOWN, never Ok.
///
/// Why: a staleness detector that passes when it cannot read the source is
/// worse than no detector, because it converts "unknown" into "fine". `Unknown`
/// ranks above `Warn` in [`CheckStatus`], so it can never render as healthy.
#[test]
fn lag_is_unknown_when_the_source_is_unreadable() {
    let tt = identity(REPO_OWNER, REPO_NAME);
    let check = report(Some(&tt), &|| {
        RepoAssets::Unreadable("the fixture refused the read".to_string())
    });
    assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    assert_ne!(check.status, CheckStatus::Ok, "{}", check.message);
    assert!(
        check.message.contains("the fixture refused the read"),
        "the Unknown must carry WHY; got: {}",
        check.message
    );
}

/// The same arm reached through the real git path: the ref does not exist.
///
/// Why: `lag_is_unknown_when_the_source_is_unreadable` proves the fold; this
/// proves [`read_repo_assets`] actually produces `Unreadable` for the most
/// likely real cause — a checkout that has never fetched, or has no network.
#[test]
fn lag_is_unknown_when_origin_main_is_absent() {
    let dir = crate::test_support::hermetic_temp_dir();
    let path = dir.path().to_path_buf();
    if !git(&path, &["init", "-q", "-b", "main"]) {
        return;
    }
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/bobmatnyc/trusty-tools.git",
        ],
    );

    let check = check_bundled_asset_lag(Some(&path));
    assert_eq!(check.status, CheckStatus::Unknown, "{}", check.message);
    assert!(
        check.message.contains("git fetch origin"),
        "the Unknown must name the remedy for an unfetched checkout; got: {}",
        check.message
    );
}

/// Not a git repo at all: [`read_repo_assets`] reports why, and never panics.
#[test]
fn lag_is_unknown_outside_a_git_repo() {
    let dir = crate::test_support::hermetic_temp_dir();
    match read_repo_assets(dir.path()) {
        RepoAssets::Unreadable(why) => assert!(!why.is_empty(), "the reason must be stated"),
        RepoAssets::Read { .. } => panic!("a non-repo directory must not read as a source tree"),
    }
}

/// The reader returns one hash per committed asset, keyed the way the bundle is.
#[test]
fn lag_reads_the_committed_tree() {
    let Some((_guard, repo)) = seeded_repo(|_| {}) else {
        return;
    };
    match read_repo_assets(&repo) {
        RepoAssets::Read { hashes, .. } => {
            assert_eq!(
                hashes.len(),
                bundled_key_stamps().len(),
                "every seeded asset must come back"
            );
            assert!(
                hashes.contains_key(DRIFT_KEY),
                "keys are bundle keys, not repo paths"
            );
        }
        RepoAssets::Unreadable(why) => panic!("the seeded repo must be readable: {why}"),
    }
}

/// The per-key stamps cover exactly the bundle's `skills/*` entries.
///
/// Why: a key set that silently shrank would make the row report clean over a
/// subset of the assets it claims to audit.
#[test]
fn bundled_key_stamps_covers_every_skill_entry() {
    let expected = bundle::ALL
        .iter()
        .filter(|a| a.rel_path.starts_with("skills/"))
        .count();
    let stamps = bundled_key_stamps();
    assert_eq!(stamps.len(), expected);
    assert!(expected > 0, "the bundle must embed skills at all");
    // The construction is `skill_bundle_stamp`'s own, per entry.
    let sample = bundle::ALL
        .iter()
        .find(|a| a.rel_path.starts_with("skills/"))
        .expect("at least one skill entry");
    let key = sample.rel_path.trim_start_matches("skills/");
    assert_eq!(
        stamps.get(key),
        Some(&asset_stamp(sample.rel_path, sample.contents))
    );
}

/// The build id renders as an instant an operator can compare against a commit.
#[test]
fn build_timestamp_is_an_iso_instant() {
    let rendered = build_timestamp();
    assert!(
        rendered.ends_with('Z') && rendered.contains('T'),
        "expected an ISO-8601 UTC instant, got {rendered}"
    );
}
