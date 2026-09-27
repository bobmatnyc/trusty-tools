//! The repository reconciliation cleanup runs before its first destructive
//! command (#7275 round 2). Split out of `mod.rs` for the SLOC cap (#8301).

use super::{CleanupRequest, Git, owned};

/// Does the checkout at `repo_root` actually BE `repo` (#7275 round 2)?
///
/// Why: `--repo` aims `gh` and `repo_root` aims every git command, and a
/// registry entry carries both independently — written at `tm pr open` time
/// from a URL and a cwd that can drift apart afterwards (a directory reused for
/// a different clone, a hand-edited registry, an `origin` repointed). Reading a
/// MERGED pull request from one repository and then deleting branches and
/// worktrees in another is the destructive shape that mismatch produces, so the
/// two are reconciled before either is used.
/// What: reads `remote.origin.url` at `repo_root` through the [`Git`] seam —
/// not a second `Command::new("git")` — and parses it with the crate's one
/// slug parser. `None` when they agree (or when the request states no repo, in
/// which case `gh` and git both use the checkout). `Some(reason)` refuses, and
/// an origin that cannot be read or parsed refuses too: this is the ADR-0045
/// undeterminable case on a destructive path.
/// Test: `cleanup_refuses_when_the_registry_repo_and_checkout_disagree`,
/// `cleanup_refuses_when_the_checkout_origin_cannot_be_read`,
/// `cleanup_clean_path_removes_everything` (the agreeing case).
pub(super) fn repo_mismatch<T: Git>(git: &T, req: &CleanupRequest) -> Option<String> {
    let stated = req
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())?;
    let url = match git.run(
        &req.repo_root,
        &owned(&["config", "--get", "remote.origin.url"]),
    ) {
        Ok(out) if out.success && !out.stdout.trim().is_empty() => out.stdout.trim().to_string(),
        Ok(out) => {
            return Some(format!(
                "the registry names `{stated}`, and the `origin` remote at {} could not be read \
                 to confirm it: {} — refusing rather than running destructive git in a \
                 repository this cleanup may not be for (#7275)",
                req.repo_root.display(),
                out.stderr.trim()
            ));
        }
        Err(e) => {
            return Some(format!(
                "the registry names `{stated}`, and the `origin` remote at {} could not be read \
                 to confirm it: {e:#} (#7275)",
                req.repo_root.display()
            ));
        }
    };
    // The alias table only affects `ssh://<alias>/…` URLs, so an https origin
    // — what every test states — resolves identically on every machine (#7196).
    let actual = match crate::session_manager::worktree_repo_slug::parse_repo_slug(
        &url,
        &crate::session_manager::ssh_host_alias::SshHostAliases::for_current_user(),
    ) {
        Ok(slug) => slug,
        Err(_) => {
            return Some(format!(
                "the registry names `{stated}`, and the `origin` URL at {} ({url}) names no \
                 repository this can compare it against — refusing (#7275)",
                req.repo_root.display()
            ));
        }
    };
    // A non-github host resolves to `host/owner/repo`; the registry records the
    // `owner/repo` half, so the tail is what must match.
    let agrees = actual.eq_ignore_ascii_case(stated)
        || actual
            .to_ascii_lowercase()
            .ends_with(&format!("/{}", stated.to_ascii_lowercase()));
    if agrees {
        return None;
    }
    Some(format!(
        "the registry names `{stated}` but the checkout at {} is `{actual}` — refusing: a merged \
         pull request in one repository must never authorise branch deletion or worktree removal \
         in another (#7275)",
        req.repo_root.display()
    ))
}
