//! Runtime-editable banner art loader.
//!
//! Why: operators want to swap the splash art without rebuilding the binary;
//! shipping the embedded default as both the compile-time fallback and the
//! first-run seed file gives a zero-setup experience while still allowing
//! per-machine customisation.
//! What: `load_banner_art` checks `TRUSTY_MPM_BANNER_FILE` (env override), then
//! `~/.trusty-mpm/banner.txt` (user-editable), then falls back to the embedded
//! compile-time default. On first run (file absent) the default is written to
//! `~/.trusty-mpm/banner.txt` so the user can discover and edit it. On every
//! run where the home-dir file exists, `refresh_if_legacy` (below) checks
//! whether its content is still byte-identical (modulo whitespace trimming)
//! to a *previous* embedded default recorded in `legacy`; if so the file is
//! transparently rewritten to the current `DEFAULT_BANNER_ART` so shipped art
//! updates actually reach users who never customised the seed file.
//! Test: `banner_source_*` in the inline `tests` module below.

/// Embedded compile-time default banner art.
///
/// Why: a fresh install must always render something without touching disk.
/// What: the block-robot design shared with `trusty-agents`' REPL splash
/// (issue #3326) — sourced from `trusty_common::banner::TRUSTY_SPLASH_ART`
/// (the single source of truth) instead of a locally embedded copy, so the
/// two binaries can never drift apart again.
/// Test: `banner_source_embedded_fallback_is_nonempty`.
pub(crate) const DEFAULT_BANNER_ART: &str = trusty_common::banner::TRUSTY_SPLASH_ART;

/// The two environment values banner resolution depends on, captured as data
/// instead of read from `std::env` at each use.
///
/// Why (#5544): the resolution rules can only be tested by controlling `$HOME`
/// and `$TRUSTY_MPM_BANNER_FILE`, and the obvious way to do that —
/// `std::env::set_var` in the test — mutates PROCESS-GLOBAL state that every
/// other test in the `tm` test binary sees for as long as it is set. `cargo
/// test` runs tests as threads in one process, so a restore-on-drop guard
/// bounds the leak's lifetime but not its visibility, and `#[serial]` only
/// excludes other `#[serial]` tests. `$HOME` is the worst variable to leak this
/// way because it is read TRANSITIVELY — `dirs::home_dir`, `FrameworkPaths`,
/// and the three-tier agent-roster scan all consult it — so the set of tests
/// that can observe a repoint is unbounded and cannot be enumerated. Passing
/// the values in removes the global mutation rather than scheduling around it.
/// This mirrors [`crate::commands::pm_guard_bash`]'s `PathEnv`, which fixed the
/// same class for `$TMPDIR`.
/// What: `override_file` is `$TRUSTY_MPM_BANNER_FILE`, `home` is `$HOME`, each
/// `None` when unset. [`BannerEnv::from_process`] is the one place that reads
/// the real environment, so production behavior is unchanged.
/// Test: every `banner_source_*` test builds one directly; the production entry
/// point [`load_banner_art`] goes through
/// [`BannerEnv::from_process`].
pub(crate) struct BannerEnv {
    override_file: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
}

impl BannerEnv {
    /// Read `$TRUSTY_MPM_BANNER_FILE` and `$HOME` from the running process.
    pub(crate) fn from_process() -> Self {
        Self {
            override_file: std::env::var_os("TRUSTY_MPM_BANNER_FILE"),
            home: std::env::var_os("HOME"),
        }
    }

    /// Build one from explicit values — the test constructor.
    #[cfg(test)]
    fn new(override_file: Option<&std::path::Path>, home: Option<&std::path::Path>) -> Self {
        Self {
            override_file: override_file.map(|p| p.as_os_str().to_os_string()),
            home: home.map(|p| p.as_os_str().to_os_string()),
        }
    }

    /// Resolve the home-directory banner file path.
    ///
    /// Why: `~/.trusty-mpm/banner.txt` is the canonical user-editable location.
    /// What: returns `<home>/.trusty-mpm/banner.txt` when `home` is present.
    /// Test: covered indirectly by `banner_source_*` tests.
    fn home_banner_path(&self) -> Option<std::path::PathBuf> {
        self.home.as_ref().map(|h| {
            std::path::PathBuf::from(h)
                .join(".trusty-mpm")
                .join("banner.txt")
        })
    }

    /// Resolve the active banner file path.
    ///
    /// Why: the env-var override lets CI / container environments inject art
    /// without touching the home directory; it takes precedence over the
    /// default home-dir path so per-invocation overrides work from a single env
    /// var.
    /// What: returns `override_file` when set and non-empty, else the
    /// home-directory path (may or may not exist on disk).
    /// Test: `banner_source_env_override_takes_precedence`.
    fn banner_file_path(&self) -> Option<std::path::PathBuf> {
        if let Some(path) = self.override_file.as_ref()
            && !path.is_empty()
        {
            return Some(std::path::PathBuf::from(path));
        }
        self.home_banner_path()
    }
}

/// Write the embedded default art to `<home>/.trusty-mpm/banner.txt` on first run.
///
/// Why: seeding the file makes it discoverable — the user can open
/// `~/.trusty-mpm/banner.txt` and edit it without knowing where the default
/// came from. Failure is non-fatal (read-only home, restricted container, etc.).
///
/// #5544: there is no process-reading wrapper beside this function. The only
/// production caller is [`load_banner_art_in`]'s `NotFound` arm, which already
/// holds the [`BannerEnv`] it must seed against — a second entry point that
/// re-read `$HOME` could seed a DIFFERENT file from the one just looked up.
/// What: creates `<home>/.trusty-mpm/` if absent, then atomically opens the file
/// with `create_new` (O_CREAT|O_EXCL) and writes `DEFAULT_BANNER_ART`. The
/// atomic open eliminates the TOCTOU window between an existence check and a
/// subsequent write; `AlreadyExists` is treated as a benign no-op. Never
/// overwrites an existing file.
/// Test: `banner_source_first_run_writes_default`, `banner_source_first_run_no_overwrite`.
pub(crate) fn write_default_if_absent_in(env: &BannerEnv) {
    let Some(path) = env.home_banner_path() else {
        return;
    };
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    // Atomic exclusive create: AlreadyExists → benign no-op; any other error
    // is also silently ignored (best-effort, non-fatal).
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = f.write_all(DEFAULT_BANNER_ART.as_bytes());
    }
}

/// Rewrite `path` to the current default when its content is a known-legacy,
/// never-customised seed.
///
/// Why: `write_default_if_absent_in` seeds the home-dir file once, on first run,
/// and never touches it again — by design, so per-machine customisation is
/// preserved. That design has a gap: an operator who installed `tm` months
/// ago and never opened `~/.trusty-mpm/banner.txt` has a seed file frozen at
/// whatever `DEFAULT_BANNER_ART` was on their install date, and every shipped
/// art update since is invisible to them. Comparing the on-disk content
/// against every *previous* embedded default (`legacy::KNOWN_LEGACY_DEFAULTS`)
/// distinguishes "still exactly what we shipped, unmodified" from "the user
/// changed this" — only the former is safe to overwrite. The comparison is
/// deliberately whitespace-trimmed (not raw-byte): a file that differs from a
/// legacy default only by incidental leading/trailing blank lines was never
/// meaningfully customised — refreshing it to the current art is the correct
/// outcome, not a regression, and matches the trimming `load_banner_art`
/// already applies when deciding whether a file counts as "empty".
/// What: trims `trimmed_content` (already trimmed by the caller) and compares
/// it against each trimmed entry in `legacy::KNOWN_LEGACY_DEFAULTS`. On a
/// match, overwrites `path` with `DEFAULT_BANNER_ART` and returns `true`. On
/// no match (including when the file already holds the current default),
/// returns `false` and leaves `path` untouched. On a write failure the file
/// is likewise left untouched (best-effort, non-fatal), a debug line is
/// logged, and `false` is returned so the caller falls back to serving the
/// stale-but-still-legacy `content` it already read.
/// Test: `banner_source_refresh_on_legacy_match`,
/// `banner_source_refresh_does_not_touch_custom_content`,
/// `banner_source_refresh_is_noop_on_current_default`.
fn refresh_if_legacy(path: &std::path::Path, trimmed_content: &str) -> bool {
    let is_known_legacy = super::legacy::KNOWN_LEGACY_DEFAULTS
        .iter()
        .any(|legacy| legacy.trim() == trimmed_content);
    if !is_known_legacy {
        return false;
    }
    match std::fs::write(path, DEFAULT_BANNER_ART) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(
                "failed to refresh legacy banner seed {}: {e}",
                path.display()
            );
            false
        }
    }
}

/// Load the banner art text, preferring the user-editable override file.
///
/// Why: allows operators to customise the splash art without rebuilding.
/// What: checks `TRUSTY_MPM_BANNER_FILE` env var first, then
/// `~/.trusty-mpm/banner.txt`. When neither exists, seeds the home-dir file
/// (best-effort) and returns the embedded default. When the file exists and
/// its trimmed content exactly matches a known legacy default, it is
/// transparently refreshed to the current default (see `refresh_if_legacy`)
/// before being returned. Read/parse errors are non-fatal: they fall back to
/// the embedded default with a debug log.
/// Test: `banner_source_override_file_used`, `banner_source_missing_falls_back`,
/// `banner_source_empty_falls_back`, `banner_source_env_override_takes_precedence`,
/// `banner_source_refresh_on_legacy_match`,
/// `banner_source_refresh_does_not_touch_custom_content`.
pub(crate) fn load_banner_art() -> String {
    load_banner_art_in(&BannerEnv::from_process())
}

/// [`load_banner_art`] against an explicit [`BannerEnv`].
///
/// Why (#5544): see [`BannerEnv`] — this is the seam that lets every resolution
/// branch be tested without repointing the process's `$HOME` or
/// `$TRUSTY_MPM_BANNER_FILE`.
/// What: identical to [`load_banner_art`], resolving both paths from `env`
/// instead of the live environment. The `NotFound` arm seeds through
/// [`write_default_if_absent_in`] with the SAME `env`, so a test's seeding side
/// effect lands in that test's own sandbox.
/// Test: `banner_source_override_file_used`, `banner_source_missing_falls_back`,
/// `banner_source_empty_falls_back`, `banner_source_env_override_takes_precedence`,
/// `banner_source_refresh_on_legacy_match`,
/// `banner_source_refresh_does_not_touch_custom_content`.
pub(crate) fn load_banner_art_in(env: &BannerEnv) -> String {
    let Some(path) = env.banner_file_path() else {
        return DEFAULT_BANNER_ART.to_string();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) if !content.trim().is_empty() => {
            if refresh_if_legacy(&path, content.trim()) {
                DEFAULT_BANNER_ART.to_string()
            } else {
                content
            }
        }
        Ok(_) => {
            tracing::debug!(
                "banner file is empty, using embedded default: {}",
                path.display()
            );
            DEFAULT_BANNER_ART.to_string()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // First run: seed the file so it's discoverable.
            write_default_if_absent_in(env);
            DEFAULT_BANNER_ART.to_string()
        }
        Err(e) => {
            tracing::debug!(
                "failed to read banner file {}: {e}, using embedded default",
                path.display()
            );
            DEFAULT_BANNER_ART.to_string()
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // #5544: these tests write NO process-global environment. Every value
    // `load_banner_art_in` / `write_default_if_absent_in` resolve from `$HOME` and
    // `$TRUSTY_MPM_BANNER_FILE` is injected through `BannerEnv` instead.
    //
    // The previous revision repointed both variables behind a restore-on-drop
    // guard plus `#[serial]`. Neither closes the window that matters. `cargo
    // test` runs a target's tests as threads in ONE process, so the mutation is
    // visible to every sibling for as long as it is set, and `#[serial]` only
    // excludes other `#[serial]` tests — the default group these joined
    // serialises them against each other and against nothing else. A `$HOME`
    // repoint straddling a non-serial sibling's roster scan is what produced
    // `REAL=43 FAKED=38` in #5544, missing exactly the five agents carried only
    // by the `~/.claude/agents` tier.
    //
    // `#[serial]` is correspondingly gone: there is nothing left to serialise,
    // and these tests now run fully parallel. `bin_target_writes_no_home_env`
    // in `tests_env_isolation.rs` is what keeps it that way.

    /// Build a `BannerEnv` over `dir`, with no override file.
    fn home_only(home: &std::path::Path) -> BannerEnv {
        BannerEnv::new(None, Some(home))
    }

    /// Embedded default is non-empty and contains block-robot chars.
    #[test]
    fn banner_source_embedded_fallback_is_nonempty() {
        assert!(!DEFAULT_BANNER_ART.trim().is_empty());
        assert!(
            DEFAULT_BANNER_ART.contains('█'),
            "default art should contain full-block chars"
        );
    }

    /// When the override file is present and non-empty, it is used.
    #[test]
    fn banner_source_override_file_used() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("banner.txt");
        std::fs::write(&file, "CUSTOM ART\n").unwrap();

        let art = load_banner_art_in(&BannerEnv::new(Some(&file), None));

        assert_eq!(art.trim(), "CUSTOM ART");
    }

    /// When the override file is missing, the embedded default is returned.
    ///
    /// #4407: this test must sandbox `$HOME` even though it only cares about
    /// the override path. `load_banner_art`'s `NotFound` arm — the one this
    /// test exists to exercise — calls `write_default_if_absent_in`, which
    /// resolves `<home>/.trusty-mpm/banner.txt` and WRITES to it. With the
    /// ambient `$HOME` this seeded a real file in the developer's own home
    /// directory. Pointing the injected home at this test's own tempdir
    /// contains the write and additionally asserts it, so the seeding side
    /// effect is covered rather than merely tolerated.
    #[test]
    fn banner_source_missing_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("no-such.txt");
        let fake_home = dir.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();

        let art = load_banner_art_in(&BannerEnv::new(Some(&file), Some(&fake_home)));
        let seeded = std::fs::read_to_string(fake_home.join(".trusty-mpm").join("banner.txt")).ok();

        assert_eq!(art, DEFAULT_BANNER_ART);
        assert_eq!(
            seeded.as_deref(),
            Some(DEFAULT_BANNER_ART),
            "the NotFound arm seeds <home>/.trusty-mpm/banner.txt — it must land in \
             THIS test's sandbox, never the ambient home (#4407)"
        );
    }

    /// When the override file exists but is whitespace-only, the embedded
    /// default is returned.
    #[test]
    fn banner_source_empty_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty.txt");
        std::fs::write(&file, "   \n   \n").unwrap();

        let art = load_banner_art_in(&BannerEnv::new(Some(&file), None));

        assert_eq!(art, DEFAULT_BANNER_ART);
    }

    /// An empty override value is ignored and the home path is used.
    ///
    /// Why: `banner_file_path` treats an empty `$TRUSTY_MPM_BANNER_FILE` as
    /// unset. That branch was previously unreachable from a test, because
    /// `set_var` with an empty value and `remove_var` were indistinguishable
    /// once the restore ran; injecting the value makes it directly assertable.
    #[test]
    fn banner_source_empty_override_value_falls_through_to_home() {
        let dir = tempfile::tempdir().unwrap();
        let fake_home = dir.path().join("home");
        std::fs::create_dir_all(fake_home.join(".trusty-mpm")).unwrap();
        std::fs::write(
            fake_home.join(".trusty-mpm").join("banner.txt"),
            "HOME ART\n",
        )
        .unwrap();

        let env = BannerEnv {
            override_file: Some(std::ffi::OsString::new()),
            home: Some(fake_home.as_os_str().to_os_string()),
        };

        assert_eq!(load_banner_art_in(&env).trim(), "HOME ART");
    }

    /// Env-var path takes precedence over the home-dir path.
    #[test]
    fn banner_source_env_override_takes_precedence() {
        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("env-banner.txt");
        std::fs::write(&env_file, "ENV ART\n").unwrap();

        // A fake home carrying a DIFFERENT banner, so precedence is observable.
        let fake_home = dir.path().join("home");
        std::fs::create_dir_all(fake_home.join(".trusty-mpm")).unwrap();
        std::fs::write(
            fake_home.join(".trusty-mpm").join("banner.txt"),
            "HOME ART\n",
        )
        .unwrap();

        let art = load_banner_art_in(&BannerEnv::new(Some(&env_file), Some(&fake_home)));

        assert_eq!(art.trim(), "ENV ART");
    }

    /// First run: default art is written to `<home>/.trusty-mpm/banner.txt`.
    #[test]
    fn banner_source_first_run_writes_default() {
        let dir = tempfile::tempdir().unwrap();
        let fake_home = dir.path().join("fresh-home");
        std::fs::create_dir_all(&fake_home).unwrap();

        write_default_if_absent_in(&home_only(&fake_home));

        let written = std::fs::read_to_string(fake_home.join(".trusty-mpm").join("banner.txt"))
            .expect("default banner.txt should be written on first run");
        assert_eq!(written, DEFAULT_BANNER_ART);
    }

    /// First run does not overwrite an existing file.
    #[test]
    fn banner_source_first_run_no_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let fake_home = dir.path().join("existing-home");
        let banner_dir = fake_home.join(".trusty-mpm");
        std::fs::create_dir_all(&banner_dir).unwrap();
        let banner_path = banner_dir.join("banner.txt");
        std::fs::write(&banner_path, "KEEP ME\n").unwrap();

        write_default_if_absent_in(&home_only(&fake_home));

        assert_eq!(std::fs::read_to_string(&banner_path).unwrap(), "KEEP ME\n");
    }

    /// With no home at all, seeding is a silent no-op rather than a panic.
    #[test]
    fn banner_source_no_home_is_a_noop() {
        write_default_if_absent_in(&BannerEnv::new(None, None));
        assert_eq!(
            load_banner_art_in(&BannerEnv::new(None, None)),
            DEFAULT_BANNER_ART
        );
    }

    /// A banner file holding a known-legacy default (never customised by the
    /// user) is transparently refreshed to the current `DEFAULT_BANNER_ART`
    /// on load — this is the fix for the stale-seed shadowing bug: a user
    /// who installed months ago and never edited their seed file must still
    /// see shipped art updates.
    #[test]
    fn banner_source_refresh_on_legacy_match() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("banner.txt");
        std::fs::write(&file, super::super::legacy::LEGACY_PRE_1907).unwrap();

        let art = load_banner_art_in(&BannerEnv::new(Some(&file), None));
        let on_disk_after = std::fs::read_to_string(&file).unwrap();

        assert_eq!(
            art, DEFAULT_BANNER_ART,
            "load_banner_art must return the refreshed current default"
        );
        assert_eq!(
            on_disk_after, DEFAULT_BANNER_ART,
            "the on-disk legacy seed file must be rewritten to the current default"
        );
    }

    /// A banner file whose content does not match any known legacy default
    /// (i.e. the user customised it) must never be touched.
    #[test]
    fn banner_source_refresh_does_not_touch_custom_content() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("banner.txt");
        std::fs::write(&file, "MY CUSTOM ROBOT ART\n").unwrap();

        let art = load_banner_art_in(&BannerEnv::new(Some(&file), None));
        let on_disk_after = std::fs::read_to_string(&file).unwrap();

        assert_eq!(
            art.trim(),
            "MY CUSTOM ROBOT ART",
            "custom content must be returned unchanged"
        );
        assert_eq!(
            on_disk_after, "MY CUSTOM ROBOT ART\n",
            "custom content on disk must never be rewritten"
        );
    }

    /// A banner file already holding the current default is a no-op refresh
    /// (not a known *previous* legacy default, so no rewrite happens — and
    /// none is needed since it already matches).
    #[test]
    fn banner_source_refresh_is_noop_on_current_default() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("banner.txt");
        std::fs::write(&file, DEFAULT_BANNER_ART).unwrap();

        let art = load_banner_art_in(&BannerEnv::new(Some(&file), None));

        assert_eq!(art, DEFAULT_BANNER_ART);
    }
}
