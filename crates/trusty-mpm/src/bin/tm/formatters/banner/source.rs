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

/// Resolve the home-directory banner file path.
///
/// Why: `~/.trusty-mpm/banner.txt` is the canonical user-editable location.
/// What: returns `$HOME/.trusty-mpm/banner.txt` when `$HOME` is set.
/// Test: covered indirectly by `banner_source_*` tests.
fn home_banner_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| {
        std::path::PathBuf::from(h)
            .join(".trusty-mpm")
            .join("banner.txt")
    })
}

/// Resolve the active banner file path.
///
/// Why: the env-var override lets CI / container environments inject art
/// without touching the home directory; it takes precedence over the default
/// home-dir path so per-invocation overrides work from a single env var.
/// What: returns `$TRUSTY_MPM_BANNER_FILE` when set and non-empty, else the
/// home-directory path (may or may not exist on disk).
/// Test: `banner_source_env_override_takes_precedence`.
fn banner_file_path() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("TRUSTY_MPM_BANNER_FILE")
        && !path.is_empty()
    {
        return Some(std::path::PathBuf::from(path));
    }
    home_banner_path()
}

/// Write the embedded default art to `~/.trusty-mpm/banner.txt` on first run.
///
/// Why: seeding the file makes it discoverable — the user can open
/// `~/.trusty-mpm/banner.txt` and edit it without knowing where the default
/// came from. Failure is non-fatal (read-only home, restricted container, etc.).
/// What: creates `~/.trusty-mpm/` if absent, then atomically opens the file
/// with `create_new` (O_CREAT|O_EXCL) and writes `DEFAULT_BANNER_ART`. The
/// atomic open eliminates the TOCTOU window between an existence check and a
/// subsequent write; `AlreadyExists` is treated as a benign no-op. Never
/// overwrites an existing file.
/// Test: `banner_source_first_run_writes_default`, `banner_source_first_run_no_overwrite`.
pub(crate) fn write_default_if_absent() {
    let Some(path) = home_banner_path() else {
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
/// Why: `write_default_if_absent` seeds the home-dir file once, on first run,
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
    let Some(path) = banner_file_path() else {
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
            write_default_if_absent();
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

    // Serialise every test that mutates $HOME or $TRUSTY_MPM_BANNER_FILE.
    // Cargo runs unit tests in parallel by default; env-var mutations are global
    // and can race. A static Mutex is the idiomatic solution without adding the
    // `serial_test` crate.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_dir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "tm-banner-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&base).expect("create temp dir");
        base
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
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir();
        let file = dir.join("banner.txt");
        std::fs::write(&file, "CUSTOM ART\n").unwrap();

        let old = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        // SAFETY: serialised by ENV_LOCK; env mutation restored before unlock.
        unsafe { std::env::set_var("TRUSTY_MPM_BANNER_FILE", &file) };
        let art = load_banner_art();
        unsafe {
            match old {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(art.trim(), "CUSTOM ART");
    }

    /// When the override file is missing, the embedded default is returned.
    #[test]
    fn banner_source_missing_falls_back() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir();
        let file = dir.join("no-such.txt");

        let old = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        // SAFETY: serialised by ENV_LOCK; env mutation restored before unlock.
        unsafe { std::env::set_var("TRUSTY_MPM_BANNER_FILE", &file) };
        let art = load_banner_art();
        unsafe {
            match old {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(art, DEFAULT_BANNER_ART);
    }

    /// When the override file exists but is whitespace-only, the embedded
    /// default is returned.
    #[test]
    fn banner_source_empty_falls_back() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir();
        let file = dir.join("empty.txt");
        std::fs::write(&file, "   \n   \n").unwrap();

        let old = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        // SAFETY: serialised by ENV_LOCK; env mutation restored before unlock.
        unsafe { std::env::set_var("TRUSTY_MPM_BANNER_FILE", &file) };
        let art = load_banner_art();
        unsafe {
            match old {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(art, DEFAULT_BANNER_ART);
    }

    /// Env-var path takes precedence over the home-dir path.
    #[test]
    fn banner_source_env_override_takes_precedence() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir();
        let env_file = dir.join("env-banner.txt");
        std::fs::write(&env_file, "ENV ART\n").unwrap();

        // Simulate a fake HOME with a different banner.
        let fake_home = dir.join("home");
        std::fs::create_dir_all(fake_home.join(".trusty-mpm")).unwrap();
        let home_file = fake_home.join(".trusty-mpm").join("banner.txt");
        std::fs::write(&home_file, "HOME ART\n").unwrap();

        let old_env = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        let old_home = std::env::var_os("HOME");
        // SAFETY: single-threaded test; env mutations are restored before return.
        unsafe {
            std::env::set_var("TRUSTY_MPM_BANNER_FILE", &env_file);
            std::env::set_var("HOME", &fake_home);
        }

        let art = load_banner_art();

        unsafe {
            match old_env {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
            match old_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(art.trim(), "ENV ART");
    }

    /// First run: default art is written to ~/.trusty-mpm/banner.txt.
    #[test]
    fn banner_source_first_run_writes_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fake_home = temp_dir().join("fresh-home");
        std::fs::create_dir_all(&fake_home).unwrap();

        let old_home = std::env::var_os("HOME");
        let old_env = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        // SAFETY: single-threaded test; env mutations are restored before return.
        unsafe {
            std::env::set_var("HOME", &fake_home);
            std::env::remove_var("TRUSTY_MPM_BANNER_FILE");
        }

        write_default_if_absent();
        let written_path = fake_home.join(".trusty-mpm").join("banner.txt");
        let written = std::fs::read_to_string(&written_path).ok();

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match old_env {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
        }
        std::fs::remove_dir_all(fake_home.parent().unwrap()).ok();

        assert!(
            written.is_some(),
            "default banner.txt should be written on first run"
        );
        assert_eq!(written.unwrap(), DEFAULT_BANNER_ART);
    }

    /// First run does not overwrite an existing file.
    #[test]
    fn banner_source_first_run_no_overwrite() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fake_home = temp_dir().join("existing-home");
        let banner_dir = fake_home.join(".trusty-mpm");
        std::fs::create_dir_all(&banner_dir).unwrap();
        let banner_path = banner_dir.join("banner.txt");
        std::fs::write(&banner_path, "KEEP ME\n").unwrap();

        let old_home = std::env::var_os("HOME");
        let old_env = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        // SAFETY: single-threaded test; env mutations are restored before return.
        unsafe {
            std::env::set_var("HOME", &fake_home);
            std::env::remove_var("TRUSTY_MPM_BANNER_FILE");
        }

        write_default_if_absent();
        let content = std::fs::read_to_string(&banner_path).unwrap();

        unsafe {
            match old_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match old_env {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
        }
        std::fs::remove_dir_all(fake_home.parent().unwrap()).ok();

        assert_eq!(content, "KEEP ME\n");
    }

    /// A banner file holding a known-legacy default (never customised by the
    /// user) is transparently refreshed to the current `DEFAULT_BANNER_ART`
    /// on load — this is the fix for the stale-seed shadowing bug: a user
    /// who installed months ago and never edited their seed file must still
    /// see shipped art updates.
    #[test]
    fn banner_source_refresh_on_legacy_match() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir();
        let file = dir.join("banner.txt");
        std::fs::write(&file, super::super::legacy::LEGACY_PRE_1907).unwrap();

        let old = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        // SAFETY: serialised by ENV_LOCK; env mutation restored before unlock.
        unsafe { std::env::set_var("TRUSTY_MPM_BANNER_FILE", &file) };
        let art = load_banner_art();
        let on_disk_after = std::fs::read_to_string(&file).unwrap();
        unsafe {
            match old {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();

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
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir();
        let file = dir.join("banner.txt");
        std::fs::write(&file, "MY CUSTOM ROBOT ART\n").unwrap();

        let old = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        // SAFETY: serialised by ENV_LOCK; env mutation restored before unlock.
        unsafe { std::env::set_var("TRUSTY_MPM_BANNER_FILE", &file) };
        let art = load_banner_art();
        let on_disk_after = std::fs::read_to_string(&file).unwrap();
        unsafe {
            match old {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();

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
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_dir();
        let file = dir.join("banner.txt");
        std::fs::write(&file, DEFAULT_BANNER_ART).unwrap();

        let old = std::env::var_os("TRUSTY_MPM_BANNER_FILE");
        // SAFETY: serialised by ENV_LOCK; env mutation restored before unlock.
        unsafe { std::env::set_var("TRUSTY_MPM_BANNER_FILE", &file) };
        let art = load_banner_art();
        unsafe {
            match old {
                Some(v) => std::env::set_var("TRUSTY_MPM_BANNER_FILE", v),
                None => std::env::remove_var("TRUSTY_MPM_BANNER_FILE"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(art, DEFAULT_BANNER_ART);
    }
}
