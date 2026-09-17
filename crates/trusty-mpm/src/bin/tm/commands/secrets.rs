//! `tm secrets` command handlers — slice 1, keychain only (issue #7521,
//! DOC-74 §6/§8).
//!
//! Why: the storage itself belongs in trusty-common
//! (`credentials::KeychainBackend`), so this file is only value ENTRY and
//! PRINTING — the two places a secret can leak. Both are written as pure
//! functions over an injected backend, so the "never prints the value"
//! property is directly testable without a real keychain.
//! What: `dispatch` plus one handler per [`crate::cli::SecretsAction`]
//! variant. Every value-bearing path takes the value from stdin or a masked
//! prompt, hands it straight to the backend, and drops it; no handler returns,
//! logs, or formats it, and no error message quotes it.
//! Test: `secrets_add_reads_value_from_stdin_when_dash`,
//! `secrets_add_never_prints_the_value`,
//! `secrets_add_refuses_when_no_tty_and_no_stdin_flag`,
//! `secrets_list_prints_names_only`,
//! `secrets_configure_rejects_unsupported_provider`,
//! `secrets_doctor_reports_probe_result_without_prompting`.

use std::io::{IsTerminal, Read};

use anyhow::{Context, bail};
use trusty_common::credentials::KeychainBackend;
use trusty_mpm::core::trusty_tools_config::secrets as secrets_config;
use trusty_mpm::core::trusty_tools_config::secrets::{KEYCHAIN_BACKEND, SecretsConfig};

use crate::cli::SecretsAction;

/// The sentinel `--value` argument meaning "read the value from stdin".
const STDIN_SENTINEL: &str = "-";

/// Route a parsed [`SecretsAction`] to its handler.
///
/// Test: the six `cli_parses_secrets_*` parse tests cover the inputs; each
/// handler has its own behavioural test.
pub(crate) fn dispatch(action: SecretsAction) -> anyhow::Result<()> {
    match action {
        SecretsAction::Configure { provider, group } => configure(&provider, group.as_deref()),
        SecretsAction::Add { key, value } => add(&key, value.as_deref()),
        SecretsAction::List => list(),
        SecretsAction::Remove { key } => remove(&key),
        SecretsAction::Doctor => doctor(),
    }
}

/// Reject any backend slice 1 does not implement.
///
/// Why: silently accepting `--provider onepassword` and storing in the
/// keychain would put values somewhere the operator did not choose.
/// What: only `keychain` passes; anything else errors naming #7519, the issue
/// that adds the other backends.
/// Test: `secrets_configure_rejects_unsupported_provider`.
fn validate_provider(provider: &str) -> anyhow::Result<()> {
    if provider == KEYCHAIN_BACKEND {
        return Ok(());
    }
    bail!(
        "unsupported secrets provider {provider:?}: slice 1 implements only `keychain`. \
         1Password and Keeper land with #7519."
    )
}

/// `tm secrets configure` handler.
///
/// Test: `secrets_configure_rejects_unsupported_provider`,
/// `secrets_configure_writes_group_and_backend_to_config` (the write itself).
fn configure(provider: &str, group: Option<&str>) -> anyhow::Result<()> {
    validate_provider(provider)?;
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    // A config that will not parse is an error here, never a fall-through to
    // the git-remote-derived group.
    let group = secrets_config::resolve(group, &cwd)?.group;
    let path = secrets_config::save(SecretsConfig::new(KEYCHAIN_BACKEND, Some(group.clone())))?;
    println!("tm secrets: backend {KEYCHAIN_BACKEND}, group {group}");
    println!("  config: {}", path.display());
    println!(
        "  keychain service: {}",
        trusty_common::credentials::keychain_service_name(&group)
    );
    Ok(())
}

/// Open the configured group's vault, failing closed on an unusable config.
///
/// Why: every value-bearing verb needs the same three checks — a supported
/// backend, a determinable group, and a constructible vault — and a wrong
/// group silently reads another project's secrets.
/// What: reads the `secrets:` section, refuses a non-keychain backend, and
/// resolves the group (config, else the git remote of the working directory).
/// Test: exercised by `secrets_list_prints_names_only` through the injected
/// backend; the config half is tested in `trusty_tools_config::secrets`.
fn open_backend() -> anyhow::Result<KeychainBackend> {
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    // Fail-closed: a corrupt config errors here rather than resolving to the
    // git-remote-derived group, which would be a different vault.
    let resolved = secrets_config::resolve(None, &cwd)?;
    validate_provider(&resolved.backend)?;
    Ok(KeychainBackend::new(&resolved.group)?)
}

/// Read a value from `reader` to EOF, trimmed; blank is an error.
///
/// Why: mirrors `tm auth set-token`'s stdin contract — a trailing newline from
/// `printf`/a pipe must not become part of the stored value, and an empty pipe
/// must not silently store an empty secret.
/// What: reads to EOF, trims surrounding whitespace, rejects an empty result.
/// The error message never quotes what was read.
/// Test: `secrets_add_reads_value_from_stdin_when_dash`.
fn read_value_from_reader(mut reader: impl Read) -> anyhow::Result<String> {
    let mut buf = String::new();
    reader.read_to_string(&mut buf)?;
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        bail!("no value provided on stdin for this key");
    }
    Ok(trimmed.to_string())
}

/// Decide where `add`'s value comes from, refusing argv and a silent non-TTY.
///
/// Why: the two failure modes that leak or hang — a literal value in argv
/// (visible in `ps` and shell history) and a prompt written to something that
/// is not a terminal.
/// What: `Some("-")` → stdin; `None` on a TTY → masked prompt; `None` without
/// a TTY → an error naming `--value -`; any other `--value` argument → an
/// error refusing a literal. Returns the branch to take rather than the value,
/// so the decision is testable without a terminal.
/// Test: `secrets_add_refuses_when_no_tty_and_no_stdin_flag`,
/// `secrets_add_refuses_a_literal_value_argument`.
fn value_source(value: Option<&str>, stdin_is_tty: bool) -> anyhow::Result<ValueSource> {
    match value {
        Some(STDIN_SENTINEL) => Ok(ValueSource::Stdin),
        Some(_) => bail!(
            "a secret value is never accepted as a command argument (it would be visible in `ps` \
             and shell history) — use `--value -` and pipe it, or omit --value to be prompted"
        ),
        None if stdin_is_tty => Ok(ValueSource::Prompt),
        None => bail!(
            "stdin is not a terminal and --value - was not given — pipe the value and pass \
             `--value -` (e.g. `printf %s \"$VALUE\" | tm secrets add KEY --value -`)"
        ),
    }
}

/// Where `add` reads its value from. Never holds a value itself.
#[derive(Debug, PartialEq, Eq)]
enum ValueSource {
    /// Read stdin to EOF.
    Stdin,
    /// Prompt on the TTY with echo off.
    Prompt,
}

/// `tm secrets add` handler.
///
/// Test: `secrets_add_reads_value_from_stdin_when_dash`,
/// `secrets_add_never_prints_the_value`.
fn add(key: &str, value: Option<&str>) -> anyhow::Result<()> {
    let backend = open_backend()?;
    let source = value_source(value, std::io::stdin().is_terminal())?;
    let secret = match source {
        ValueSource::Stdin => read_value_from_reader(std::io::stdin())?,
        ValueSource::Prompt => {
            let entered = rpassword::prompt_password(format!("Value for {key} (input hidden): "))
                .context("reading the value from the terminal")?;
            let trimmed = entered.trim().to_string();
            if trimmed.is_empty() {
                bail!("no value entered for this key");
            }
            trimmed
        }
    };
    for line in add_into(&backend, key, &secret)? {
        println!("{line}");
    }
    drop(secret);
    Ok(())
}

/// Store one value and return the lines to print — names and locations only.
///
/// Why: the seam every "never prints the value" test runs through; it takes
/// the backend as an argument so a test can inject `MemoryKeyStore`.
/// What: writes through the backend, then renders a confirmation built from
/// the key name, the group, and the service — none of which can carry the
/// value.
/// Test: `secrets_add_never_prints_the_value`.
fn add_into(backend: &KeychainBackend, key: &str, value: &str) -> anyhow::Result<Vec<String>> {
    backend.set(key, value)?;
    Ok(vec![
        format!("tm secrets: stored {key} in group {}", backend.group()),
        format!("  keychain service: {}", backend.service()),
    ])
}

/// `tm secrets list` handler.
///
/// Test: `secrets_list_prints_names_only`.
fn list() -> anyhow::Result<()> {
    let backend = open_backend()?;
    for line in list_lines(&backend)? {
        println!("{line}");
    }
    Ok(())
}

/// The lines `list` prints: the group, then one name per stored key.
///
/// Why: rendering split from I/O so the "names only" property is testable.
/// What: never calls `get`, so no value is ever read, let alone printed.
/// Test: `secrets_list_prints_names_only`.
fn list_lines(backend: &KeychainBackend) -> anyhow::Result<Vec<String>> {
    let names = backend.list()?;
    let mut lines = vec![format!(
        "tm secrets: group {} ({} key{})",
        backend.group(),
        names.len(),
        if names.len() == 1 { "" } else { "s" }
    )];
    lines.extend(names.into_iter().map(|n| format!("  {n}")));
    Ok(lines)
}

/// `tm secrets remove` handler.
///
/// Test: `secrets_remove_reports_the_name_it_dropped`.
fn remove(key: &str) -> anyhow::Result<()> {
    let backend = open_backend()?;
    println!("{}", remove_from(&backend, key)?);
    Ok(())
}

/// Delete one key and return the line to print — the name, never a value.
///
/// Test: `secrets_remove_reports_the_name_it_dropped`.
fn remove_from(backend: &KeychainBackend, key: &str) -> anyhow::Result<String> {
    backend.remove(key)?;
    Ok(format!(
        "tm secrets: removed {key} from group {} (if it was present)",
        backend.group()
    ))
}

/// What `doctor` reports. Carries no field capable of holding a value.
///
/// Test: `secrets_doctor_reports_probe_result_without_prompting`.
#[derive(Debug)]
struct DoctorReport {
    backend: String,
    group: String,
    keychain_reachable: bool,
    indexed_names: usize,
    index_path: String,
}

impl DoctorReport {
    /// Healthy means: a supported backend that answers its probe.
    fn healthy(&self) -> bool {
        self.backend == KEYCHAIN_BACKEND && self.keychain_reachable
    }
}

/// Render a [`DoctorReport`]. Pure — it prompts nothing and reads no value.
///
/// Test: `secrets_doctor_reports_probe_result_without_prompting`.
fn doctor_lines(report: &DoctorReport) -> Vec<String> {
    vec![
        "tm secrets doctor".to_string(),
        format!("  backend: {}", report.backend),
        format!("  group: {}", report.group),
        format!(
            "  keychain: {}",
            if report.keychain_reachable {
                "reachable"
            } else {
                "UNREACHABLE"
            }
        ),
        format!("  indexed names: {}", report.indexed_names),
        format!("  index: {}", report.index_path),
    ]
}

/// `tm secrets doctor` handler — exit 0 healthy, non-zero otherwise.
///
/// Test: `secrets_doctor_reports_probe_result_without_prompting` covers the
/// rendering and the healthy/unhealthy split.
fn doctor() -> anyhow::Result<()> {
    let backend = open_backend()?;
    let report = DoctorReport {
        backend: KEYCHAIN_BACKEND.to_string(),
        group: backend.group().to_string(),
        keychain_reachable: KeychainBackend::keychain_reachable(),
        indexed_names: backend.list()?.len(),
        index_path: backend.index_path().display().to_string(),
    };
    for line in doctor_lines(&report) {
        println!("{line}");
    }
    if !report.healthy() {
        bail!("tm secrets doctor: the keychain backend did not answer its probe");
    }
    Ok(())
}

#[cfg(test)]
#[path = "secrets_tests.rs"]
mod tests;
