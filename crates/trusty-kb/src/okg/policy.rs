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
//! roots, and then rejects any HIDDEN segment below the matched root. "Hidden"
//! means three things, because one platform's convention is not another's: a
//! dot-prefix (`~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.config/gh`,
//! `~/.trusty-agents`), the `UF_HIDDEN` file flag (macOS marks `~/Library` this
//! way — it is NOT dot-prefixed, yet holds `Application Support/<App>` tokens,
//! `Keychains`, and `Preferences`), and a small name backstop for when the flag
//! is absent. Together they fail closed for the credential dirs nobody has
//! invented yet, without enumerating the ones that exist. The check is scoped
//! below the root rather than applied to the whole path so an operator who
//! explicitly configures a hidden root (`~/.local/share/corpus`, or even a
//! directory inside `~/Library`) is taken at their word — explicit
//! configuration beats the default heuristic. The roots are configuration, not
//! a hardcoded list: real corpora live in arbitrary places
//! (`~/Corpora/research`), so an operator must be able to extend them.
//!
//! Known limits, accepted deliberately:
//!   - Loose non-hidden files sitting directly in `$HOME` (`~/private_key.pem`)
//!     are still reachable under the DEFAULT policy. That is inherent to
//!     defaulting to `$HOME` at all; an operator wanting a tighter boundary
//!     narrows `docstore_roots` to the specific corpus directories.
//!   - This gate authorises a ROOT. Per-file symlink safety during the walk is
//!     `WalkDir`'s `follow_links(false)`, pinned explicitly in
//!     [`crate::okg::docstore::scan`] so a refactor cannot silently flip it.
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
    /// Why: ordinary corpora (`~/Documents`, `~/Corpora/research`) work out
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
        if let Some(hidden) = hidden_segment_below(&matched, &resolved) {
            anyhow::bail!(
                "doc store {} is not ingestible: it lies inside the hidden directory {hidden:?}, \
                 which is excluded because such paths hold credentials and tool state — \
                 name it directly in [okg] docstore_roots if you really intend to ingest it",
                resolved.display()
            );
        }
        Ok(resolved)
    }
}

/// Segment names excluded by name on every platform.
///
/// Why: `~/Library` is the macOS equivalent of the dotfile directories — it
/// holds `Application Support/<App>/*.json` tokens, `Keychains`, `Preferences`,
/// and `Logs` — but it is NOT dot-prefixed; Finder hides it with the `UF_HIDDEN`
/// file flag instead. A dot-only rule therefore left every per-app credential
/// file under the default `$HOME` policy readable, and `.json`/`.log`/`.toml`
/// are all in `DEFAULT_EXTENSIONS`. The flag check below catches this properly,
/// but it is a filesystem query that can be defeated (a restored-from-backup or
/// synced home may not carry the flag), so the name is also refused outright as
/// a backstop. Cross-platform rather than macOS-gated: `Library` is not a
/// conventional corpus directory anywhere, and keeping one rule everywhere makes
/// the behaviour testable on every CI runner instead of only on macOS.
const EXCLUDED_SEGMENT_NAMES: &[&str] = &["Library"];

/// The first excluded segment BELOW `root`, if any.
///
/// Why: three rules, one walk — dot-prefix, the name backstop, and the
/// platform hidden flag. Walking segment by segment (rather than testing only
/// the leaf) means a corpus nested deep under a hidden ancestor is still
/// refused.
/// Test: `ssh_style_dotdir_is_rejected`, `macos_library_is_rejected`,
/// `platform_hidden_flag_is_rejected`.
fn hidden_segment_below(root: &Path, resolved: &Path) -> Option<String> {
    let relative = resolved.strip_prefix(root).ok()?;
    let mut probe = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        probe.push(part);
        let name = part.to_string_lossy().to_string();
        if name.starts_with('.') {
            return Some(name);
        }
        if EXCLUDED_SEGMENT_NAMES
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(&name))
        {
            return Some(name);
        }
        if is_platform_hidden(&probe) {
            return Some(name);
        }
    }
    None
}

/// Whether the OS marks this path hidden by a file flag rather than by name.
///
/// macOS sets `UF_HIDDEN` on `~/Library` and friends. Checking the real flag
/// generalises past the hardcoded name list to anything else the user or an
/// installer has hidden.
///
/// Fails CLOSED: an unreadable segment counts as hidden. A permission-denied
/// stat, a transient IO error, or a segment removed between canonicalisation
/// and this probe all mean "cannot determine" — and in a security gate that
/// must read as "not permitted", never as "not hidden". Erring the other way
/// let an unreadable path through on an error the caller never saw.
#[cfg(target_os = "macos")]
fn is_platform_hidden(path: &Path) -> bool {
    use std::os::macos::fs::MetadataExt;
    /// `UF_HIDDEN` from `<sys/stat.h>` — "hint that this item should not be displayed".
    const UF_HIDDEN: u32 = 0x0000_8000;
    std::fs::metadata(path)
        .map(|m| m.st_flags() & UF_HIDDEN != 0)
        .unwrap_or(true)
}

/// Non-macOS platforms have no equivalent flag; the dot-prefix and name rules
/// carry the whole exclusion there.
#[cfg(not(target_os = "macos"))]
fn is_platform_hidden(_path: &Path) -> bool {
    false
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
        std::fs::create_dir_all(home.join("Corpora/research")).unwrap();
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

    /// Why: code-critic CRITICAL — `~/Library` is macOS's credential and
    /// app-state tree (`Application Support/<App>/*.json` tokens, `Keychains`,
    /// `Preferences`, `Logs`) but it is NOT dot-prefixed, so the original
    /// dot-only rule PERMITTED all of it under the default `$HOME` policy —
    /// and `.json`/`.log`/`.toml` are all in `DEFAULT_EXTENSIONS`. Every prior
    /// fixture was dot-prefixed, so nothing caught it.
    /// What: asserts the three real-world shapes are refused by name, on every
    /// platform, at any nesting depth.
    /// Test: self-contained.
    #[test]
    fn macos_library_is_rejected() {
        let (_t, home) = home();
        let policy = DocStorePolicy::home_default(&home);

        for relative in [
            "Library",
            "Library/Application Support/SomeApp",
            "Library/Keychains",
            "Library/Preferences",
            "Library/Logs",
        ] {
            let path = home.join(relative);
            std::fs::create_dir_all(&path).unwrap();
            let err = policy
                .permit(&path)
                .expect_err(&format!("{relative} must not be ingestible"));
            assert!(
                err.to_string().contains("hidden directory"),
                "unexpected error for {relative}: {err}"
            );
        }
    }

    /// Why: explicit configuration must still beat the heuristic — an operator
    /// who names a directory inside `~/Library` in `docstore_roots` has opted
    /// in deliberately, consistent with how a configured `~/.local/...` root
    /// already behaves.
    /// What: configures a root INSIDE Library and asserts it is permitted while
    /// its siblings under the plain `$HOME` root are still refused.
    /// Test: self-contained.
    #[test]
    fn explicitly_configured_library_root_is_permitted() {
        let (_t, home) = home();
        let corpus = home.join("Library/Application Support/MyCorpus");
        std::fs::create_dir_all(&corpus).unwrap();
        let secrets = home.join("Library/Application Support/SomeApp");
        std::fs::create_dir_all(&secrets).unwrap();

        // Home alone: the corpus is refused along with everything else there.
        assert!(DocStorePolicy::home_default(&home).permit(&corpus).is_err());

        // Named directly: permitted, because the operator said so.
        let policy = DocStorePolicy::new(vec![home.clone(), corpus.clone()]);
        assert_eq!(
            policy.permit(&corpus).unwrap(),
            corpus,
            "an explicitly configured root inside Library must still work"
        );
        assert!(
            policy.permit(&secrets).is_err(),
            "opting one directory in must not open the rest of Library"
        );
    }

    /// Why: the name backstop only covers `Library`. The real generalisation is
    /// the `UF_HIDDEN` flag, which covers anything else the user or an installer
    /// has hidden — so the flag path needs its own coverage, independent of the
    /// name list.
    /// What: creates an ordinarily-named directory, sets `UF_HIDDEN` via
    /// `chflags`, and asserts it becomes non-ingestible. macOS-only: no other
    /// supported platform has the flag, and `is_platform_hidden` is compiled to
    /// a constant `false` there.
    /// Test: self-contained.
    #[test]
    #[cfg(target_os = "macos")]
    fn platform_hidden_flag_is_rejected() {
        let (_t, home) = home();
        let policy = DocStorePolicy::home_default(&home);
        let dir = home.join("Vault");
        std::fs::create_dir_all(dir.join("inner")).unwrap();

        // Ordinary name, no flag → permitted.
        assert!(policy.permit(&dir).is_ok(), "control case must pass");

        let ok = std::process::Command::new("/usr/bin/chflags")
            .arg("hidden")
            .arg(&dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "chflags must succeed for this test to mean anything");

        assert!(
            policy.permit(&dir).is_err(),
            "a UF_HIDDEN directory must not be ingestible"
        );
        assert!(
            policy.permit(&dir.join("inner")).is_err(),
            "nor anything beneath it"
        );
    }

    /// Why: code-critic HIGH — the flag probe swallowed metadata errors as
    /// `false`, so a permission-denied stat, a transient IO error, or a segment
    /// removed mid-check silently read as "not hidden" and PERMITTED the path.
    /// A security gate must treat "cannot determine" as "not permitted".
    /// What: probes the flag layer directly on paths whose metadata cannot be
    /// read, and asserts each is reported hidden. macOS-only: the flag layer is
    /// compiled to a constant `false` elsewhere, so there is nothing to fail
    /// open there — the dot and name rules are unconditional on every platform,
    /// which is why the `~/Library` exclusion holds regardless of this.
    /// Test: self-contained.
    #[test]
    #[cfg(target_os = "macos")]
    fn unreadable_segment_fails_closed() {
        let (_t, home) = home();

        // A path that vanished between validation and probe.
        assert!(
            is_platform_hidden(&home.join("gone-between-check-and-read")),
            "an unstattable path must count as hidden, not as safe"
        );

        // A real permission-denied stat: a child under a mode-0o000 parent.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = home.join("locked");
            let child = locked.join("inner");
            std::fs::create_dir_all(&child).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

            // Root ignores the mode bits, so only assert when the read really fails.
            let unreadable = std::fs::metadata(&child).is_err();
            if unreadable {
                assert!(
                    is_platform_hidden(&child),
                    "a permission-denied segment must count as hidden"
                );
            }
            // Restore so the tempdir can be cleaned up.
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
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
