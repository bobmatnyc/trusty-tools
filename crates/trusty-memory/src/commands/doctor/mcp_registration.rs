//! MCP-client registration check for `trusty-memory doctor` (#5265).
//!
//! Why: Codex listed `trusty-memory` as an enabled stdio server and exposed no
//! tools. Nothing on the machine reported that: `doctor` checked the fastembed
//! cache, the launchd plist, the daemon's health, and the palace locks, none of
//! which were broken — so the one thing that WAS broken had nowhere to be seen.
//! A registration can also carry the retired `--stdio` flag, or point at a
//! binary older than the one `doctor` is running, and neither is visible from
//! the client's green connection indicator.
//!
//! What: reads back the registrations `trusty-memory setup` writes and reports
//! four facts per client — the registered executable and its own `--version`,
//! the effective argument vector, the transport that vector selects, and the
//! command that repairs it. A vector that never reaches `serve` is a `Fail`; a
//! legacy `serve --stdio` vector is a `Warn`; an absent registration is a
//! `Warn`.
//!
//! Scope: Codex's `~/.codex/config.toml` and Claude Code's two GLOBAL settings
//! files. `setup` also patches per-project `.claude/settings.json` files found
//! by a `$HOME` walk; doctor does not repeat that walk, because a diagnostic
//! that takes tens of seconds on a large `$HOME` does not get run. When no
//! global file exists, [`check_claude_registrations`] says so in a real result
//! rather than emitting nothing.
//!
//! Test: the inline `tests` module below.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::CheckResult;

/// The argument a registration must carry for the process to speak MCP.
const MCP_ENTRYPOINT_ARG: &str = "serve";

/// The retired transport-selection flag (#5265).
///
/// Why: bare `serve` has spoken MCP over stdio since #5267, so this flag selects
/// nothing. A registration still carrying it works today and is repaired by the
/// next `setup` run — that is a warning, not a failure.
const LEGACY_TRANSPORT_FLAG: &str = "--stdio";

/// The transport contract the canonical vector selects.
const TRANSPORT_CONTRACT: &str =
    "MCP over stdio; the process stays attached to stdin/stdout and starts the \
     daemon itself when one is not already running";

/// One MCP-client registration of trusty-memory, as read off disk.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct McpRegistration {
    /// File the registration was read from.
    pub(super) path: PathBuf,
    /// Executable the client will launch.
    pub(super) command: String,
    /// Argument vector, verbatim — the field #5265 was filed about.
    pub(super) args: Vec<String>,
    /// Environment overrides the client will apply to the launched process.
    pub(super) env: BTreeMap<String, String>,
}

impl McpRegistration {
    /// Does this registration reach MCP mode at all?
    fn launches_mcp(&self) -> bool {
        self.args.iter().any(|a| a == MCP_ENTRYPOINT_ARG)
    }

    /// Does it still carry the retired transport-selection flag?
    fn carries_legacy_flag(&self) -> bool {
        self.args.iter().any(|a| a == LEGACY_TRANSPORT_FLAG)
    }

    /// Which palace and which data directory this session will bind to.
    ///
    /// Why: two registrations with the same argument vector can address
    /// different state. `--palace` pins the palace; `TRUSTY_DATA_DIR_OVERRIDE`
    /// moves the whole data directory, and therefore the daemon, somewhere else.
    fn binding(&self) -> String {
        let palace = self
            .args
            .iter()
            .enumerate()
            .find_map(|(i, a)| flag_value(a, "--palace", self.args.get(i + 1)))
            .map_or_else(
                || "palace resolved from the client's working directory".to_string(),
                |p| format!("palace '{p}'"),
            );
        match self.env.get("TRUSTY_DATA_DIR_OVERRIDE") {
            Some(dir) => format!("{palace}, isolated data dir {dir}"),
            None => format!("{palace}, machine-wide data dir"),
        }
    }
}

/// Value of `flag` given the current token and the one after it.
///
/// Handles both `--flag value` and `--flag=value`.
fn flag_value(arg: &str, flag: &str, next: Option<&String>) -> Option<String> {
    let rest = arg.strip_prefix(flag)?;
    if let Some(v) = rest.strip_prefix('=') {
        return (!v.is_empty()).then(|| v.to_string());
    }
    if rest.is_empty() {
        return next.filter(|v| !v.starts_with('-')).cloned();
    }
    None
}

/// Verdict for one registration.
///
/// Why (#5265): the client reports every one of these connections as enabled. A
/// vector that never reaches `serve` delivers no tools while looking healthy, so
/// it is a failure; a legacy `--stdio` vector works but is about to stop being
/// the contract, so it is a warning; an absent registration is a client nobody
/// set up yet.
/// What: `Fail` when `serve` is missing, `Warn` when `--stdio` is present or the
/// registration is absent, `Pass` otherwise. Every arm names the version, the
/// effective vector, the transport contract, and `trusty-memory setup`.
/// Test: `registration_without_serve_fails`,
/// `nested_json_string_vector_fails`, `legacy_stdio_vector_warns`,
/// `canonical_vector_passes_and_reports_the_four_facts`,
/// `absent_registration_warns`.
pub(super) fn check_registration(
    reg: Option<&McpRegistration>,
    client: &str,
    path: &Path,
    version: &str,
) -> CheckResult {
    let label = format!("{client} MCP registration");
    let Some(reg) = reg else {
        return CheckResult::warn(
            label,
            format!(
                "absent from {} — run `trusty-memory setup` to register it",
                path.display()
            ),
        );
    };

    let detail = format!(
        "{} — exe={} ({version}), args={:?}, transport={TRANSPORT_CONTRACT}, binding={}",
        reg.path.display(),
        reg.command,
        reg.args,
        reg.binding(),
    );

    if !reg.launches_mcp() {
        return CheckResult::fail(
            label,
            format!(
                "{detail}. The vector never reaches `{MCP_ENTRYPOINT_ARG}`, so the client \
                 launches a process that exits before MCP initialization while showing the \
                 connection as enabled (#5265). Repair it with `trusty-memory setup`."
            ),
        );
    }
    if reg.carries_legacy_flag() {
        return CheckResult::warn(
            label,
            format!(
                "{detail}. `{LEGACY_TRANSPORT_FLAG}` is the retired transport-selection flag \
                 (#5265) — bare `{MCP_ENTRYPOINT_ARG}` is the contract. Run `trusty-memory \
                 setup` to rewrite it."
            ),
        );
    }
    CheckResult::pass(label, detail)
}

/// Version string of the executable a registration launches.
///
/// Why: a registration can point at a stale binary on `PATH` — a different
/// install from the one `doctor` is running — and that difference is invisible
/// unless the registered command is asked itself.
/// What: runs `<command> --version` and returns its trimmed stdout. A command
/// that cannot be spawned reports that instead, because it is the same class of
/// finding.
/// Test: not unit-tested — it spawns a process; the surrounding formatting is
/// covered by `check_registration`'s tests, which pass the version in.
fn registered_exe_version(command: &str) -> String {
    match std::process::Command::new(command)
        .arg("--version")
        .output()
    {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                "version unreadable".to_string()
            } else {
                text
            }
        }
        Ok(out) => format!("`{command} --version` exited {}", out.status),
        Err(e) => format!("NOT LAUNCHABLE: {e}"),
    }
}

/// Read the Codex CLI registration for `server_key`, if present.
///
/// Why: `trusty_common::codex_config` owns WRITING this file; until #5265
/// nothing read it back, so a broken registration had no way to be observed.
/// What: parses `<home>/.codex/config.toml` and returns
/// `[mcp_servers.<server_key>]`'s `command`, `args`, and `env`. A missing file,
/// a parse failure, or a missing table all yield `None` — reported as "absent",
/// which carries the same remediation.
/// Test: `reads_a_codex_registration`, `codex_missing_file_is_none`.
pub(super) fn read_codex_registration(home: &Path, server_key: &str) -> Option<McpRegistration> {
    let path = trusty_common::codex_config::codex_config_path(home);
    let doc: toml::Value = std::fs::read_to_string(&path).ok()?.parse().ok()?;
    let entry = doc.get("mcp_servers")?.get(server_key)?;
    Some(McpRegistration {
        path,
        command: entry.get("command")?.as_str()?.to_string(),
        args: entry
            .get("args")
            .and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        env: entry
            .get("env")
            .and_then(toml::Value::as_table)
            .map(|t| {
                t.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Read a Claude Code registration for `server_key` out of one settings file.
///
/// What: parses the JSON and returns `mcpServers.<server_key>`'s `command`,
/// `args`, and `env`. Same `None`-on-any-problem contract as the Codex reader.
/// Test: `reads_a_claude_registration`.
pub(super) fn read_claude_registration(path: &Path, server_key: &str) -> Option<McpRegistration> {
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let entry = doc.get("mcpServers")?.get(server_key)?;
    Some(McpRegistration {
        path: path.to_path_buf(),
        command: entry.get("command")?.as_str()?.to_string(),
        args: entry
            .get("args")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        env: entry
            .get("env")
            .and_then(serde_json::Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The two Claude Code settings files that exist independently of any project.
fn claude_global_settings_paths(home: &Path) -> Vec<PathBuf> {
    ["settings.json", "settings.local.json"]
        .iter()
        .map(|n| home.join(".claude").join(n))
        .collect()
}

/// Every Claude Code verdict for this machine — never an empty list.
///
/// Why (#5265): `setup` patches project-local `.claude/settings.json` files it
/// finds by a `$HOME` walk, so a machine with zero GLOBAL files is a normal
/// state. Emitting nothing there would mean a broken project-local registration
/// is reported by nothing on the machine — the failure this check exists to end.
/// What: one verdict per existing global file; when neither exists, one `Warn`
/// naming the scope limit.
/// Test: `absent_global_claude_files_still_emit_a_scope_warning`,
/// `existing_global_claude_file_is_checked`.
pub(super) fn check_claude_registrations(
    home: &Path,
    server_key: &str,
    version_of: &dyn Fn(Option<&McpRegistration>) -> String,
) -> Vec<CheckResult> {
    let paths = claude_global_settings_paths(home);
    let existing: Vec<&PathBuf> = paths.iter().filter(|p| p.is_file()).collect();

    if existing.is_empty() {
        return vec![CheckResult::warn(
            "Claude Code MCP registration",
            format!(
                "neither global settings file exists ({}) — nothing was checked for this \
                 client. `trusty-memory setup` also patches PROJECT-LOCAL \
                 .claude/settings.json files, which doctor does not scan; check those by \
                 hand, or run `trusty-memory setup` to (re)register everywhere.",
                paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )];
    }

    existing
        .into_iter()
        .map(|path| {
            let reg = read_claude_registration(path, server_key);
            check_registration(reg.as_ref(), "Claude Code", path, &version_of(reg.as_ref()))
        })
        .collect()
}

/// Every MCP-client registration verdict for this machine.
///
/// Why: `handle_doctor` should ask one question and get every client's answer,
/// so adding a client later does not mean editing the orchestrator.
/// What: the Codex verdict followed by the Claude Code verdicts. Each
/// registration's version is read from the executable it actually names, so a
/// registration pointing at a stale install reports that install's version.
/// Test: the per-client helpers above are unit-tested; this assembly is
/// process-level.
pub(super) fn check_mcp_registrations(server_key: &str) -> Vec<CheckResult> {
    let Some(home) = dirs::home_dir() else {
        return vec![CheckResult::unknown(
            "MCP registrations",
            "could not resolve the home directory, so no client config was read",
        )];
    };
    let version_of = |reg: Option<&McpRegistration>| {
        reg.map_or_else(
            || "no executable".to_string(),
            |r| registered_exe_version(&r.command),
        )
    };

    let codex = read_codex_registration(&home, server_key);
    let mut results = vec![check_registration(
        codex.as_ref(),
        "Codex",
        &trusty_common::codex_config::codex_config_path(&home),
        &version_of(codex.as_ref()),
    )];
    results.extend(check_claude_registrations(&home, server_key, &version_of));
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::doctor::CheckStatus;

    fn reg(args: &[&str]) -> McpRegistration {
        McpRegistration {
            path: PathBuf::from("/x/.codex/config.toml"),
            command: "trusty-memory".into(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            env: BTreeMap::new(),
        }
    }

    fn verdict(args: &[&str]) -> CheckResult {
        check_registration(
            Some(&reg(args)),
            "Codex",
            Path::new("/x/.codex/config.toml"),
            "trusty-memory 0.40.0",
        )
    }

    /// The #5265 defect itself: a vector that never reaches MCP mode must be a
    /// failure, because the client already reports the connection as enabled.
    #[test]
    fn registration_without_serve_fails() {
        let result = verdict(&[]);
        assert_eq!(result.status, CheckStatus::Fail, "got {result:?}");
        let text = format!("{result:?}");
        assert!(text.contains("#5265"), "must cite the issue: {text}");
        assert!(
            text.contains("trusty-memory setup"),
            "must name the repair: {text}"
        );
    }

    /// The reporter's hand-edited shape — one literal argument whose TEXT looks
    /// like an argument vector — is still broken, and must still fail.
    #[test]
    fn nested_json_string_vector_fails() {
        assert_eq!(verdict(&["[\"serve\"]"]).status, CheckStatus::Fail);
    }

    /// A joined single string is the same class of defect: the process receives
    /// `serve --stdio` as one token and clap rejects it.
    #[test]
    fn joined_string_vector_fails() {
        assert_eq!(verdict(&["serve --stdio"]).status, CheckStatus::Fail);
    }

    /// A legacy `--stdio` vector still works, so it warns rather than failing —
    /// and the warning names the flag and the repair.
    #[test]
    fn legacy_stdio_vector_warns() {
        let result = verdict(&["serve", "--stdio"]);
        assert_eq!(result.status, CheckStatus::Warn, "got {result:?}");
        let text = format!("{result:?}");
        assert!(text.contains("--stdio"), "must name the flag: {text}");
        assert!(
            text.contains("trusty-memory setup"),
            "must name the repair: {text}"
        );
    }

    /// The canonical vector passes and reports the four facts an operator needs.
    #[test]
    fn canonical_vector_passes_and_reports_the_four_facts() {
        let mut r = reg(&["serve", "--palace", "my-proj"]);
        r.env
            .insert("TRUSTY_DATA_DIR_OVERRIDE".into(), "/tmp/iso".into());
        let result = check_registration(
            Some(&r),
            "Codex",
            Path::new("/x/.codex/config.toml"),
            "trusty-memory 0.40.0",
        );
        assert_eq!(result.status, CheckStatus::Pass, "got {result:?}");
        let text = format!("{result:?}");
        assert!(text.contains("trusty-memory 0.40.0"), "version: {text}");
        assert!(text.contains("\\\"serve\\\""), "effective args: {text}");
        assert!(text.contains("MCP over stdio"), "transport: {text}");
        assert!(text.contains("palace 'my-proj'"), "binding: {text}");
        assert!(text.contains("/tmp/iso"), "data dir: {text}");
    }

    /// An unpinned registration says so rather than guessing a palace.
    #[test]
    fn unpinned_registration_reports_the_working_directory_rule() {
        let binding = reg(&["serve"]).binding();
        assert!(binding.contains("working directory"), "got {binding}");
        assert!(binding.contains("machine-wide"), "got {binding}");
    }

    /// `--palace=value` is the same pin as `--palace value`.
    #[test]
    fn equals_form_of_the_palace_flag_is_recognised() {
        assert!(reg(&["serve", "--palace=x"])
            .binding()
            .contains("palace 'x'"));
    }

    /// No registration at all is a warning that names the file and the fix.
    #[test]
    fn absent_registration_warns() {
        let result = check_registration(None, "Codex", Path::new("/x/.codex/config.toml"), "v");
        assert_eq!(result.status, CheckStatus::Warn, "got {result:?}");
    }

    #[test]
    fn reads_a_codex_registration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = trusty_common::codex_config::codex_config_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[mcp_servers.trusty-memory]\ncommand = \"trusty-memory\"\n\
             args = [\"serve\"]\n\n[mcp_servers.trusty-memory.env]\n\
             TRUSTY_DATA_DIR_OVERRIDE = \"/tmp/iso\"\n",
        )
        .unwrap();

        let r = read_codex_registration(tmp.path(), "trusty-memory").expect("registration");
        assert_eq!(r.command, "trusty-memory");
        assert_eq!(r.args, vec!["serve".to_string()]);
        assert_eq!(
            r.env.get("TRUSTY_DATA_DIR_OVERRIDE").map(String::as_str),
            Some("/tmp/iso")
        );
    }

    #[test]
    fn codex_missing_file_is_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(read_codex_registration(tmp.path(), "trusty-memory").is_none());
    }

    /// The scope limit must reach the operator as a verdict, not as silence.
    #[test]
    fn absent_global_claude_files_still_emit_a_scope_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let version = |_: Option<&McpRegistration>| "v".to_string();

        let results = check_claude_registrations(tmp.path(), "trusty-memory", &version);

        assert_eq!(results.len(), 1, "silence is the defect: {results:?}");
        assert_eq!(results[0].status, CheckStatus::Warn, "got {:?}", results[0]);
        let text = format!("{:?}", results[0]);
        assert!(
            text.contains("PROJECT-LOCAL"),
            "must name what was NOT scanned: {text}"
        );
    }

    /// An existing global file is checked on its merits, and a broken one fails
    /// rather than producing the scope warning.
    #[test]
    fn existing_global_claude_file_is_checked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"mcpServers":{"trusty-memory":{"command":"trusty-memory","args":[]}}}"#,
        )
        .unwrap();
        let version = |_: Option<&McpRegistration>| "v".to_string();

        let results = check_claude_registrations(tmp.path(), "trusty-memory", &version);

        assert_eq!(results.len(), 1, "one verdict per existing file");
        assert_eq!(
            results[0].status,
            CheckStatus::Fail,
            "an empty vector is the #5265 defect: {:?}",
            results[0]
        );
    }

    #[test]
    fn reads_a_claude_registration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"trusty-memory":{"command":"trusty-memory","args":["serve"]}}}"#,
        )
        .unwrap();

        let r = read_claude_registration(&path, "trusty-memory").expect("registration");
        assert_eq!(r.args, vec!["serve".to_string()]);
    }
}
