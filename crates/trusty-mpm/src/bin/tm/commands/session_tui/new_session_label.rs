//! What each new-session picker row reads as (#7406), and why it is offered
//! (#11822).
//!
//! Why: the label rules were split out of `new_session.rs` when #11822 added a
//! second axis to every row — the reason it is in the list — and that file sat
//! at the 500-SLOC cap.
//!
//! What: [`row_labels`] turns every target into its short `owner/repo` form;
//! [`row_reason`] says where the row came from — the operator's working
//! directory or the project registry — so a row that looks stray is explained
//! on screen.
//!
//! Test: `new_session_rows_show_owner_repo_only`,
//! `new_session_labels_disambiguate_by_host`,
//! `new_session_cwd_rows_say_why_they_are_offered`.

use std::path::Path;

use trusty_common::github_path::parse_remote_url;

use super::new_session::Target;

/// One display label per target — `owner/repo`, never a URL or a path (#7406).
///
/// Why: the owner's screen showed a stored `repo_url` verbatim, which is either
/// a full `https://github.com/owner/repo` or an absolute checkout path. Both are
/// wider than the overlay, so the row wrapped and the list stopped being
/// scannable. `owner/repo` is the identity, and everything else was noise.
/// What: a parseable git remote becomes `owner/repo`, widened to
/// `host/owner/repo` ONLY when another row on a different host spells the same
/// `owner/repo`. An absolute path becomes `<basename> (local)`, as does a row
/// with no `repo_url` at all. Anything else falls back to the registry name.
/// #11822: the unregistered cwd row is labelled from its canonical URL the
/// same way.
/// Test: `new_session_rows_show_owner_repo_only`,
/// `new_session_labels_disambiguate_by_host`.
pub(crate) fn row_labels(targets: &[Target]) -> Vec<String> {
    let labels: Vec<Option<Label>> = targets
        .iter()
        .map(|t| match t {
            Target::Registered { name, repo, .. } => Some(label_for(name, repo)),
            Target::Cwd(id) => Some(label_for(&id.name, &id.repo_url)),
            Target::Other => None,
        })
        .collect();
    labels
        .iter()
        .map(|label| match label {
            Some(Label::Remote { host, owner, repo }) => {
                if collides_across_hosts(&labels, host, owner, repo) {
                    format!("{host}/{owner}/{repo}")
                } else {
                    format!("{owner}/{repo}")
                }
            }
            Some(Label::Local(base)) => format!("{base} (local)"),
            Some(Label::Plain(name)) => name.clone(),
            None => "other — type a project path…".to_string(),
        })
        .collect()
}

/// Why a row is in the picker, as the suffix the row shows (#11822).
///
/// Why: the registry holds every project this host ever registered, including
/// ones registered implicitly from session history, so a row unrelated to the
/// current checkout looked like a bug with no stated reason.
/// What: `" · cwd"` for the row that is the working directory's repo (whether
/// or not it is also registered), `" · registered"` for any other registry
/// row, and nothing for the typed-path escape row.
/// Test: `new_session_cwd_rows_say_why_they_are_offered`.
pub(crate) fn row_reason(target: &Target, is_cwd: bool) -> &'static str {
    match target {
        Target::Other => "",
        Target::Cwd(_) => " · cwd",
        Target::Registered { .. } if is_cwd => " · cwd",
        Target::Registered { .. } => " · registered",
    }
}

/// What one registry row reduces to before the host is decided.
enum Label {
    /// A git remote: owner and repo as the remote spells them, plus its host.
    Remote {
        /// Remote host, kept only for the disambiguating form.
        host: String,
        /// Repository owner.
        owner: String,
        /// Repository name.
        repo: String,
    },
    /// A checkout that exists only on this host; the directory's basename.
    Local(String),
    /// Neither a remote nor a path — the registry name is all there is.
    Plain(String),
}

/// Reduce one registry row to its [`Label`].
fn label_for(name: &str, repo: &str) -> Label {
    if let Ok(remote) = parse_remote_url(repo) {
        return Label::Remote {
            host: remote.host,
            owner: remote.owner,
            repo: remote.repo,
        };
    }
    let trimmed = repo.trim();
    if trimmed.is_empty() {
        return Label::Local(name.to_string());
    }
    if trimmed.starts_with('/') {
        let base = Path::new(trimmed)
            .file_name()
            .map(|b| b.to_string_lossy().to_string())
            .unwrap_or_else(|| name.to_string());
        return Label::Local(base);
    }
    Label::Plain(name.to_string())
}

/// True when another row spells this `owner/repo` on a DIFFERENT host.
///
/// The host is width the row can rarely afford, so it is added only when
/// leaving it out would make two rows read identically.
fn collides_across_hosts(labels: &[Option<Label>], host: &str, owner: &str, repo: &str) -> bool {
    labels.iter().flatten().any(|l| {
        matches!(
            l,
            Label::Remote {
                host: h,
                owner: o,
                repo: r,
            } if o.eq_ignore_ascii_case(owner)
                && r.eq_ignore_ascii_case(repo)
                && !h.eq_ignore_ascii_case(host)
        )
    })
}
