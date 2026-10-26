//! Keep a proven gh token in gh's own keyring entry, never in a tm
//! `hosts.yml` (#10037, owner ruling D6e).
//!
//! Why: #8914 wrote the `--account-token-stdin` token, and every token tm
//! proved for a pin, as plaintext into
//! `<state_root>/gh-accounts/<login>/hosts.yml`. D6e forbids that copy. A
//! pinned session's gh already reads its token per call from the keyring
//! through the #9750 shim (`gh auth token --hostname <host> --user <login>`),
//! so the keyring entry that lookup reads is where the token goes.
//! What: [`GhKeyringWriter`] stores a token as gh's per-login keyring entry
//! (service `gh:<host>`, account `<login>`) and reads it back. It never writes
//! gh's active-account slot (account `""`), which `gh auth login` would.
//! [`store_in_keyring`] writes that entry, then rewrites `hosts.yml` to name
//! the login with no `oauth_token` for the host. A keyring failure writes
//! nothing to `hosts.yml`; the rewrite is atomic. [`plaintext_token_files`]
//! lists every tm account dir whose `hosts.yml` still holds a token, for the
//! `gh_account_dir_tokens` doctor row.
//! Test: `gh_keyring_token_tests.rs`.

use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};

/// The keyring service gh reads a host's tokens from (gh `keyringServiceName`).
pub(crate) fn keyring_service(host: &str) -> String {
    format!("gh:{host}")
}

/// Stores a gh token in the keyring entry gh reads for one login (#10037).
///
/// Why: the OS keyring is out of bounds for tests; the trait is the seam.
/// What: `store` writes `token` as service [`keyring_service`]`(host)`,
/// account `login`, then reads it back; `Ok` only when the read-back equals
/// `token`. An `Err` never contains the token.
/// Test: `a_keyring_failure_writes_no_token_to_hosts_yml`.
pub(crate) trait GhKeyringWriter {
    /// Store `token` as `login`'s gh keyring entry on `host`, read back.
    fn store(&self, host: &str, login: &str, token: &str) -> Result<(), String>;
}

/// The production [`GhKeyringWriter`]: the macOS login Keychain through
/// `/usr/bin/security`, or the Secret Service through `secret-tool`.
///
/// Why: gh reads its macOS keyring through `/usr/bin/security`, so an item
/// that tool creates is readable by gh with no access prompt. The token goes
/// on the tool's stdin, never on its command line.
/// What: refuses a value it cannot quote ([`quoted`]); writes; reads back
/// with the same tool and compares. Bounded by
/// [`crate::core::gh_account::GH_ENFORCE_TIMEOUT`] per call, since a locked
/// keychain hangs (#6867). A unit-test build never touches the keyring, and
/// neither does a process that has latched the #9121 sandbox
/// ([`crate::secret_source::sandboxed`]).
/// Test: the gate order is `the_system_keyring_refuses_a_sandboxed_store_first`;
/// the rest needs a real keyring, so the decisions it feeds are covered
/// through `TableKeyring` in `gh_keyring_token_tests.rs`.
pub(crate) struct SystemGhKeyring;

impl GhKeyringWriter for SystemGhKeyring {
    fn store(&self, host: &str, login: &str, token: &str) -> Result<(), String> {
        Self::store_gated(crate::secret_source::sandboxed(), host, login, token)
    }
}

/// `Err` when `sandboxed`: such a process never reaches the per-user keyring.
///
/// Why (#9121): a pure decision, so a test covers both arms without setting
/// the process-wide latch under its parallel siblings.
/// Test: `refuse_in_sandbox_refuses_only_when_sandboxed`.
pub(crate) fn refuse_in_sandbox(sandboxed: bool) -> Result<(), String> {
    if sandboxed {
        return Err("a sandboxed tm never writes the keyring (#9121)".to_string());
    }
    Ok(())
}

impl SystemGhKeyring {
    /// [`GhKeyringWriter::store`] with the sandbox state injected.
    ///
    /// What: the sandbox refusal runs first, then the unit-test refusal, so a
    /// test reaches the sandbox arm.
    /// Test: `the_system_keyring_refuses_a_sandboxed_store_first`,
    /// `the_system_keyring_is_never_written_by_a_test`.
    fn store_gated(sandboxed: bool, host: &str, login: &str, token: &str) -> Result<(), String> {
        refuse_in_sandbox(sandboxed)?;
        if cfg!(test) {
            return Err("a unit-test build never writes the keyring (#10037)".to_string());
        }
        let service = keyring_service(host);
        let (svc, user, secret) = (
            quoted("host", &service)?,
            quoted("login", login)?,
            quoted("token", token)?,
        );
        platform_write(
            &service,
            login,
            token,
            &format!("{svc} -a {user} -w {secret}"),
        )?;
        // #10037: the keyring must answer with this token before hosts.yml loses it.
        if platform_read(&service, login)?.trim() != token {
            return Err(format!(
                "the keyring did not return the token tm stored for '{login}' on {host}"
            ));
        }
        Ok(())
    }
}

/// `text` single-quoted for `security -i`, or why it cannot be.
///
/// What: refuses an empty value, whitespace, a quote, a backslash or any
/// control character, so the operand can never end its quotes or a line.
/// The refusal names `what`, never the value.
fn quoted(what: &str, text: &str) -> Result<String, String> {
    let unsafe_char = |c: char| c.is_whitespace() || c.is_control() || "'\"\\".contains(c);
    if text.is_empty() || text.chars().any(unsafe_char) {
        return Err(format!(
            "the {what} holds a character tm cannot hand the keyring safely"
        ));
    }
    Ok(format!("'{text}'"))
}

/// Run `cmd` with `input` on stdin under the keyring bound; its stdout on a
/// zero exit. An `Err` names `what` and the exit, never stdin or stdout.
fn run_keyring_tool(
    cmd: std::process::Command,
    input: Option<Vec<u8>>,
    what: &str,
) -> Result<String, String> {
    use crate::core::bounded_proc::run_bounded_with_input;
    let out = run_bounded_with_input(cmd, input, crate::core::gh_account::GH_ENFORCE_TIMEOUT)
        .map_err(|e| format!("the keyring {what} {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the keyring {what} failed (exit {})",
            out.status
                .code()
                .map_or("signal".to_string(), |c| c.to_string())
        ));
    }
    Ok(out.stdout)
}

/// macOS: `security -i` reads `add-generic-password -U -s ...` from stdin.
#[cfg(target_os = "macos")]
fn platform_write(
    _service: &str,
    _login: &str,
    _token: &str,
    operands: &str,
) -> Result<(), String> {
    let mut cmd = std::process::Command::new("/usr/bin/security");
    cmd.arg("-i");
    let line = format!("add-generic-password -U -s {operands}\n");
    run_keyring_tool(cmd, Some(line.into_bytes()), "write").map(drop)
}

/// macOS: the item's password, as gh's `security find-generic-password` reads it.
#[cfg(target_os = "macos")]
fn platform_read(service: &str, login: &str) -> Result<String, String> {
    let mut cmd = std::process::Command::new("/usr/bin/security");
    cmd.args(["find-generic-password", "-s", service, "-a", login, "-w"]);
    run_keyring_tool(cmd, None, "read-back")
}

/// Linux: `secret-tool store` with gh's attributes and label; the token on stdin.
#[cfg(target_os = "linux")]
fn platform_write(service: &str, login: &str, token: &str, _operands: &str) -> Result<(), String> {
    let mut cmd = std::process::Command::new("secret-tool");
    let label = format!("Password for '{login}' on '{service}'");
    cmd.args([
        "store", "--label", &label, "service", service, "username", login,
    ]);
    run_keyring_tool(cmd, Some(token.as_bytes().to_vec()), "write").map(drop)
}

/// Linux: `secret-tool lookup` on gh's attributes.
#[cfg(target_os = "linux")]
fn platform_read(service: &str, login: &str) -> Result<String, String> {
    let mut cmd = std::process::Command::new("secret-tool");
    cmd.args(["lookup", "service", service, "username", login]);
    run_keyring_tool(cmd, None, "read-back")
}

/// Any other platform: no keyring tm can write, so the pin is refused.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_write(_: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
    Err("tm cannot write a gh keyring entry on this platform".to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_read(_: &str, _: &str) -> Result<String, String> {
    Err("tm cannot read a gh keyring entry on this platform".to_string())
}

/// Store `token` in `login`'s gh keyring entry for `host`, then make
/// `<dir>/hosts.yml` name `login` with no token for `host` (#10037).
///
/// Why: D6e — the token lives in the keyring the #9750 shim reads, never in
/// tm's `hosts.yml`. A legacy `hosts.yml` token (#8914) is migrated by the
/// same call: it reaches the keyring first, then leaves the file.
/// What: the new `hosts.yml` text is computed first, so a file that is not a
/// gh mapping is refused before the keyring is touched. Then
/// `keyring.store`; its `Err` returns at once and `hosts.yml` is not written.
/// Then the file is replaced atomically
/// ([`trusty_common::atomic_file::write_atomic`]) when it changed. Other hosts'
/// entries and every non-token key are kept.
/// Test: `a_stored_token_reaches_the_keyring_and_never_hosts_yml`,
/// `a_legacy_hosts_yml_token_is_migrated_and_other_hosts_kept`,
/// `a_keyring_failure_writes_no_token_to_hosts_yml`,
/// `a_failed_hosts_yml_rewrite_leaves_the_file_whole`,
/// `a_hosts_yml_that_is_not_a_mapping_is_refused_before_the_keyring`.
pub(crate) fn store_in_keyring(
    dir: &Path,
    host: &str,
    login: &str,
    token: &str,
    keyring: &dyn GhKeyringWriter,
) -> Result<(), String> {
    let path = dir.join("hosts.yml");
    let rendered = login_only_hosts_yml(&path, host, login)?;
    keyring.store(host, login, token)?;
    if let Some(text) = rendered {
        trusty_common::atomic_file::write_atomic(&path, text.as_bytes())
            .map_err(|e| format!("cannot write {} ({e})", path.display()))?;
    }
    Ok(())
}

/// The `hosts.yml` text naming `login` as `host`'s user with every
/// `oauth_token` for `host` removed, or `None` when the file already is that.
///
/// What: a missing or blank file is an empty mapping. A file that is not YAML,
/// or not a gh mapping, is an `Err`, never replaced.
fn login_only_hosts_yml(path: &Path, host: &str, login: &str) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{} is unreadable ({e})", path.display())),
    };
    let original: Value = if text.trim().is_empty() {
        Value::Mapping(Mapping::new())
    } else {
        serde_yaml::from_str(&text).map_err(|_| format!("{} is not YAML", path.display()))?
    };
    let mut doc = original.clone();
    let not_a_map = || format!("{} is not a gh hosts.yml mapping", path.display());
    let host_map =
        mapping_at(doc.as_mapping_mut().ok_or_else(not_a_map)?, host).ok_or_else(not_a_map)?;
    host_map.remove("oauth_token");
    host_map.insert("user".into(), login.into());
    let users = mapping_at(host_map, "users").ok_or_else(not_a_map)?;
    // gh's own keyring layout names the login with a null entry.
    users.entry(login.into()).or_insert(Value::Null);
    for (_, entry) in users.iter_mut() {
        if let Some(entry) = entry.as_mapping_mut() {
            entry.remove("oauth_token");
        }
    }
    if doc == original && !text.trim().is_empty() {
        return Ok(None);
    }
    serde_yaml::to_string(&doc)
        .map(Some)
        .map_err(|e| format!("cannot render {} ({e})", path.display()))
}

/// The mapping under `key` in `map`, created when absent or null.
fn mapping_at<'a>(map: &'a mut Mapping, key: &str) -> Option<&'a mut Mapping> {
    let slot = map.entry(key.into()).or_insert(Value::Null);
    if slot.is_null() {
        *slot = Value::Mapping(Mapping::new());
    }
    slot.as_mapping_mut()
}

/// Does gh `hosts.yml` text hold a non-empty `oauth_token` for any host?
///
/// What: a host-level `oauth_token` or any `users.<login>.oauth_token`.
/// A blank file holds none. `Err` when the text is not YAML or not a
/// mapping; never carries a value.
/// Test: `holds_token_sees_host_and_user_tokens`.
pub(crate) fn hosts_yml_holds_token(text: &str) -> Result<bool, String> {
    let doc: Value = serde_yaml::from_str(text).map_err(|_| "is not YAML".to_string())?;
    let present = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .is_some_and(|t| !t.trim().is_empty())
    };
    // #10037 fail-open check: a file gh cannot read as hosts is unknown, not clean.
    let Some(hosts) = doc.as_mapping() else {
        return if doc.is_null() {
            Ok(false)
        } else {
            Err("is not a gh hosts.yml mapping".to_string())
        };
    };
    Ok(hosts.values().any(|host| {
        present(host.get("oauth_token"))
            || host
                .get("users")
                .and_then(Value::as_mapping)
                .is_some_and(|users| users.values().any(|u| present(u.get("oauth_token"))))
    }))
}

/// Every `<state_root>/gh-accounts/*/hosts.yml` that holds a token, and every
/// one that could not be read or parsed (#10037).
///
/// Why: a dir #8914 wrote keeps its plaintext token until its login is pinned
/// again; doctor names it so the operator can.
/// What: `Ok((holding, unreadable))`; a missing `gh-accounts` dir is two empty
/// lists. An unlistable `gh-accounts` dir is an `Err`. A dir with no
/// `hosts.yml` is skipped. Paths only; never a value.
/// Test: `plaintext_token_files_names_holding_and_unreadable_files`.
pub(crate) fn plaintext_token_files(
    state_root: &Path,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let root = state_root.join(crate::core::gh_account_dir::GH_ACCOUNTS_DIR_NAME);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((vec![], vec![])),
        Err(e) => return Err(format!("{} is unreadable ({e})", root.display())),
    };
    let (mut holding, mut unreadable) = (Vec::new(), Vec::new());
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path().join("hosts.yml"),
            Err(e) => return Err(format!("{} is unreadable ({e})", root.display())),
        };
        match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => unreadable.push(path),
            Ok(text) => match hosts_yml_holds_token(&text) {
                Ok(true) => holding.push(path),
                Ok(false) => {}
                Err(_) => unreadable.push(path),
            },
        }
    }
    holding.sort();
    unreadable.sort();
    Ok((holding, unreadable))
}

#[cfg(test)]
#[path = "gh_keyring_token_tests.rs"]
pub(crate) mod gh_keyring_token_tests;
