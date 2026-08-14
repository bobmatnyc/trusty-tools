//! Where an approved root can come from, and how a candidate path is matched
//! against those sources.
//!
//! Why: #767's owner policy admits exactly two categories of indexable root —
//! a root registered as a PROJECT, and a worktree the system provisioned under
//! one. Neither is expressible by `allowlist.toml` alone: the project registry
//! is owned by `trusty-mpm`, and a worktree is created and destroyed far too
//! often for an operator to hand-approve each one. Rather than invent a second
//! approval list, this module makes the allowlist a UNION of typed sources and
//! keeps `allowlist.toml` as the one hand-editable member of that union.
//!
//! What: [`AllowlistPaths`] names the two files consulted (both injectable for
//! tests), [`AllowSource`] says which member approved a path, and
//! [`resolve_allow_source`] answers the only question callers have — is this
//! root approved, and by what. The project registry is read as DATA (its JSON
//! shape) because `trusty-search` cannot depend on `trusty-mpm`; an unreadable
//! or absent registry yields ZERO project roots, which denies rather than
//! permits.
//!
//! Test: `sources_tests.rs`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Directory segments that mark a system-provisioned git worktree.
///
/// Why: `tm` and Claude Code both provision agent worktrees at
/// `<repo>/.claude/worktrees/<id>`, and the `tm` worktree flow also uses
/// `<repo>/.worktrees/<id>`. Those are the "explicitly provisioned locations"
/// of #767's policy; nothing else under an approved root is auto-approved.
/// What: matched as a two-component path segment immediately under an approved
/// root, never as a substring of the full path.
/// Test: `worktree_under_approved_root_is_allowed`,
/// `sibling_of_approved_root_is_not_allowed`.
const WORKTREE_SEGMENTS: &[&[&str]] = &[&[".claude", "worktrees"], &[".worktrees"]];

/// Which member of the allowlist union approved a root.
///
/// Why: the daemon logs WHY a root was accepted, and an operator auditing
/// "what is indexed" needs to tell a hand-approved entry from one the project
/// registry vouched for — the two have different removal procedures.
/// What: three variants mirroring the three sources consulted by
/// [`resolve_allow_source`].
/// Test: `resolve_source_reports_explicit`, `resolve_source_reports_project`,
/// `worktree_under_approved_root_is_allowed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowSource {
    /// An `[[index]]` entry the operator wrote into `allowlist.toml`.
    Explicit,
    /// A root registered in the `trusty-mpm` project registry.
    Project,
    /// A git worktree provisioned under the named approved root.
    ProvisionedWorktree {
        /// The approved root the worktree was provisioned under.
        parent: PathBuf,
    },
    /// A directory strictly inside the named approved root.
    ///
    /// Why: `trusty-search.yaml` declares several named indexes over sub-roots
    /// of one repo (`duetto-api`, `duetto-ui`), and `POST /indexes/:id/reindex`
    /// can re-point an index at a narrower root. Refusing those would break a
    /// shipped feature. It costs nothing in blast radius: everything under
    /// `<approved>/…` is already indexable through `<approved>` itself, so
    /// approving a descendant exposes strictly LESS, never more. The hard
    /// denylist is still applied to the descendant, so `<approved>/secrets`
    /// stays refused.
    WithinApproved {
        /// The approved root that contains this path.
        parent: PathBuf,
    },
    /// The request itself named this exact root and opted in
    /// (`CreateIndexRequest::allow_sensitive_path`).
    ///
    /// Why (#767 + #2914): `tcode`'s working project is a directory the USER
    /// bound it to, and it can legitimately be a bake-off scratch root under an
    /// OS-temp prefix. Nothing can put such a root into `allowlist.toml` from
    /// inside `trusty-common` (which cannot depend on `trusty-search`), so
    /// without this the shipped #2914 behaviour would be `403` forever.
    ///
    /// This is NOT a general default-deny bypass, for four reasons: the flag
    /// defaults to `false`, so every automatic path #767 names — cwd probe,
    /// query-referenced path, transient worktree, test fixture — is unaffected;
    /// the denylist rows for credential directories, secret file names, and
    /// top-level home directories are still enforced (only the ephemeral-prefix
    /// rows relax); the approval is per-request and writes nothing durable, so
    /// it cannot accumulate or outlive the call; and it confers no privilege a
    /// caller lacks, since the daemon is loopback-only and unauthenticated, so
    /// any process that can set the flag can equally write `allowlist.toml`.
    /// Every use is logged at `warn`.
    /// Test: `create_index_accepts_explicit_sensitive_path_optin`,
    /// `allow_sensitive_path_still_obeys_the_credential_denylist`.
    ExplicitRequest,
}

impl std::fmt::Display for AllowSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AllowSource::Explicit => write!(f, "explicit allowlist entry"),
            AllowSource::Project => write!(f, "registered project"),
            AllowSource::ProvisionedWorktree { parent } => {
                write!(f, "provisioned worktree of {}", parent.display())
            }
            AllowSource::WithinApproved { parent } => {
                write!(f, "inside approved root {}", parent.display())
            }
            AllowSource::ExplicitRequest => {
                write!(f, "explicit opted-in single-root request")
            }
        }
    }
}

/// The two files the allowlist union is read from.
///
/// Why: every check must be injectable so tests never read (or write) the
/// operator's real configuration, and so a daemon under test can be pointed at
/// a fixture. A struct rather than two loose `Option<&Path>` parameters keeps
/// the call sites from drifting as sources are added.
/// What: `None` on either field means "use the real default location".
/// Test: every test in `sources_tests.rs` constructs one explicitly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AllowlistPaths {
    /// Override for `allowlist.toml`; `None` uses `AllowlistConfig::default_path()`.
    pub allowlist: Option<PathBuf>,
    /// Override for the `trusty-mpm` project registry; `None` uses
    /// [`default_project_paths_file`].
    pub project_paths: Option<PathBuf>,
}

impl AllowlistPaths {
    /// Resolved path to `allowlist.toml`.
    pub fn allowlist_file(&self) -> PathBuf {
        self.allowlist
            .clone()
            .unwrap_or_else(super::AllowlistConfig::default_path)
    }

    /// Resolved path to the project registry.
    pub fn project_paths_file(&self) -> PathBuf {
        self.project_paths
            .clone()
            .unwrap_or_else(default_project_paths_file)
    }

    /// Builder: point the allowlist at `path`.
    pub fn with_allowlist(mut self, path: impl Into<PathBuf>) -> Self {
        self.allowlist = Some(path.into());
        self
    }

    /// Builder: point the project registry at `path`.
    pub fn with_project_paths(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_paths = Some(path.into());
        self
    }
}

/// Location of the `trusty-mpm` project registry: `<mpm-root>/project-paths.json`.
///
/// Why: the registry is the authority for "is this directory a project", and
/// `trusty-search` cannot link `trusty-mpm` to ask it — so it reads the same
/// file `tm` writes. `TRUSTY_MPM_ROOT` is honoured because `tm` itself honours
/// it; ignoring it would make a relocated `tm` install silently deny every
/// project.
/// What: `$TRUSTY_MPM_ROOT/project-paths.json` when that variable is set and
/// non-empty, else `~/.trusty-mpm/project-paths.json`.
/// Test: `default_project_paths_file_ends_at_the_registry`.
pub fn default_project_paths_file() -> PathBuf {
    let root = match std::env::var("TRUSTY_MPM_ROOT") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw),
        _ => match dirs::home_dir() {
            Some(home) => home.join(".trusty-mpm"),
            None => PathBuf::from(".trusty-mpm"),
        },
    };
    root.join("project-paths.json")
}

/// One row of `project-paths.json`, as written by
/// `trusty_mpm::core::project_aliases::ProjectAliasEntry`.
///
/// Why: mirrors the producer's shape so the file can be read without a
/// dependency edge. Unknown fields are ignored, so the producer can grow the
/// record without breaking this reader.
#[derive(Debug, Deserialize)]
struct ProjectPathEntry {
    path: PathBuf,
}

/// Read every registered project root from the `trusty-mpm` project registry.
///
/// Why: this is the "projects" half of #767's policy, and it must FAIL CLOSED —
/// a missing, unreadable, or malformed registry has to deny indexing, never
/// permit it. Returning `Vec` rather than `Result` is what enforces that: there
/// is no error path a caller could accidentally treat as "allow everything".
/// What: parses the JSON array, drops rows whose path is empty, and logs once
/// at `warn` when a present file fails to parse (silence there would hide a
/// registry the operator believes is in force).
/// Test: `project_roots_reads_registry`, `project_roots_empty_when_missing`,
/// `project_roots_empty_when_malformed`.
pub fn project_roots(path: &Path) -> Vec<PathBuf> {
    if !path.exists() {
        return Vec::new();
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                "allowlist: could not read project registry {}: {e} — \
                 no project roots are approved this run",
                path.display()
            );
            return Vec::new();
        }
    };
    match serde_json::from_str::<Vec<ProjectPathEntry>>(&raw) {
        Ok(entries) => entries
            .into_iter()
            .map(|e| e.path)
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        Err(e) => {
            tracing::warn!(
                "allowlist: could not parse project registry {}: {e} — \
                 no project roots are approved this run",
                path.display()
            );
            Vec::new()
        }
    }
}

/// Every root approved by the union, paired with the source that approved it.
///
/// Why: the daemon logs it, `trusty-search index list` prints it, and the
/// warm-boot filter tests each restored entry against it. One producer keeps
/// those three from drifting.
/// What: explicit `allowlist.toml` entries first (so a hand-approved root
/// reports as `Explicit` even when the project registry also lists it), then
/// project roots. Propagates a malformed `allowlist.toml` as an error — a
/// corrupt file must surface loudly rather than silently read as empty.
/// Test: `approved_roots_prefers_explicit_over_project`.
pub fn approved_roots(paths: &AllowlistPaths) -> anyhow::Result<Vec<(PathBuf, AllowSource)>> {
    let cfg = super::AllowlistConfig::load_from(&paths.allowlist_file())?;
    let mut out: Vec<(PathBuf, AllowSource)> = Vec::new();
    for entry in cfg.entries {
        let canonical = super::canonicalise(&entry.path);
        if !is_usable_root(&canonical, "allowlist.toml") {
            continue;
        }
        out.push((canonical, AllowSource::Explicit));
    }
    for root in project_roots(&paths.project_paths_file()) {
        let canonical = super::canonicalise(&root);
        if !is_usable_root(&canonical, "project registry") {
            continue;
        }
        if out.iter().any(|(p, _)| *p == canonical) {
            continue;
        }
        out.push((canonical, AllowSource::Project));
    }
    Ok(out)
}

/// Reject an approved-root value that would make containment match everything.
///
/// Why: `resolve_allow_source` decides containment with `Path::starts_with`, and
/// `Path::new("/Users/me/.ssh").starts_with("")` is `true` — as is
/// `starts_with("/")`. `canonicalise` falls back to the input on failure, so a
/// single malformed row (`[[index]] path = ""`, or `path = "/"`) would turn the
/// whole allowlist into a global allow. That is a one-typo total defeat of
/// default-deny, so it is rejected at the source rather than guarded at each
/// comparison.
/// What: a usable root is absolute and has more than one component — which
/// excludes `""`, a relative path, and the filesystem root itself. Each rejected
/// row is logged at `warn` naming the file it came from; silently dropping it
/// would leave an operator wondering why their entry does nothing.
/// Test: `empty_allowlist_entry_does_not_approve_everything`,
/// `filesystem_root_entry_does_not_approve_everything`,
/// `relative_allowlist_entry_is_rejected`.
fn is_usable_root(canonical: &Path, source: &str) -> bool {
    if canonical.is_absolute() && canonical.components().count() > 1 {
        return true;
    }
    tracing::warn!(
        path = %canonical.display(),
        %source,
        "allowlist: ignoring an approved-root entry that is empty, relative, or \
         the filesystem root — such a value would approve every path (#767)"
    );
    false
}

/// Whether `candidate` is a system-provisioned worktree of `root`.
///
/// Why: an agent worktree is created and removed on the timescale of a single
/// task, so requiring an operator to approve each one would make the gate
/// unusable and invite a blanket bypass. Approving them by DERIVATION from an
/// already-approved root keeps the blast radius equal to that root.
/// What: `candidate` must live directly under `<root>/<segment…>/<name>` for
/// one of [`WORKTREE_SEGMENTS`] — matched component-wise, so a directory merely
/// NAMED `.worktrees-backup` does not qualify, and a root's ordinary
/// subdirectory (`<root>/src`) is not approved just because its parent is.
/// Test: `worktree_under_approved_root_is_allowed`,
/// `sibling_of_approved_root_is_not_allowed`,
/// `subdirectory_of_approved_root_is_allowed` (proves an ordinary descendant
/// reaches the containment arm rather than this one).
fn is_provisioned_worktree_of(candidate: &Path, root: &Path) -> bool {
    let Ok(rel) = candidate.strip_prefix(root) else {
        return false;
    };
    let components: Vec<String> = rel
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    WORKTREE_SEGMENTS.iter().any(|segment| {
        // `<segment…>` plus exactly one worktree-name component.
        components.len() == segment.len() + 1
            && components
                .iter()
                .zip(segment.iter())
                .all(|(have, want)| have == want)
    })
}

/// Resolve which source (if any) approves `path`.
///
/// Why: the single answer every gate needs. Keeping the union logic here means
/// `POST /indexes`, the reindex/relocate root override, the CLI, and warm-boot
/// all ask the same question and cannot drift apart.
/// What: canonicalises `path`, then checks the union in order — exact match
/// against an approved root; a worktree provisioned under one (reported first
/// because it is the more specific answer, and because such a worktree can live
/// OUTSIDE its repo, where containment would not find it); finally strict
/// containment inside an approved root. Returns `None` when nothing approves it
/// (default-deny).
/// Test: `resolve_source_reports_explicit`, `resolve_source_reports_project`,
/// `worktree_under_approved_root_is_allowed`,
/// `subdirectory_of_approved_root_is_allowed`, `unlisted_root_is_denied`.
pub fn resolve_allow_source(
    path: &Path,
    paths: &AllowlistPaths,
) -> anyhow::Result<Option<AllowSource>> {
    let target = super::canonicalise(path);
    let roots = approved_roots(paths)?;
    if let Some((_, source)) = roots.iter().find(|(root, _)| *root == target) {
        return Ok(Some(source.clone()));
    }
    for (root, _) in &roots {
        if is_provisioned_worktree_of(&target, root) {
            return Ok(Some(AllowSource::ProvisionedWorktree {
                parent: root.clone(),
            }));
        }
    }
    for (root, _) in &roots {
        if target.starts_with(root) && target != *root {
            return Ok(Some(AllowSource::WithinApproved {
                parent: root.clone(),
            }));
        }
    }
    Ok(None)
}
