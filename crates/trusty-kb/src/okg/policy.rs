//! Doc-store read confinement — which directories may be ingested at all.
//!
//! Why: `okg_ingest_docstore` takes a filesystem path from a MODEL. Without a
//! boundary that is an arbitrary local-file-read capability on the default base
//! assistant (inherited by every persona that extends it): `/etc`, `~/.ssh`,
//! `~/.aws`, or `/` could be walked verbatim into a KB tree that is then
//! searchable and quotable in chat. Because the content being ingested is
//! itself untrusted, a prompt-injected document could name the next path to
//! read — so this is an exfiltration primitive, not merely an over-broad read.
//!
//! The WRITE side was already confined (`resolve_store` keeps every entity
//! under the knowledge directory). This is the matching gate on the READ side,
//! and it is enforced in this crate — at scan time, not just at tool-argument
//! time — so a hand-edited or previously-poisoned `registry.toml` row cannot
//! bypass it on a later run.
//!
//! What: [`DocStorePolicy`] holds allow-list roots. [`DocStorePolicy::permit`]
//! canonicalises a candidate directory, requires it to sit under one of those
//! roots, and then rejects any hidden segment BELOW the matched root — that one
//! rule covers `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.config/gh`, and
//! `~/.trusty-agents` itself without enumerating them, and it fails closed for
//! the dotfile dirs nobody has invented yet. It is scoped below the root rather
//! than applied to the whole path so an operator who explicitly configures a
//! hidden root (`~/.local/share/corpus`) is taken at their word. The roots are
//! configuration, not a hardcoded list: real corpora live in arbitrary places
//! (`~/Duetto/cto-resources`), so an operator must be able to extend them.
//!
//! Test: `home_default_permits_ordinary_dir`, `ssh_style_dotdir_is_rejected`,
//! `outside_allow_list_is_rejected`, `symlink_escape_is_rejected`,
//! `configured_root_is_permitted`, `empty_allow_list_denies_everything`.

use std::path::{Component, Path, PathBuf};

/// Directories a doc-store ingest is permitted to read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocStorePolicy {
    /// Allowed roots. A candidate must canonicalise to one of these or below.
    /// Empty denies everything — a policy that was never configured must not
    /// silently mean "allow all".
    pub allowed_roots: Vec<PathBuf>,
}

impl DocStorePolicy {
    /// Build a policy from explicit roots.
    pub fn new(allowed_roots: Vec<PathBuf>) -> Self {
        Self { allowed_roots }
    }

    /// The default policy: the user's home directory, minus every dot-path.
    ///
    /// Why: ordinary corpora (`~/Documents`, `~/Duetto/cto-resources`) work out
    /// of the box, while the credential and config directories that make an
    /// unconfined read dangerous are excluded by the dot-segment rule rather
    /// than by an enumeration that would inevitably fall behind.
    pub fn home_default(home: &Path) -> Self {
        Self::new(vec![home.to_path_buf()])
    }

    /// Resolve a candidate directory, or explain why it is not permitted.
    ///
    /// Why/What: see the module doc. Canonicalisation happens FIRST, so a
    /// `..` chain or a symlink pointing at `/etc` is judged by where it
    /// actually lands, not by how it was spelled.
    /// Test: the tests in this module.
    pub fn permit(&self, candidate: &Path) -> anyhow::Result<PathBuf> {
        if self.allowed_roots.is_empty() {
            anyhow::bail!(
                "no doc-store roots are configured, so {} cannot be ingested — \
                 add one under [okg] docstore_roots in ~/.trusty-agents/config.toml",
                candidate.display()
            );
        }
        let resolved = candidate.canonicalize().map_err(|e| {
            anyhow::anyhow!("doc store {} cannot be resolved: {e}", candidate.display())
        })?;
        if !resolved.is_dir() {
            anyhow::bail!("doc store {} is not a directory", resolved.display());
        }

        // Find the allowed root this path actually sits under. An unresolvable
        // root simply never matches — a stale config entry must not widen the
        // boundary or crash the ingest.
        let Some(matched) = self
            .allowed_roots
            .iter()
            .filter_map(|root| root.canonicalize().ok())
            .filter(|root| resolved.starts_with(root))
            // Longest match wins, so a more specific root's own dot-ancestry is
            // not re-judged against a broader parent root.
            .max_by_key(|root| root.components().count())
        else {
            anyhow::bail!(
                "doc store {} is outside every configured doc-store root ({}) — \
                 add it under [okg] docstore_roots in ~/.trusty-agents/config.toml to ingest it",
                resolved.display(),
                self.allowed_roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };

        // Hidden directories hold credentials and tool state, never corpora.
        //
        // The check runs only on the portion BELOW the matched root, for two
        // reasons. It must not judge the root's own ancestry — an operator who
        // configures `~/.local/share/corpus` as a root has explicitly opted in,
        // and on macOS even a plain temp dir lives under a dot-named path. And
        // it runs on the RESOLVED path, so a symlink from `~/Documents/keys`
        // into `~/.ssh` is still caught.
        let relative = resolved.strip_prefix(&matched).unwrap_or(Path::new(""));
        if let Some(dot) = first_dot_segment(relative) {
            anyhow::bail!(
                "doc store {} is not ingestible: it lies inside the hidden directory {dot:?}, \
                 which is excluded because such paths hold credentials and tool state",
                resolved.display()
            );
        }
        Ok(resolved)
    }
}

/// The first path component that begins with `.` (ignoring `/` and `..`).
fn first_dot_segment(path: &Path) -> Option<String> {
    path.components().find_map(|c| match c {
        Component::Normal(part) => {
            let s = part.to_string_lossy();
            s.starts_with('.').then(|| s.to_string())
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake home with an ordinary corpus and a credential dir.
    fn home() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        // Canonicalise up front: macOS /var -> /private/var would otherwise make
        // every prefix comparison in these tests accidental.
        let home = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("Documents/notes")).unwrap();
        std::fs::create_dir_all(home.join("Duetto/cto-resources")).unwrap();
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::create_dir_all(home.join(".config/gh")).unwrap();
        (tmp, home)
    }

    /// Why: the default must not break the ordinary case it exists to serve.
    /// What: asserts a normal directory under home resolves.
    /// Test: self-contained.
    #[test]
    fn home_default_permits_ordinary_dir() {
        let (_t, home) = home();
        let policy = DocStorePolicy::home_default(&home);
        let target = home.join("Documents/notes");
        assert_eq!(policy.permit(&target).unwrap(), target);
    }

    /// Why: THE security case (code-critic CRITICAL 2). `~/.ssh` is inside the
    /// allowed root, so only the dot-segment rule stops it.
    /// What: asserts `~/.ssh` and a nested `~/.config/gh` are both rejected with
    /// an explanatory message.
    /// Test: self-contained.
    #[test]
    fn ssh_style_dotdir_is_rejected() {
        let (_t, home) = home();
        let policy = DocStorePolicy::home_default(&home);

        for path in [home.join(".ssh"), home.join(".config/gh")] {
            let err = policy
                .permit(&path)
                .expect_err("credential directories must never be ingestible");
            assert!(
                err.to_string().contains("hidden directory"),
                "unexpected error for {}: {err}",
                path.display()
            );
        }
    }

    /// Why: paths outside the allow-list entirely — `/etc`, `/` — are the other
    /// half of the capability that must not exist.
    /// What: asserts a sibling of home, and `/etc` when present, are rejected.
    /// Test: self-contained.
    #[test]
    fn outside_allow_list_is_rejected() {
        let (_t, home) = home();
        let policy = DocStorePolicy::home_default(&home);

        let elsewhere = tempfile::tempdir().unwrap();
        let err = policy.permit(elsewhere.path()).expect_err("outside home");
        assert!(
            err.to_string().contains("outside every configured"),
            "{err}"
        );

        let etc = Path::new("/etc");
        if etc.is_dir() {
            assert!(policy.permit(etc).is_err(), "/etc must never be ingestible");
        }
    }

    /// Why: a symlink inside an allowed root is the classic bypass — the literal
    /// path looks fine and only the resolved one betrays it.
    /// What: links `~/Documents/escape` to an outside dir and to `~/.ssh`, and
    /// asserts both are rejected.
    /// Test: self-contained (unix only — no symlink semantics elsewhere).
    #[test]
    #[cfg(unix)]
    fn symlink_escape_is_rejected() {
        let (_t, home) = home();
        let policy = DocStorePolicy::home_default(&home);
        let outside = tempfile::tempdir().unwrap();

        let escape = home.join("Documents/escape");
        std::os::unix::fs::symlink(outside.path(), &escape).unwrap();
        let err = policy.permit(&escape).expect_err("symlink out of home");
        assert!(
            err.to_string().contains("outside every configured"),
            "{err}"
        );

        let to_ssh = home.join("Documents/keys");
        std::os::unix::fs::symlink(home.join(".ssh"), &to_ssh).unwrap();
        let err = policy
            .permit(&to_ssh)
            .expect_err("symlink into a credential dir");
        assert!(err.to_string().contains("hidden directory"), "{err}");
    }

    /// Why: real corpora live outside home too, so the allow-list must be
    /// genuinely extensible rather than a hardcoded home check.
    /// What: adds an explicit root and asserts a directory under it resolves
    /// while its parent's sibling still does not.
    /// Test: self-contained.
    #[test]
    fn configured_root_is_permitted() {
        let (_t, home) = home();
        let extra = tempfile::tempdir().unwrap();
        let extra_root = extra.path().canonicalize().unwrap();
        std::fs::create_dir_all(extra_root.join("cto-resources")).unwrap();

        let policy = DocStorePolicy::new(vec![home.clone(), extra_root.join("cto-resources")]);
        assert!(policy.permit(&extra_root.join("cto-resources")).is_ok());
        assert!(policy.permit(&home.join("Documents")).is_ok());
        assert!(
            policy.permit(&extra_root).is_err(),
            "only the configured subtree is allowed, not its parent"
        );
    }

    /// Why: an unconfigured policy must fail CLOSED. Defaulting to "allow all"
    /// would reintroduce the vulnerability the moment a caller forgot to wire
    /// the config.
    /// What: asserts the default (empty) policy permits nothing.
    /// Test: self-contained.
    #[test]
    fn empty_allow_list_denies_everything() {
        let (_t, home) = home();
        let policy = DocStorePolicy::default();
        let err = policy
            .permit(&home.join("Documents"))
            .expect_err("fail closed");
        assert!(err.to_string().contains("no doc-store roots"), "{err}");
    }
}
