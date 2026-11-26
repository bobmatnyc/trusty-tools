//! The working-directory repo in the `tm ls` / bare `tm` new-session picker
//! (#11822).
//!
//! Why: the picker's rows came from the project registry alone, and it opened
//! on the cursor session's project (#7406). A developer who ran `tm` inside a
//! checkout the registry did not hold never saw that checkout offered: the
//! picker opened on whichever registry row sorted first, and the only way to
//! the cwd repo was the free-text entry.
//!
//! What: [`with_cwd`] adds the cwd repo to the target list when no registry row
//! already names it, and answers which row it is — the row the picker opens on.
//! [`request_for_cwd`] is what Enter on an unregistered cwd row creates:
//! register, then a session in the checkout root, the same two legs a typed
//! checkout path produces. The #7406 temp-path and scratchpad filter applies to
//! the cwd row too, through the same predicate the registry rows pass.
//!
//! Test: `new_session_cwd_*` in `super::new_session_cwd_tests`.

use std::path::Path;

use trusty_mpm::project::record::repo_url_matches;

use super::new_session::{NewProject, NewSessionRequest, ProjectIdentity, Target};

/// Put the cwd repo in the target list and say which row it is.
///
/// Why: AC (a) of #11822 — `n` must open on the repo the operator is standing
/// in. AC (c) — the only row this adds is that repo; nothing else enters the
/// list from outside the registry.
/// What: `None` (and the list unchanged) when there is no cwd repo, or when its
/// checkout fails the #7406 offerable filter. When a registry row already names
/// the cwd repo — by checkout directory, by `repo_url`, or by stored path — that
/// row's index is returned and the list is unchanged. Otherwise a
/// [`Target::Cwd`] row is inserted first and `Some(0)` returned.
/// Test: `new_session_cwd_preselects_an_unregistered_cwd_repo`,
/// `new_session_cwd_preselects_the_registered_cwd_row`,
/// `new_session_cwd_rows_are_only_the_cwd_and_the_registry`,
/// `new_session_cwd_falls_back_to_the_cursor_without_a_cwd_repo`.
pub(crate) fn with_cwd(
    mut targets: Vec<Target>,
    cwd: Option<ProjectIdentity>,
) -> (Vec<Target>, Option<usize>) {
    let Some(id) = cwd else {
        return (targets, None);
    };
    // #11822: keep #7406's filter — a temp or scratchpad checkout is never offered.
    if !crate::commands::projects::offerable::is_offerable_checkout(Path::new(&id.root)) {
        return (targets, None);
    }
    if let Some(index) = targets.iter().position(|t| names_cwd(t, &id)) {
        return (targets, Some(index));
    }
    targets.insert(0, Target::Cwd(id));
    (targets, Some(0))
}

/// True when a registry row is the cwd repo.
fn names_cwd(target: &Target, id: &ProjectIdentity) -> bool {
    let Target::Registered { repo, checkout, .. } = target else {
        return false;
    };
    let root = Path::new(&id.root);
    checkout.as_deref() == Some(root)
        || Path::new(repo.trim()) == root
        || repo_url_matches(&id.repo_url, repo)
}

/// The create request for an unregistered cwd row: register, then create.
///
/// Why: the cwd repo is not in the registry, so the request carries the same
/// register leg a typed checkout path gets
/// ([`request_for_path`](super::new_session::request_for_path)) — the session
/// list and `tm project list` then agree about it.
/// What: `register` holds the repo leaf and canonical URL; `repo` is the
/// checkout root the daemon creates the session in.
/// Test: `new_session_cwd_enter_registers_then_creates_in_the_checkout`.
pub(crate) fn request_for_cwd(id: &ProjectIdentity) -> NewSessionRequest {
    NewSessionRequest {
        register: Some(NewProject {
            name: id.name.clone(),
            repo_url: id.repo_url.clone(),
        }),
        repo: id.root.clone(),
        label: id.name.clone(),
        name_hint: None,
    }
}
