//! Free-text project entry for the `tm ls` new-session picker (#7488).
//!
//! Why: the picker could only start a session in a project the registry already
//! held, or in a checkout already on this host. A project the operator has not
//! cloned yet — the overwhelmingly common case for "work on this repo someone
//! just sent me" — had no way in at all, so they left the surface and typed
//! `tm session new <url>`.
//!
//! What: [`parse_project_entry`] is the pure recogniser for the three spellings
//! the owner asked for — a full clone URL, a bare `owner/repo` (defaulting to
//! `github.com`), and an explicit `domain/owner/repo`. [`request_for_entry`]
//! turns a recognised one into the SAME
//! [`NewSessionRequest`](super::new_session::NewSessionRequest) a typed checkout
//! path produces, so the clone and the registration run through
//! [`perform`](super::new_session::perform) — the one register-then-create
//! driver — rather than a second implementation of either.
//!
//! Nothing here touches the network or the disk: a rejected entry is a
//! `Result::Err` the overlay shows inline, never a silently dropped keystroke.
//!
//! Test: `new_session_entry_*` in `super::tests`.

use trusty_common::github_path::parse_remote_url;

use super::new_session::{NewProject, NewSessionRequest, Target};

/// Forge a bare `owner/repo` entry means.
///
/// GitHub is the only host the two-segment shorthand can mean unambiguously —
/// every other host has to be spelled, which is what the three-segment form is
/// for.
const DEFAULT_HOST: &str = "github.com";

/// A project the operator named by typing rather than by picking a row.
///
/// Why: registering a project needs a name and a clone URL, and both come from
/// one look at the typed text, so they travel together — the same shape
/// [`ProjectIdentity`](super::new_session::ProjectIdentity) has for a path,
/// minus the local root a project that is not cloned yet does not have.
/// What: `name` is the repo leaf (the registry's naming convention), `repo_url`
/// the clone URL the daemon is handed.
/// Test: `new_session_entry_parses_the_three_shapes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectEntry {
    /// Registry key for the project — the repo leaf.
    pub(crate) name: String,
    /// Clone URL handed to the create call.
    pub(crate) repo_url: String,
}

/// Recognise one typed project spelling, or refuse it.
///
/// Why (#7488 acceptance 2): the picker must tell "this is a project I can
/// clone" from "this is a typo" WITHOUT a round trip, so the whole decision is
/// one pure function the tests can table-drive.
/// What: `Some` for exactly three shapes —
/// - a clone URL [`parse_remote_url`] accepts (`https`, `http`, `ssh`, `git`
///   schemes and the scp-style `git@host:owner/repo`), kept VERBATIM so an
///   `ssh` entry still clones over ssh;
/// - `owner/repo`, which means [`DEFAULT_HOST`];
/// - `domain/owner/repo`, where the first segment must look like a host.
///
/// `None` for everything else, including a local path (`/…`, `~/…`, `./…`,
/// which the checkout resolver owns), an entry carrying whitespace, and a
/// two-segment entry whose first segment is a domain — `example.com/repo` names
/// no owner, and guessing one would clone the wrong thing.
/// Test: `new_session_entry_parses_the_three_shapes`,
/// `new_session_entry_rejects_malformed_text`.
pub(crate) fn parse_project_entry(text: &str) -> Option<ProjectEntry> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    if is_url_shaped(trimmed) {
        let remote = parse_remote_url(trimmed).ok()?;
        return Some(ProjectEntry {
            name: remote.repo,
            repo_url: trimmed.trim_end_matches('/').to_string(),
        });
    }
    // A path is the checkout resolver's job, not a clone target.
    if trimmed.starts_with('/') || trimmed.starts_with('~') || trimmed.starts_with('.') {
        return None;
    }
    let bare = trimmed.trim_end_matches('/');
    let bare = bare.strip_suffix(".git").unwrap_or(bare);
    match bare.split('/').collect::<Vec<&str>>().as_slice() {
        [owner, repo] if is_segment(owner) && is_segment(repo) && !owner.contains('.') => {
            Some(entry(DEFAULT_HOST, owner, repo))
        }
        [host, owner, repo] if is_host(host) && is_segment(owner) && is_segment(repo) => {
            Some(entry(host, owner, repo))
        }
        _ => None,
    }
}

/// Build the `https` entry for a host/owner/repo triple.
fn entry(host: &str, owner: &str, repo: &str) -> ProjectEntry {
    ProjectEntry {
        name: repo.to_string(),
        repo_url: format!("https://{host}/{owner}/{repo}"),
    }
}

/// True when the text is a git remote URL rather than a slash-separated triple.
///
/// A scheme (`https://`) or a colon before the first slash (scp-syntax
/// `git@host:owner/repo`) is what distinguishes one; both are shapes
/// [`parse_remote_url`] owns, so recognising them here only decides WHICH
/// parser answers.
fn is_url_shaped(text: &str) -> bool {
    text.contains("://")
        || text
            .split('/')
            .next()
            .is_some_and(|authority| authority.contains(':'))
}

/// True when a path segment can be an owner or a repository name.
fn is_segment(segment: &str) -> bool {
    !segment.is_empty() && segment != "." && segment != ".."
}

/// True when a first segment names a host rather than an owner.
fn is_host(segment: &str) -> bool {
    if segment.eq_ignore_ascii_case("localhost") {
        return true;
    }
    segment.contains('.') && !segment.starts_with('.') && !segment.ends_with('.')
}

/// Turn typed text into a create request, or say why it cannot be one.
///
/// Why (#7488 acceptance 2 and 3): a project the registry already holds must
/// NOT be registered a second time — `register` is an unqualified upsert, so a
/// redundant call would replace the stored record's optional fields with the
/// two this flow can supply. Comparing against the targets already in hand
/// answers that without another round trip, exactly as
/// [`request_for_path`](super::new_session::request_for_path) does for a path.
/// What: an unrecognised entry is an `Err` the overlay shows inline and nothing
/// is created. A recognised one whose name or URL is already a registered
/// target reuses that row's stored `repo` and skips the registration; anything
/// else carries a `register` leg, and its clone URL as the create argument —
/// the daemon clones it, the same provisioning `tm session new <url>` triggers.
/// Test: `new_session_entry_builds_a_clone_and_register_request`,
/// `new_session_entry_reuses_a_registered_project`,
/// `new_session_entry_rejects_malformed_text`.
pub(crate) fn request_for_entry(
    text: &str,
    targets: &[Target],
) -> Result<NewSessionRequest, String> {
    let trimmed = text.trim();
    let Some(entry) = parse_project_entry(trimmed) else {
        return Err(format!(
            "{trimmed} is not a project — type owner/repo, domain/owner/repo, \
             or a clone URL"
        ));
    };
    let known = targets.iter().find_map(|t| match t {
        Target::Registered { name, repo } => {
            (*name == entry.name || *repo == entry.repo_url).then(|| (name.clone(), repo.clone()))
        }
        Target::Other => None,
    });
    // Already registered: start the session in the row the registry holds
    // rather than cloning a second copy of it.
    if let Some((name, repo)) = known {
        return Ok(NewSessionRequest {
            register: None,
            repo,
            label: name,
        });
    }
    Ok(NewSessionRequest {
        register: Some(NewProject {
            name: entry.name.clone(),
            repo_url: entry.repo_url.clone(),
        }),
        repo: entry.repo_url,
        label: entry.name,
    })
}
