//! Project-identity derivation for the `tctl ensure` project-setup stages.
//!
//! Why: the index-register and palace-create stages need two project-scoped
//! names: the trusty-search index id and the trusty-memory palace name. Both
//! must be deterministic so re-running `ensure` targets the same index and
//! palace. The palace name must also be the one every reader of the palace
//! resolves — `tm` launch, the memory hook, `tm memory` — or `ensure` creates a
//! palace nobody reads (#11823).
//!
//! What: [`index_id_for`] (trusty-search index id = directory basename, matching
//! `trusty-search`'s own `resolve_index_name` default) and [`palace_name_for`],
//! a thin wrapper over `trusty_common::resolve_palace`. This module keeps no
//! palace-naming rule of its own: no pin parsing, no folder-slug fallback.
//!
//! Test: `tests` cover the index id and the resolver's pin, `owner-repo`,
//! parent-dir and fail-closed pin cases as `ensure` sees them; the full rule is
//! pinned in `trusty-common`'s `palace_resolve_tests.rs`.

use std::path::Path;

use trusty_common::PalaceCreatePlanError;

/// Derive the trusty-search index id for a project directory.
///
/// Why: `ensure` registers the project under the same id `trusty-search index`
/// would use by default, so the two agree and re-registration is a no-op. That
/// default is the directory basename (`resolve_index_name`'s final fallback).
/// What: returns the final path component of `project_root` as a `String`.
/// Returns `None` when the path has no final component (filesystem root).
/// Test: `tests::index_id_is_basename`, `tests::index_id_root_is_none`.
pub fn index_id_for(project_root: &Path) -> Option<String> {
    project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

/// Name the project's trusty-memory palace through the shared resolver.
///
/// Why (#11823): `ensure` used to read the pin itself and fall back to the
/// folder slug, so for `Acme/My_Repo` it created `my-repo` while every reader
/// looked for `acme-my-repo`; and a malformed pin was skipped silently, naming
/// a palace nobody chose. One resolver is the only rule.
/// What: returns `resolve_palace_for_project(project_root)`'s id — the pin,
/// then git `owner-repo`, then the main worktree's `parent-dir`. It ignores
/// `TRUSTY_MEMORY_PALACE`, exactly as the daemon does when it validates the
/// create, so the two agree even when the variable is exported. A pin that
/// exists but cannot be trusted (unparsable, empty, invalid id, newer schema),
/// or a root outside any project, is the resolver's error, returned unchanged;
/// there is no fallback name.
/// Test: `tests::palace_name_for_reads_pin`,
/// `tests::palace_name_for_uses_owner_repo_from_the_git_remote`,
/// `tests::palace_name_for_without_a_remote_uses_the_parent_dir_slug`,
/// `tests::palace_name_for_rejects_an_untrustworthy_pin`,
/// `tests::palace_name_for_ignores_the_palace_env`.
pub fn palace_name_for(project_root: &Path) -> Result<String, PalaceCreatePlanError> {
    // #11823: the daemon's env-free rule, so tctl sends a name it accepts.
    trusty_common::resolve_palace_for_project(project_root).map(|r| r.id)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::commands::ensure::ENV_TEST_LOCK as ENV_LOCK;
    use crate::commands::test_support::clear_env;
    use std::path::PathBuf;
    use std::process::Command;
    use trusty_common::{PALACE_OVERRIDE_ENV, PIN_FILE_REL};

    /// Create `<tmp>/<name>` as a git repo, with `origin` set when `remote` is.
    ///
    /// Why: the resolver's levels 3 and 4 read git, so the tests need a real repo
    /// whose remote they control rather than the developer's checkout.
    pub(crate) fn git_project(name: &str, remote: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().join(name);
        std::fs::create_dir_all(&root).expect("mkdir");
        // The resolver canonicalises (macOS `/var` → `/private/var`), so the
        // pin path it reports is compared against the canonical form.
        let root = std::fs::canonicalize(&root).expect("canonicalize");
        let git = |args: &[&str]| {
            let st = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .status()
                .expect("run git");
            assert!(st.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        if let Some(url) = remote {
            git(&["remote", "add", "origin", url]);
        }
        (tmp, root)
    }

    /// Write `body` as the palace pin of `root`.
    pub(crate) fn write_pin(root: &Path, body: &str) -> PathBuf {
        let path = root.join(PIN_FILE_REL);
        std::fs::create_dir_all(path.parent().expect("pin parent")).expect("mkdir");
        std::fs::write(&path, body).expect("write pin");
        path
    }

    /// Resolve with the env override cleared, so the developer's own
    /// `TRUSTY_MEMORY_PALACE` cannot decide the answer.
    fn resolve_isolated(root: &Path) -> Result<String, PalaceCreatePlanError> {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env(&[PALACE_OVERRIDE_ENV]);
        palace_name_for(root)
    }

    /// Why: the index id must be the bare directory basename so it matches
    /// `trusty-search index`'s default.
    /// What: `/Users/bob/Projects/trusty-tools` → `trusty-tools`.
    /// Test: This is the test.
    #[test]
    fn index_id_is_basename() {
        assert_eq!(
            index_id_for(Path::new("/Users/bob/Projects/trusty-tools")).as_deref(),
            Some("trusty-tools")
        );
    }

    /// Why: a root-less path has no basename; the id must be `None` so the caller
    /// surfaces an actionable error rather than registering an empty id.
    /// What: `/` → `None`.
    /// Test: This is the test.
    #[test]
    fn index_id_root_is_none() {
        assert_eq!(index_id_for(Path::new("/")), None);
    }

    /// Why: a committed pin is authoritative and wins over the git remote.
    /// What: pin `pinned-name` in a repo whose remote is `acme/widget`.
    /// Test: This is the test.
    #[test]
    fn palace_name_for_reads_pin() {
        let (_tmp, root) = git_project("checkout-dir", Some("git@github.com:acme/widget.git"));
        write_pin(&root, "schema_version: 1\npalace: pinned-name\n");
        assert_eq!(resolve_isolated(&root).expect("resolves"), "pinned-name");
    }

    /// Why (#11823): readers name a remote-backed project `owner-repo`; `ensure`
    /// used the folder slug (`my-repo`), so it created a palace no reader used.
    /// What: a repo at `My_Repo` with remote `Acme/My_Repo` → `acme-my-repo`.
    /// Test: This is the test.
    #[test]
    fn palace_name_for_uses_owner_repo_from_the_git_remote() {
        let (_tmp, root) = git_project("My_Repo", Some("https://github.com/Acme/My_Repo.git"));
        assert_eq!(resolve_isolated(&root).expect("resolves"), "acme-my-repo");
    }

    /// Why: with no remote and no pin the resolver keys on the main worktree's
    /// `parent/dir`, and the result must still pass the daemon's id gate even
    /// for a long folder name (#2443).
    /// What: `Trusty_Tools` and a 90-character folder, both without a remote.
    /// Test: This is the test.
    #[test]
    fn palace_name_for_without_a_remote_uses_the_parent_dir_slug() {
        let (_tmp, root) = git_project("Trusty_Tools", None);
        let id = resolve_isolated(&root).expect("resolves");
        assert!(id.ends_with("-trusty-tools"), "got {id:?}");

        let (_tmp2, long) = git_project(&"long-name-".repeat(9), None);
        let id = resolve_isolated(&long).expect("a long folder still resolves");
        assert!(
            trusty_common::palace_id_is_valid(&id),
            "derived id must pass the daemon gate, got {id:?} ({} bytes)",
            id.len()
        );
    }

    /// Why (#11823): a pin that exists but cannot be trusted used to fall back
    /// to the folder slug silently. It must be an error naming the pin file.
    /// What: unparsable, empty and invalid-id pins each return `Err` whose text
    /// names the pin path.
    /// Test: This is the test.
    #[test]
    fn palace_name_for_rejects_an_untrustworthy_pin() {
        for body in [
            "palace: [unclosed\n",
            "schema_version: 1\npalace: \"   \"\n",
            "schema_version: 1\npalace: ../escape\n",
        ] {
            let (_tmp, root) = git_project("widget", Some("https://github.com/acme/widget.git"));
            let pin = write_pin(&root, body);
            let err = resolve_isolated(&root).expect_err(body);
            assert!(
                err.to_string().contains(&pin.display().to_string()),
                "error must name the pin path for {body:?}: {err}"
            );
        }
    }

    /// Why (#11823): the daemon ignores `TRUSTY_MEMORY_PALACE` when it
    /// validates a create, so tctl must too or an exported variable makes
    /// every `ensure` create a refused name.
    #[test]
    fn palace_name_for_ignores_the_palace_env() {
        let (_tmp, root) = git_project("My_Repo", Some("https://github.com/Acme/My_Repo.git"));
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::commands::test_support::set_env(PALACE_OVERRIDE_ENV, Path::new("exported-palace"));
        let id = palace_name_for(&root);
        clear_env(&[PALACE_OVERRIDE_ENV]);
        assert_eq!(id.expect("resolves"), "acme-my-repo");
    }
}
