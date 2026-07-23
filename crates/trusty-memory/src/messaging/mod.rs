//! Inter-project messaging primitive (issue #99).
//!
//! Why: Replaces the Python `/mpm-message` skill (claude-mpm repo, writes
//! to `~/.claude-mpm/messaging.db`) with a trusty-memory-native primitive.
//! Single-daemon-per-host architecture means cross-project messaging is
//! just a write to a different palace and a read at session start — no
//! IPC required.
//!
//! What: helpers that encode messages as **drawers tagged with a `msg:*`
//! namespace** so we don't have to change the `Drawer` schema:
//!
//! - `msg:v1` — marker tag for fast filtering / dedup.
//! - `msg:from=<palace>` — sender palace id.
//! - `msg:to=<palace>` — recipient palace id (redundant with the host palace,
//!   kept for audit + cross-palace queries).
//! - `msg:purpose=<string>` — free-text purpose / category set by the sender.
//! - `msg:sent_at=<rfc3339>` — UTC ISO 8601 timestamp when the sender wrote it.
//! - `msg:read=<bool>` — receiver-controlled read flag (`true` after the
//!   SessionStart hook has delivered it once).
//!
//! Sub-modules:
//!   - `types`: `Message`, tag-prefix constants, `build_message_tags`, slug
//!     helpers.
//!   - `operations`: `send_message_to_palace`, `list_unread_messages`,
//!     `list_messages`, `mark_message_read`, `cwd_palace_slug`.
//!
//! Test: `tests::round_trip_send_and_inbox`, `tests::slug_derivation_cases`,
//! `tests::mark_read_is_atomic_under_concurrency`.

mod operations;
mod types;

pub use operations::{
    cwd_palace_slug, cwd_palace_slug_at, list_messages, list_unread_messages, mark_message_read,
    send_message_to_palace,
};
pub use types::{
    build_message_tags, slugify_for_palace, slugify_string, Message, MSG_MARKER_TAG,
    TAG_FROM_PREFIX, TAG_PURPOSE_PREFIX, TAG_READ_PREFIX, TAG_SENT_AT_PREFIX, TAG_TO_PREFIX,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{CreatorInfo, CreatorSource};
    use chrono::Utc;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::Arc;
    use trusty_common::memory_core::{Palace, PalaceHandle, PalaceId, PalaceRegistry};

    /// RAII guard that sets or clears an environment variable for the duration
    /// of a test and restores the prior value on drop.
    ///
    /// Why: the issue-#1217 derivation reads `TRUSTY_MEMORY_PALACE`; tests must
    /// pin it deterministically without leaking state into sibling tests. Pair
    /// every use with `#[serial_test::serial]` so no other thread reads the env
    /// concurrently (cargo runs test fns across OS threads in one process).
    /// What: `set` installs a value, `clear` removes it; both capture the prior
    /// value and restore it in `Drop`.
    /// Test: exercised by `cwd_palace_slug_at_env_override_wins` and the
    /// derivation tests that must run with the override cleared.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: paired with `#[serial]` on the calling test so no other
            // thread reads or writes the env concurrently.
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }

        fn clear(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            // SAFETY: see `set`.
            unsafe { std::env::remove_var(key) };
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see `set`.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    /// Test-only builder for a `CreatorInfo`. Tests don't care which writer
    /// they simulate; pinning the values here avoids per-test boilerplate.
    fn test_creator() -> CreatorInfo {
        CreatorInfo {
            client: "test-suite".to_string(),
            version: "0.0.0".to_string(),
            source: CreatorSource::Mcp,
            cwd: Some("/tmp/test".to_string()),
            workstream: None,
        }
    }

    /// Helper: build a registry + palace under a tempdir and return both.
    fn fresh_palace(id: &str) -> (PalaceRegistry, Arc<PalaceHandle>, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let registry = PalaceRegistry::new();
        let palace = Palace {
            id: PalaceId::new(id),
            name: id.to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: root.join(id),
        };
        registry
            .create_palace(&root, palace)
            .expect("create_palace");
        let handle = registry
            .open_palace(&root, &PalaceId::new(id))
            .expect("open_palace");
        (registry, handle, root)
    }

    #[test]
    fn build_message_tags_includes_all_fields() {
        let ts = Utc::now();
        let tags = build_message_tags("alpha", "beta", "task", ts);
        assert!(tags.contains(&MSG_MARKER_TAG.to_string()));
        assert!(tags.iter().any(|t| t == "msg:from=alpha"));
        assert!(tags.iter().any(|t| t == "msg:to=beta"));
        assert!(tags.iter().any(|t| t == "msg:purpose=task"));
        assert!(tags.iter().any(|t| t == "msg:read=false"));
        assert!(tags
            .iter()
            .any(|t| t.starts_with("msg:sent_at=") && t.ends_with(&ts.to_rfc3339())));
    }

    #[test]
    fn decode_message_from_drawer_round_trips() {
        use chrono::DateTime;
        use trusty_common::memory_core::palace::Drawer;
        use uuid::Uuid;
        let ts = "2026-05-25T12:34:56+00:00"
            .parse::<DateTime<chrono::FixedOffset>>()
            .unwrap()
            .with_timezone(&Utc);
        let mut d = Drawer::new(Uuid::new_v4(), "hello world");
        d.tags = build_message_tags("alpha", "beta", "task", ts);
        let m = Message::from_drawer(&d).expect("decode");
        assert_eq!(m.from_palace, "alpha");
        assert_eq!(m.to_palace, "beta");
        assert_eq!(m.purpose, "task");
        assert_eq!(m.sent_at, ts);
        assert!(!m.read);
        assert_eq!(m.content, "hello world");
    }

    #[test]
    fn decode_skips_non_message_drawer() {
        use trusty_common::memory_core::palace::Drawer;
        use uuid::Uuid;
        let d = Drawer::new(Uuid::new_v4(), "not a message");
        assert!(Message::from_drawer(&d).is_none());
    }

    #[test]
    fn formatted_message_includes_from_purpose_and_body() {
        use trusty_common::memory_core::palace::Drawer;
        use uuid::Uuid;
        let mut d = Drawer::new(Uuid::new_v4(), "the body");
        let ts = Utc::now();
        d.tags = build_message_tags("alpha", "beta", "request", ts);
        let m = Message::from_drawer(&d).unwrap();
        let formatted = m.to_injection_block();
        assert!(formatted.contains("alpha"));
        assert!(formatted.contains("beta"));
        assert!(formatted.contains("request"));
        assert!(formatted.contains("the body"));
    }

    #[test]
    fn slug_derivation_cases() {
        // Basic lowercase + hyphenation.
        assert_eq!(slugify_string("trusty-tools"), "trusty-tools");
        assert_eq!(slugify_string("Trusty_Tools"), "trusty-tools");
        assert_eq!(slugify_string("trusty tools"), "trusty-tools");
        assert_eq!(slugify_string("  trusty   tools  "), "trusty-tools");
        // Git suffix stripped.
        assert_eq!(slugify_string("trusty-tools.git"), "trusty-tools");
        // Non-alphanumerics stripped.
        assert_eq!(slugify_string("trusty/tools!"), "trustytools");
        // Multiple consecutive hyphens collapse.
        assert_eq!(slugify_string("foo--bar"), "foo-bar");
        // Pure unicode -> empty (caller must guard).
        assert_eq!(slugify_string("漢字"), "");

        // Path-based variants pick the basename.
        assert_eq!(
            slugify_for_palace(Path::new("/home/u/projects/Trusty_Tools")).unwrap(),
            "trusty-tools"
        );
    }

    #[serial_test::serial]
    #[test]
    fn cwd_palace_slug_uses_git_toplevel() {
        // Issue #1217: a repo *without* an origin remote must derive from the
        // git toplevel via the parent/dir slug — and must NOT take the nested
        // sub-directory name. (With an origin remote it would use owner/repo;
        // that path is covered by `cwd_palace_slug_at_uses_git_owner_repo`.)
        let _guard = EnvGuard::clear(crate::palace_id_derive::PALACE_OVERRIDE_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        // Init a fake repo (no remote) so the test is hermetic.
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .status();
        if status.map(|s| s.success()).unwrap_or(false) {
            // Create a sub-directory so we can confirm we resolve back to
            // the toplevel and not to the sub-dir name.
            let nested = tmp.path().join("nested-area");
            std::fs::create_dir_all(&nested).unwrap();
            let slug = cwd_palace_slug_at(&nested).expect("slug");
            // The toplevel parent/dir slug must end with the toplevel basename,
            // never the nested directory name.
            assert_ne!(slug, "nested-area", "slug must come from git toplevel");
            assert!(
                !slug.contains("nested-area"),
                "slug must not include the nested sub-dir; got {slug}"
            );
        }
    }

    #[serial_test::serial]
    #[test]
    fn cwd_palace_slug_falls_back_to_parent_dir() {
        // Issue #1217: a non-git directory derives `parent-leaf`, not just the
        // bare leaf basename. The tempdir's own basename is random, so assert
        // the slug ends with `-my-project` and is not the bare leaf.
        let _guard = EnvGuard::clear(crate::palace_id_derive::PALACE_OVERRIDE_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("my-project");
        std::fs::create_dir_all(&dir).unwrap();
        // Not a git repo — must fall back to the parent/dir slug.
        let slug = cwd_palace_slug_at(&dir).expect("slug");
        assert!(
            slug.ends_with("-my-project"),
            "non-git dir must derive `<parent>-my-project`; got {slug}"
        );
    }

    /// Why: the `TRUSTY_MEMORY_PALACE` env override must beat every derivation
    /// source (issue #1217 precedence level 1).
    /// What: set the env var, call `cwd_palace_slug_at` from a plain dir, assert
    /// the slugified override is returned regardless of the directory name.
    /// Test: itself (serialised against other env-mutating tests via the var).
    #[serial_test::serial]
    #[test]
    fn cwd_palace_slug_at_env_override_wins() {
        let _guard = EnvGuard::set(crate::palace_id_derive::PALACE_OVERRIDE_ENV, "My Override");
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("some-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let slug = cwd_palace_slug_at(&dir).expect("slug");
        assert_eq!(
            slug, "my-override",
            "env override must win and be slugified"
        );
    }

    /// Why: a git repo *with* an origin remote must derive the default palace
    /// from the GitHub-style `owner/repo` path (issue #1217 precedence level 2).
    /// What: init a repo, add an SSH origin remote, call from a subdirectory,
    /// assert the slug is `acme-widget` (owner-repo, separator collapsed).
    /// Test: itself.
    #[serial_test::serial]
    #[test]
    fn cwd_palace_slug_at_uses_git_owner_repo() {
        let _guard = EnvGuard::clear(crate::palace_id_derive::PALACE_OVERRIDE_ENV);
        // Skip when `git` is unavailable on PATH.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .ok()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping cwd_palace_slug_at_uses_git_owner_repo: git not on PATH");
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?} failed");
        };
        run(&["init", "-q"]);
        run(&["remote", "add", "origin", "git@github.com:acme/widget.git"]);
        let nested = root.join("crates").join("foo");
        std::fs::create_dir_all(&nested).unwrap();
        let slug = cwd_palace_slug_at(&nested).expect("slug");
        assert_eq!(
            slug, "acme-widget",
            "git owner/repo must drive the default palace id; got {slug}"
        );
    }

    /// Why: Change 1 — when a `.trusty-tools/trusty-memory.yaml` pin file is
    /// present, `cwd_palace_slug_at` must return the pinned slug even when the
    /// directory basename differs. This is the core rename-safety guarantee.
    /// What: create a root dir named `actual-dir`, write a pin with
    /// `palace: pinned-name`, call `cwd_palace_slug_at`, assert result is
    /// `pinned-name` not `actual-dir`.
    /// Test: itself; serialised against all other env-mutating tests via
    /// `#[serial_test::serial]` + `EnvGuard::clear` so that a concurrently
    /// racing `cwd_palace_slug_at_env_override_wins` cannot leak
    /// `TRUSTY_MEMORY_PALACE` into this test (env check is step 1, pin file
    /// is step 2 — a leaked env var would shadow the pin and return
    /// "my-override" instead of "pinned-name").
    #[serial_test::serial]
    #[test]
    fn cwd_palace_slug_at_prefers_pin_file() {
        use crate::project_root::{write_project_pin, ProjectPin, PIN_SCHEMA_VERSION};
        let _guard = EnvGuard::clear(crate::palace_id_derive::PALACE_OVERRIDE_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("actual-dir");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "pinned-name".to_string(),
            note: None,
        };
        write_project_pin(&root, &pin).expect("write pin");

        let slug = cwd_palace_slug_at(&root).expect("slug");
        assert_eq!(
            slug, "pinned-name",
            "pin file must override the directory basename in messaging slug resolution"
        );
    }

    /// Why: `cwd_palace_slug_at` must walk upward to find the pin file, so
    /// calling it from a subdirectory resolves to the same pinned slug as
    /// calling it from the root — consistent with how `prompt-context` runs.
    /// What: pin file at root, call from a nested subdirectory, assert pinned
    /// slug is returned.
    /// Test: itself; serialised against env-mutating sibling tests so that
    /// `TRUSTY_MEMORY_PALACE` (step-1 precedence) cannot leak and shadow the
    /// pin-file result (step-2 precedence).
    #[serial_test::serial]
    #[test]
    fn cwd_palace_slug_at_reads_pin_from_subdir() {
        use crate::project_root::{write_project_pin, ProjectPin, PIN_SCHEMA_VERSION};
        let _guard = EnvGuard::clear(crate::palace_id_derive::PALACE_OVERRIDE_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("my-repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let pin = ProjectPin {
            schema_version: PIN_SCHEMA_VERSION,
            palace: "my-repo".to_string(),
            note: None,
        };
        write_project_pin(&root, &pin).expect("write pin");

        let sub = root.join("crates").join("foo");
        std::fs::create_dir_all(&sub).unwrap();
        let slug = cwd_palace_slug_at(&sub).expect("slug from subdir");
        assert_eq!(slug, "my-repo");
    }

    /// Why: the hook path must not create a pin file as a side-effect of
    /// resolving the slug — `cwd_palace_slug_at` delegates to the readonly
    /// variant so no file write occurs even when no pin exists.
    /// What: call `cwd_palace_slug_at` from a git repo with no pin file;
    /// assert the pin file is absent after the call.
    /// Test: itself; serialised so that a leaked `TRUSTY_MEMORY_PALACE` env
    /// var cannot short-circuit step 1 and return early before the pin-file
    /// absence check at step 2.
    #[serial_test::serial]
    #[test]
    fn cwd_palace_slug_at_pin_read_does_not_create_pin_file() {
        use crate::project_root::{read_project_pin, PIN_FILE_REL};
        let _guard = EnvGuard::clear(crate::palace_id_derive::PALACE_OVERRIDE_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("no-pin-project");
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let pin_path = root.join(PIN_FILE_REL);
        assert!(!pin_path.exists(), "no pin before call");

        let _slug = cwd_palace_slug_at(&root).expect("slug");

        assert!(
            !pin_path.exists(),
            "cwd_palace_slug_at must NOT create a pin file (uses readonly variant)"
        );
        // But a pin read on the root itself must still return None.
        assert!(read_project_pin(&root).unwrap().is_none());
    }

    #[tokio::test]
    async fn round_trip_send_and_inbox() {
        let (registry, handle_b, root) = fresh_palace("beta");
        // Sender writes into "beta" with from="alpha".
        let id = send_message_to_palace(
            &registry,
            &root,
            "alpha",
            "beta",
            "task",
            "hello".into(),
            test_creator(),
        )
        .await
        .expect("send");
        // Inbox-check at beta returns the new message exactly once.
        let unread = list_unread_messages(&handle_b);
        assert_eq!(unread.len(), 1, "first inbox check returns the message");
        assert_eq!(unread[0].id, id);
        assert_eq!(unread[0].from_palace, "alpha");
        assert_eq!(unread[0].to_palace, "beta");
        assert_eq!(unread[0].purpose, "task");
        assert_eq!(unread[0].content, "hello");
        // Mark read.
        let flipped = mark_message_read(&handle_b, id).await.expect("mark");
        assert!(flipped);
        // Second inbox check returns nothing.
        let after = list_unread_messages(&handle_b);
        assert!(after.is_empty(), "second inbox check is empty after mark");
        // list_messages with unread_only=false still surfaces it.
        let all = list_messages(&handle_b, false);
        assert_eq!(all.len(), 1, "history view retains the read message");
        assert!(all[0].read, "history view reports it as read");
    }

    #[tokio::test]
    async fn inbox_returns_only_unread_after_mark() {
        let (registry, handle, root) = fresh_palace("inbox-only");
        // Send 3 messages.
        let mut ids = Vec::new();
        for i in 0..3 {
            let id = send_message_to_palace(
                &registry,
                &root,
                "alpha",
                "inbox-only",
                "task",
                format!("body {i}"),
                test_creator(),
            )
            .await
            .expect("send");
            ids.push(id);
        }
        // Mark the middle one read.
        mark_message_read(&handle, ids[1]).await.expect("mark");
        // unread_only=true: 2 messages.
        let unread = list_messages(&handle, true);
        assert_eq!(unread.len(), 2);
        assert!(!unread.iter().any(|m| m.id == ids[1]));
        // unread_only=false: all 3.
        let all = list_messages(&handle, false);
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn mark_read_is_idempotent() {
        let (registry, handle, root) = fresh_palace("idempotent");
        let id = send_message_to_palace(
            &registry,
            &root,
            "alpha",
            "idempotent",
            "task",
            "msg".into(),
            test_creator(),
        )
        .await
        .expect("send");
        assert!(mark_message_read(&handle, id).await.unwrap());
        // Re-mark — must not error and must report "already read".
        assert!(!mark_message_read(&handle, id).await.unwrap());
    }

    #[tokio::test]
    async fn mark_read_is_atomic_under_concurrency() {
        // Two concurrent inbox-check style flows on the same palace must
        // not double-deliver: exactly one call flips the flag, the other
        // sees `read=true` and returns `false`. The `parking_lot::RwLock`
        // on `handle.drawers` serialises the compare-and-swap.
        let (registry, handle, root) = fresh_palace("concurrent");
        let id = send_message_to_palace(
            &registry,
            &root,
            "alpha",
            "concurrent",
            "task",
            "race".into(),
            test_creator(),
        )
        .await
        .expect("send");
        // Two concurrent async tasks race on the same drawer. The
        // parking_lot write lock inside `mark_message_read` serialises the
        // compare-and-swap so exactly one observes `read=false`.
        let h1 = handle.clone();
        let h2 = handle.clone();
        let (a, b) = tokio::join!(
            async move { mark_message_read(&h1, id).await },
            async move { mark_message_read(&h2, id).await }
        );
        let a = a.expect("mark a");
        let b = b.expect("mark b");
        // Exactly one of the two flips the flag.
        let total_flips = a as u8 + b as u8;
        assert_eq!(total_flips, 1, "exactly one mark must flip the flag");

        // Exactly one message remains, and it is read.
        let after = list_messages(&handle, false);
        assert_eq!(after.len(), 1, "exactly one message survives the race");
        assert!(after[0].read, "survivor is marked read");
        // Unread inbox is empty.
        let unread = list_unread_messages(&handle);
        assert!(unread.is_empty());
    }
}
