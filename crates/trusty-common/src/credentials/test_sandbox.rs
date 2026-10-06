//! A credential sandbox for tests: an empty credential environment, a temp
//! `$HOME` and config dirs, and a `.env.local` loader that reads nothing
//! (#9123).
//!
//! Why: a credential test that removes `FOO_API_KEY` to exercise the "absent"
//! path falls through to the next tier — the developer's real `.env.local`,
//! the `0600` store under the real `$HOME`, a real `gh` config. An
//! `assert_eq!` on what it resolved then prints a live credential. The #9123
//! audit found 24 tests across five crates with that shape, each hand-rolling
//! a partial guard. This is the one guard they share.
//! What: [`CredentialSandbox::enter`] removes every environment variable that
//! holds or locates a credential: a registered credential variable, a name
//! carrying a credential marker (`TOKEN`, `SECRET`, `_KEY`, `AUTH`, …), a
//! config-dir redirect, and any value that is a URL with embedded userinfo.
//! It also removes git config injected through the environment
//! (`GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>`, `GIT_CONFIG_VALUE_<n>`,
//! `GIT_CONFIG_PARAMETERS`) as one set, so git in the sandbox starts clean
//! (#9222). It points `HOME`, the XDG dirs, `GH_CONFIG_DIR` and
//! `TRUSTY_DATA_DIR_OVERRIDE` at a fresh temp directory, sets
//! `GIT_CONFIG_NOSYSTEM=1` and `GIT_TERMINAL_PROMPT=0`, latches the
//! `.env.local` loader so it never reads a file, and verifies the result — a
//! sandbox that cannot be set up panics rather than run the test against
//! ambient state. While one is live, `default_store` skips the OS keychain and
//! `env_local_value` reads nothing (`is_active`). Drop restores every
//! variable it touched.
//! [`assert_secret_eq`] compares a resolved value and prints only a redacted
//! preview on failure.
//!
//! The clear is by credential shape, not a full `env_clear`: in-process test
//! threads share one environment, and emptying `PATH` or `TMPDIR` under a
//! concurrently running test breaks that test. A test that spawns a process
//! gives the child `env_clear` plus an allowlist instead.
//!
//! Every caller MUST be `#[serial]` — the unkeyed group, which a keyed
//! `#[serial(name)]` group does not exclude: the sandbox mutates the process
//! environment, which other test threads read.
//!
//! Test: `the_sandbox_clears_credentials_and_restores_them`,
//! `the_sandbox_reads_no_env_local`, `the_sandbox_hides_env_local_and_the_keychain`,
//! `the_sandbox_clears_git_config_injection_and_restores_it`,
//! `a_failed_secret_assert_never_prints_the_value`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::redact_secret;

/// Name fragments that mark a variable as a credential, or as a pointer to one
/// (`*_TOKEN_FILE`, `SSH_AUTH_SOCK`, `GIT_ASKPASS`).
const SECRET_MARKERS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASS",
    "_KEY",
    "CREDENTIAL",
    "AUTH",
    "COOKIE",
    "SESSION",
];

/// Variables that redirect a tool to a config directory holding credentials.
const CONFIG_REDIRECTS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "DOCKER_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "NETRC",
];

/// Whether a variable holds or locates a credential, by name or by value.
fn looks_secret(name: &str, value: &OsStr) -> bool {
    let upper = name.to_ascii_uppercase();
    SECRET_MARKERS.iter().any(|m| upper.contains(m))
        || upper.ends_with("_PAT")
        || CONFIG_REDIRECTS.contains(&upper.as_str())
        || crate::credential_registry::is_registered_credential_env_var(name)
        || value.to_str().is_some_and(has_url_userinfo)
}

/// Whether a variable injects git config: `GIT_CONFIG_COUNT`, its numbered
/// `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>` pairs, or
/// `GIT_CONFIG_PARAMETERS`.
// #9222: the `_KEY` marker stripped only the keys, so a tm session's
// `GIT_CONFIG_COUNT=2` failed every sandboxed git with "missing config key".
fn injects_git_config(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "GIT_CONFIG_COUNT"
        || upper == "GIT_CONFIG_PARAMETERS"
        || upper.starts_with("GIT_CONFIG_KEY_")
        || upper.starts_with("GIT_CONFIG_VALUE_")
}

/// Whether the sandbox clears a variable: a credential, or injected git config.
fn clears(name: &str, value: &OsStr) -> bool {
    looks_secret(name, value) || injects_git_config(name)
}

/// Whether `value` carries a `scheme://user[:secret]@host` authority.
fn has_url_userinfo(value: &str) -> bool {
    value.match_indices("://").any(|(at, _)| {
        let rest = &value[at + 3..];
        let authority = rest.split(['/', '?', '#', ' ']).next().unwrap_or("");
        authority.contains('@')
    })
}

/// Distinguishes two sandboxes created in the same nanosecond.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Live sandboxes in this process. Only [`CredentialSandbox`] moves it, and
/// this module exists only in a test build or under `credential-test-sandbox`,
/// so a production
/// build has no way to raise it.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Whether a sandbox is live: the keychain tier of `default_store` and
/// `env_local_value` answer nothing while one is (#9123).
pub(crate) fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst) > 0
}

/// An isolated credential environment for one test. See the module docs.
///
/// Why: the type's lifetime is the isolation window; dropping it restores the
/// environment and deletes the temp tree.
#[must_use = "the sandbox is torn down when this value drops"]
pub struct CredentialSandbox {
    /// The temp tree holding `home/`, `config/`, `data/` and `cache/`.
    root: PathBuf,
    /// `(name, value before the sandbox first touched it)`, in touch order.
    saved: Vec<(OsString, Option<OsString>)>,
}

impl CredentialSandbox {
    /// Enter the sandbox. The caller must be `#[serial]`.
    ///
    /// Why/What: see the module docs.
    /// # Panics
    /// When the temp tree cannot be created, or when the isolation check that
    /// closes [`Self::enter`] fails — a test never runs half-isolated.
    #[track_caller]
    pub fn enter() -> Self {
        let root = std::env::temp_dir().join(format!(
            "trusty-credential-sandbox-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        for sub in ["home", "config/gh", "data", "cache"] {
            std::fs::create_dir_all(root.join(sub)).unwrap_or_else(|e| {
                panic!("credential sandbox: cannot create {}: {e}", root.display())
            });
        }
        // Raised as the value whose drop lowers it is built, so a panic in
        // the setup below still lowers it exactly once.
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        let mut sandbox = Self {
            root,
            saved: Vec::new(),
        };
        let ambient: Vec<OsString> = std::env::vars_os()
            .filter(|(k, v)| k.to_str().is_none_or(|k| clears(k, v)))
            .map(|(k, _)| k)
            .collect();
        for name in ambient {
            sandbox.remove_os(&name);
        }
        let root = sandbox.root.clone();
        sandbox.set("HOME", root.join("home"));
        sandbox.set("XDG_CONFIG_HOME", root.join("config"));
        sandbox.set("XDG_DATA_HOME", root.join("data"));
        sandbox.set("XDG_CACHE_HOME", root.join("cache"));
        sandbox.set("GH_CONFIG_DIR", root.join("config/gh"));
        sandbox.set("TRUSTY_DATA_DIR_OVERRIDE", root.join("data"));
        // Git reads no system config (credential helpers, `insteadOf`
        // rewrites) and never prompts for a credential.
        sandbox.set("GIT_CONFIG_NOSYSTEM", "1");
        sandbox.set("GIT_TERMINAL_PROMPT", "0");
        // Either latched here, or an earlier load ran and the clear above
        // already removed whatever it put in the environment.
        let _ = super::dotenv::skip_env_local_load();
        sandbox.verify();
        sandbox
    }

    /// Fail closed when the sandbox did not take.
    #[track_caller]
    fn verify(&self) {
        let home = self.home();
        assert!(
            dirs::home_dir().as_deref() == Some(home.as_path()),
            "credential sandbox: $HOME did not move to {}",
            home.display()
        );
        let leaked: Vec<String> = std::env::vars_os()
            .filter(|(k, v)| k.to_str().is_none_or(|k| clears(k, v)))
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(
            leaked.is_empty(),
            "credential sandbox: cleared variables survived (names only): {leaked:?}"
        );
    }

    /// The sandbox's `$HOME`.
    pub fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// The sandbox's temp root, for a test's own scratch files.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Set `key` for the rest of the sandbox's life.
    pub fn set(&mut self, key: &str, value: impl AsRef<OsStr>) {
        self.remember(OsStr::new(key));
        // SAFETY: the caller is `#[serial]` (module contract); restored on drop.
        unsafe { std::env::set_var(key, value) };
    }

    /// Remove `key` for the rest of the sandbox's life.
    pub fn remove(&mut self, key: &str) {
        self.remove_os(OsStr::new(key));
    }

    fn remove_os(&mut self, key: &OsStr) {
        self.remember(key);
        // SAFETY: as `set`.
        unsafe { std::env::remove_var(key) };
    }

    /// Record `key`'s value before its first change, so drop restores it.
    fn remember(&mut self, key: &OsStr) {
        if !self.saved.iter().any(|(k, _)| k == key) {
            self.saved.push((key.to_os_string(), std::env::var_os(key)));
        }
    }
}

impl Drop for CredentialSandbox {
    fn drop(&mut self) {
        for (key, prior) in self.saved.drain(..).rev() {
            // SAFETY: as `set`.
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Assert a resolved credential equals `expected`, redacting `actual`.
///
/// Why: `actual` is whatever the code under test resolved, which on a broken
/// isolation is a real credential; only `expected` is a test literal.
/// Test: `a_failed_secret_assert_never_prints_the_value`.
#[track_caller]
pub fn assert_secret_eq(actual: Option<&str>, expected: Option<&str>, what: &str) {
    if actual != expected {
        let shown = actual.map_or_else(
            || "None".to_string(),
            |v| format!("Some({})", redact_secret(v)),
        );
        panic!("{what}: expected {expected:?}, got {shown} (actual value redacted)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Why: the sandbox is only worth its name if a credential set before it
    /// is invisible inside it and back afterwards.
    /// What: the unkeyed `#[serial]` here, plus the keyed groups and
    /// `data_dir::ENV_LOCK` this crate's other env tests use, held by
    /// [`clears_and_restores`] — three locks that exclude nothing of each
    /// other (#7253).
    /// Test: this test.
    #[test]
    #[serial]
    fn the_sandbox_clears_credentials_and_restores_them() {
        clears_and_restores();
    }

    #[serial(dotenv_credential_env, inference_env)]
    fn clears_and_restores() {
        let _env = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        const VAR: &str = "TRUSTY_9123_PROBE_API_KEY";
        const URL_VAR: &str = "TRUSTY_9123_PROBE_REMOTE";
        let real_home = std::env::var_os("HOME");
        // SAFETY: `#[serial]`; removed again below.
        unsafe {
            std::env::set_var(VAR, "outside-value");
            std::env::set_var(URL_VAR, "https://x-access-token:probe@example.test/o/r.git");
        }
        {
            let mut sandbox = CredentialSandbox::enter();
            assert!(std::env::var_os(VAR).is_none(), "the credential leaked in");
            assert!(
                std::env::var_os(URL_VAR).is_none(),
                "the URL credential leaked in"
            );
            assert_eq!(std::env::var_os("HOME"), Some(sandbox.home().into()));
            assert!(std::env::var_os("PATH").is_some(), "PATH must survive");
            sandbox.set(VAR, "inside-value");
            assert_eq!(std::env::var(VAR).as_deref(), Ok("inside-value"));
        }
        assert_eq!(std::env::var(VAR).as_deref(), Ok("outside-value"));
        assert!(
            std::env::var_os(URL_VAR).is_some(),
            "the URL variable was not restored"
        );
        assert_eq!(std::env::var_os("HOME"), real_home);
        // SAFETY: as above.
        unsafe {
            std::env::remove_var(VAR);
            std::env::remove_var(URL_VAR);
        }
    }

    /// Why: a tm session exports `GIT_CONFIG_COUNT` with numbered key/value
    /// pairs; a sandbox that strips only the keys leaves git unable to start
    /// ("missing config key GIT_CONFIG_KEY_0", #9222).
    /// What: injects a count, two pairs and `GIT_CONFIG_PARAMETERS`, then runs
    /// git inside the sandbox: git succeeds, sees no injected key, every
    /// variable is gone, and drop restores each value exactly.
    /// Test: this test.
    #[test]
    #[serial]
    fn the_sandbox_clears_git_config_injection_and_restores_it() {
        clears_git_config_injection();
    }

    #[serial(dotenv_credential_env, inference_env)]
    fn clears_git_config_injection() {
        let _env = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        const INJECTED: &[(&str, &str)] = &[
            ("GIT_CONFIG_COUNT", "2"),
            ("GIT_CONFIG_KEY_0", "trusty.probe9222a"),
            ("GIT_CONFIG_VALUE_0", "zero"),
            ("GIT_CONFIG_KEY_1", "trusty.probe9222b"),
            ("GIT_CONFIG_VALUE_1", "one"),
            ("GIT_CONFIG_PARAMETERS", "'trusty.probe9222c'='two'"),
        ];
        let caller: Vec<_> = INJECTED
            .iter()
            .map(|(k, _)| (*k, std::env::var_os(k)))
            .collect();
        for (k, v) in INJECTED {
            // SAFETY: `#[serial]`; the caller's values are put back below.
            unsafe { std::env::set_var(k, v) };
        }
        {
            let sandbox = CredentialSandbox::enter();
            let repo = sandbox.root().join("repo");
            std::fs::create_dir(&repo).expect("create the repo dir");
            let git = |args: &[&str]| {
                let out = crate::git::command_in(&repo)
                    .args(args)
                    .output()
                    .expect("spawn git");
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(out.status.success(), "git {args:?} failed: {stderr}");
                String::from_utf8_lossy(&out.stdout).into_owned()
            };
            git(&["init", "-q"]);
            let listed = git(&["config", "--list"]);
            assert!(
                !listed.contains("probe9222"),
                "git read injected config inside the sandbox: {listed}"
            );
            for (k, _) in INJECTED {
                assert!(std::env::var_os(k).is_none(), "{k} is visible inside");
            }
        }
        for (k, v) in INJECTED {
            assert_eq!(std::env::var(k).as_deref(), Ok(*v), "{k} not restored");
        }
        for (k, prior) in caller {
            // SAFETY: as above.
            unsafe {
                match prior {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// Why: the loader reads the developer's `.env.local` the first time any
    /// resolver runs; inside a sandbox it must never read one.
    /// Test: this test.
    #[test]
    #[serial]
    fn the_sandbox_reads_no_env_local() {
        reads_no_env_local();
    }

    #[serial(dotenv_credential_env, inference_env)]
    fn reads_no_env_local() {
        let _env = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let sandbox = CredentialSandbox::enter();
        std::fs::write(
            sandbox.home().join(".env.local"),
            "TRUSTY_9123_DOTENV_TOKEN=from-env-local\n",
        )
        .expect("write a user-tier .env.local");
        super::super::load_env_local_once();
        assert!(
            std::env::var_os("TRUSTY_9123_DOTENV_TOKEN").is_none(),
            "the loader read a .env.local inside the sandbox"
        );
        assert!(
            !super::super::dotenv::skip_env_local_load(),
            "the sandbox must leave the loader latched"
        );
    }

    /// Why: env vars are one tier; the keychain and a cwd-upward
    /// `.env.local` are two more, and a sandboxed test must reach neither.
    /// What: a repo-shaped temp dir binds a probe variable in `.env.local`;
    /// the read and the keychain gate answer normally outside a sandbox and
    /// nothing inside one, and git's isolation variables are set.
    /// Test: this test.
    #[test]
    #[serial]
    fn the_sandbox_hides_env_local_and_the_keychain() {
        hides_env_local_and_the_keychain();
    }

    #[serial(dotenv_credential_env, inference_env)]
    fn hides_env_local_and_the_keychain() {
        let _env = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let repo = tempfile::TempDir::new().expect("repo");
        std::fs::create_dir(repo.path().join(".git")).expect(".git");
        std::fs::write(repo.path().join(".env.local"), "TRUSTY_9123_PROBE=v\n").expect("write");
        let read = || super::super::dotenv::env_local_value_from(repo.path(), "TRUSTY_9123_PROBE");
        assert_eq!(read().as_deref(), Some("v"), "the probe must be readable");
        assert!(super::super::resolver::keychain_allowed());
        {
            let _sandbox = CredentialSandbox::enter();
            assert!(is_active());
            assert_eq!(read(), None, "a sandboxed read reached .env.local");
            assert!(!super::super::resolver::keychain_allowed());
            assert_eq!(std::env::var("GIT_CONFIG_NOSYSTEM").as_deref(), Ok("1"));
            assert_eq!(std::env::var("GIT_TERMINAL_PROMPT").as_deref(), Ok("0"));
        }
        assert!(!is_active(), "the flag outlived the sandbox");
        assert_eq!(read().as_deref(), Some("v"));
    }

    /// Why: the leak lived in the failure path; provoke it and read it.
    /// Test: this test.
    #[test]
    fn a_failed_secret_assert_never_prints_the_value() {
        let secret = "sk-9123-0123456789abcdef0123456789";
        let panic = std::panic::catch_unwind(|| {
            assert_secret_eq(Some(secret), Some("expected"), "probe");
        })
        .expect_err("a mismatch must panic");
        let msg = panic
            .downcast_ref::<String>()
            .expect("panic! with format args carries a String");
        assert!(!msg.contains(secret), "the message echoed the value");
        assert!(!msg.contains("0123456789"), "the message echoed its tail");
        assert!(msg.contains("expected"), "the message lost the expectation");
    }
}
