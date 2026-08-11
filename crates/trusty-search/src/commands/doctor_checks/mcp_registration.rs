//! MCP-client registration checks for `trusty-search doctor` (#5264).
//!
//! Why: Codex Desktop listed `trusty-search` as an enabled stdio server while
//! the registration held `args = []`. Codex exec'd the bare binary, which
//! printed its help and exited before MCP initialization, so the model got no
//! tools from a connection that showed green. `doctor` had no check that read a
//! client registration at all — it verified the daemon, the data dir, the port,
//! and the model cache, none of which were broken — so the one thing that WAS
//! broken had nowhere to be reported.
//!
//! What: reads the registrations `trusty-search setup` writes, and reports the
//! five facts an operator needs to tell a working one from the #5264 shape —
//! the executable and its version, the effective argument vector, which project
//! or index the session will select, which daemon it will therefore talk to,
//! and the command that repairs it.
//!
//! Scope: Codex's `~/.codex/config.toml` and Claude Code's two GLOBAL settings
//! files. `setup` also patches per-project `.claude/settings.json` files found
//! by an eight-deep `$HOME` walk; doctor deliberately does not repeat that walk,
//! because a diagnostic that takes tens of seconds on a large `$HOME` would not
//! get run. [`check_claude_registrations`] states that limit in an actual
//! `CheckResult` when no global file exists, so a machine whose only
//! registration is project-local reads a warning rather than nothing at all.
//!
//! Test: the inline `tests` module below.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::CheckResult;

/// The MCP entrypoint a registration must launch for the process to speak MCP.
///
/// Why: this is the whole of #5264 — a registration missing this argument execs
/// the bare binary, which prints help and exits before initialization.
const MCP_ENTRYPOINT_ARG: &str = "serve";

/// One MCP-client registration of trusty-search, as read off disk.
#[derive(Debug, Clone, PartialEq)]
pub struct McpRegistration {
    /// Human name of the client this registration belongs to.
    pub client: &'static str,
    /// File the registration was read from.
    pub path: PathBuf,
    /// Executable the client will launch.
    pub command: String,
    /// Argument vector, verbatim — the field #5264 was filed about.
    pub args: Vec<String>,
    /// Environment overrides the client will apply to the launched process.
    pub env: BTreeMap<String, String>,
}

impl McpRegistration {
    /// Does this registration actually reach MCP mode?
    fn launches_mcp(&self) -> bool {
        self.args.iter().any(|a| a == MCP_ENTRYPOINT_ARG)
    }

    /// Which project or index the session will pin to (#1373), if any.
    ///
    /// Why: an unpinned session resolves its index from the launched process's
    /// working directory, which is the client's choice and not visible here.
    /// Saying "unpinned" is the honest answer; naming a guess would not be.
    /// What: reads `--index` / `--project` out of the argument vector, then the
    /// `TRUSTY_INDEX` environment override clap also accepts.
    fn selected_scope(&self) -> String {
        for (i, arg) in self.args.iter().enumerate() {
            if let Some(v) = flag_value(arg, "--index", self.args.get(i + 1)) {
                return format!("index '{v}'");
            }
            if let Some(v) = flag_value(arg, "--project", self.args.get(i + 1)) {
                return format!("project '{v}'");
            }
        }
        match self.env.get("TRUSTY_INDEX") {
            Some(v) => format!("index '{v}' (via TRUSTY_INDEX)"),
            None => "unpinned — resolved from the client's working directory".to_string(),
        }
    }

    /// Which daemon this registration will reach.
    ///
    /// Why: this is the "healthy, but not yours" hazard from the other half of
    /// #5264, seen from the configuration side. A registration that sets
    /// `TRUSTY_DATA_DIR` addresses an isolated instance; one that does not
    /// resolves the machine-wide daemon, whatever `doctor` itself just probed.
    fn daemon_ownership(&self, probed_base: &str) -> String {
        match self.env.get("TRUSTY_DATA_DIR") {
            Some(dir) => format!("isolated instance under TRUSTY_DATA_DIR={dir}"),
            None => format!("machine-wide daemon ({probed_base})"),
        }
    }
}

/// Value of `flag` given the current token and the one after it.
///
/// Handles both `--flag value` and `--flag=value`.
fn flag_value(arg: &str, flag: &str, next: Option<&String>) -> Option<String> {
    if let Some(rest) = arg.strip_prefix(flag) {
        if let Some(v) = rest.strip_prefix('=') {
            return (!v.is_empty()).then(|| v.to_string());
        }
        if rest.is_empty() {
            return next.filter(|v| !v.starts_with('-')).cloned();
        }
    }
    None
}

/// Verdict for one registration.
///
/// Why: the #5264 shape and an absent registration need different words and
/// different urgency — a missing registration is a client nobody set up yet,
/// while a present one that cannot initialize is a connection reporting itself
/// healthy while delivering nothing.
/// What: `Error` when the registration exists but omits `serve`, `Warn` when it
/// is absent entirely, `Ok` otherwise. Every arm names `trusty-search setup`.
/// Test: `registration_with_empty_args_is_an_error`,
/// `registration_with_serve_is_ok`, `absent_registration_warns`.
pub fn check_registration(
    reg: Option<&McpRegistration>,
    client: &str,
    path: &Path,
    version: &str,
    probed_base: &str,
) -> CheckResult {
    let Some(reg) = reg else {
        return CheckResult::Warn(format!(
            "{client} MCP registration: absent from {} — run `trusty-search setup` \
             to register it",
            path.display()
        ));
    };

    let detail = format!(
        "exe={} ({version}), args={:?}, scope={}, daemon={}",
        reg.command,
        reg.args,
        reg.selected_scope(),
        reg.daemon_ownership(probed_base),
    );

    if reg.launches_mcp() {
        CheckResult::Ok(format!(
            "{client} MCP registration: {} — {detail}",
            reg.path.display()
        ))
    } else {
        CheckResult::Error(format!(
            "{client} MCP registration launches the bare binary and never enters MCP \
             mode (#5264): {} is missing the `{MCP_ENTRYPOINT_ARG}` argument, so the \
             client shows the connection as enabled while the model gets no tools. \
             {detail}. Repair it with `trusty-search setup`.",
            reg.path.display()
        ))
    }
}

/// Version string of the executable a registration launches.
///
/// Why: a registration can point at a stale binary on `PATH` — a different
/// install from the one `doctor` is running — and that difference is invisible
/// unless the registered command is asked itself.
/// What: runs `<command> --version` and returns its trimmed stdout. A command
/// that cannot be spawned (not on `PATH`, not executable) reports that rather
/// than a version, because it is the same class of finding.
/// Test: not unit-tested — it spawns a process; the surrounding formatting is
/// covered by `check_registration`'s tests, which pass the version in.
pub fn registered_exe_version(command: &str) -> String {
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
/// Why: `trusty_common::codex_config` owns WRITING this file; nothing reads it
/// back, so a broken registration had no way to be observed. Reading stays here
/// rather than in `trusty-common` because trusty-search is the only consumer —
/// the shared-capability rule governs duplication across crates, and there is
/// no second implementation to converge.
/// What: parses `<home>/.codex/config.toml` and returns
/// `[mcp_servers.<server_key>]`'s `command`, `args`, and `env`. A missing file,
/// a parse failure, or a missing table all yield `None`: an unreadable config is
/// reported by the check as "absent", which is the same remediation.
/// Test: `reads_a_codex_registration`, `codex_missing_file_is_none`.
pub fn read_codex_registration(home: &Path, server_key: &str) -> Option<McpRegistration> {
    let path = trusty_common::codex_config::codex_config_path(home);
    let text = std::fs::read_to_string(&path).ok()?;
    let doc: toml::Value = text.parse().ok()?;
    let entry = doc.get("mcp_servers")?.get(server_key)?;
    Some(McpRegistration {
        client: "Codex",
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

/// The two Claude Code settings files that exist independently of any project.
fn claude_global_settings_paths(home: &Path) -> Vec<PathBuf> {
    ["settings.json", "settings.local.json"]
        .iter()
        .map(|n| home.join(".claude").join(n))
        .collect()
}

/// Every Claude Code verdict for this machine — never an empty list.
///
/// Why (#5264): the earlier loop skipped a settings file that did not exist and
/// emitted nothing when neither did, so a machine whose ONLY registration is
/// project-local got no Claude Code line at all. That is not exotic — `setup`
/// patches project-local `.claude/settings.json` files it finds by an eight-deep
/// `$HOME` walk, so zero global files is a normal state. A broken project-local
/// registration would then be reported by nothing on the machine, which is the
/// failure #5264 was filed about, reproduced for the client this check added.
/// What: one verdict per existing global file; when neither exists, a single
/// `Warn` that names the scope limit, so the operator reads "doctor did not
/// check project-local files" rather than silence.
/// Test: `absent_global_claude_files_still_emit_a_scope_warning`,
/// `existing_global_claude_file_is_checked`.
pub fn check_claude_registrations(
    home: &Path,
    server_key: &str,
    probed_base: &str,
    version_of: &dyn Fn(Option<&McpRegistration>) -> String,
) -> Vec<CheckResult> {
    let paths = claude_global_settings_paths(home);
    let existing: Vec<&PathBuf> = paths.iter().filter(|p| p.is_file()).collect();

    if existing.is_empty() {
        return vec![CheckResult::Warn(format!(
            "Claude Code MCP registration: neither global settings file exists ({}) \
             — nothing was checked for this client. `trusty-search setup` also \
             patches PROJECT-LOCAL .claude/settings.json files, which doctor does \
             not scan; check those by hand, or run `trusty-search setup` to \
             (re)register everywhere.",
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))];
    }

    existing
        .into_iter()
        .map(|path| {
            let reg = read_claude_registration(path, server_key);
            check_registration(
                reg.as_ref(),
                "Claude Code",
                path,
                &version_of(reg.as_ref()),
                probed_base,
            )
        })
        .collect()
}

/// Read a Claude Code registration for `server_key` out of one settings file.
///
/// What: parses the JSON and returns `mcpServers.<server_key>`'s `command`,
/// `args`, and `env`. Same `None`-on-any-problem contract as the Codex reader.
/// Test: `reads_a_claude_registration`.
pub fn read_claude_registration(path: &Path, server_key: &str) -> Option<McpRegistration> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    let entry = doc.get("mcpServers")?.get(server_key)?;
    Some(McpRegistration {
        client: "Claude Code",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(args: &[&str]) -> McpRegistration {
        McpRegistration {
            client: "Codex",
            path: PathBuf::from("/x/.codex/config.toml"),
            command: "trusty-search".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::new(),
        }
    }

    /// The #5264 defect itself: a registration whose argument vector never
    /// reaches MCP mode must be an ERROR, not a warning and not a pass.
    ///
    /// Why: the client reports this connection as enabled. If doctor also
    /// reports it as fine, nothing on the machine ever says otherwise — which
    /// is precisely how the broken shape survived long enough to be filed.
    #[test]
    fn registration_with_empty_args_is_an_error() {
        let r = reg(&[]);
        let result = check_registration(
            Some(&r),
            "Codex",
            Path::new("/x/.codex/config.toml"),
            "trusty-search 0.46.0",
            "http://127.0.0.1:7878",
        );
        assert!(result.is_error(), "expected Error, got {result:?}");
        let CheckResult::Error(msg) = &result else {
            unreachable!()
        };
        assert!(msg.contains("#5264"), "must cite the issue: {msg}");
        assert!(
            msg.contains("trusty-search setup"),
            "must name the repair: {msg}"
        );
    }

    /// The reporter's hand-edited shape — one literal argument whose TEXT looks
    /// like an argument vector — is still broken, and must still be an error.
    #[test]
    fn registration_with_nested_json_string_args_is_an_error() {
        let r = reg(&["[\"serve\"]"]);
        let result = check_registration(
            Some(&r),
            "Codex",
            Path::new("/x/.codex/config.toml"),
            "v",
            "http://127.0.0.1:7878",
        );
        assert!(result.is_error(), "expected Error, got {result:?}");
    }

    /// A correct registration passes and reports all five facts.
    #[test]
    fn registration_with_serve_is_ok_and_reports_the_five_facts() {
        let mut r = reg(&["serve", "--index", "my-proj"]);
        r.env.insert("TRUSTY_DATA_DIR".into(), "/tmp/iso".into());
        let result = check_registration(
            Some(&r),
            "Codex",
            Path::new("/x/.codex/config.toml"),
            "trusty-search 0.46.0",
            "http://127.0.0.1:7878",
        );
        let CheckResult::Ok(msg) = &result else {
            panic!("expected Ok, got {result:?}")
        };
        assert!(msg.contains("trusty-search 0.46.0"), "version: {msg}");
        assert!(msg.contains("\"serve\""), "effective args: {msg}");
        assert!(msg.contains("index 'my-proj'"), "selected scope: {msg}");
        assert!(msg.contains("/tmp/iso"), "daemon ownership: {msg}");
    }

    /// An unpinned registration says so instead of guessing an index.
    #[test]
    fn unpinned_registration_reports_the_working_directory_rule() {
        let r = reg(&["serve"]);
        assert!(r.selected_scope().contains("unpinned"));
        assert!(r
            .daemon_ownership("http://127.0.0.1:7878")
            .contains("machine-wide"));
    }

    /// `--index=value` is the same pin as `--index value`.
    #[test]
    fn equals_form_of_the_index_flag_is_recognised() {
        assert_eq!(reg(&["serve", "--index=x"]).selected_scope(), "index 'x'");
    }

    /// No registration at all is a warning that names the file and the fix.
    #[test]
    fn absent_registration_warns() {
        let result = check_registration(
            None,
            "Codex",
            Path::new("/x/.codex/config.toml"),
            "v",
            "http://127.0.0.1:7878",
        );
        assert!(result.is_warn(), "expected Warn, got {result:?}");
    }

    #[test]
    fn reads_a_codex_registration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = trusty_common::codex_config::codex_config_path(tmp.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\n\
             args = [\"serve\"]\n\n[mcp_servers.trusty-search.env]\n\
             TRUSTY_DATA_DIR = \"/tmp/iso\"\n",
        )
        .unwrap();

        let r = read_codex_registration(tmp.path(), "trusty-search").expect("registration");
        assert_eq!(r.command, "trusty-search");
        assert_eq!(r.args, vec!["serve".to_string()]);
        assert_eq!(
            r.env.get("TRUSTY_DATA_DIR").map(String::as_str),
            Some("/tmp/iso")
        );
    }

    #[test]
    fn codex_missing_file_is_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(read_codex_registration(tmp.path(), "trusty-search").is_none());
    }

    /// The scope limit must reach the operator as a verdict, not as silence.
    ///
    /// Why (#5264): `setup` patches project-local `.claude/settings.json` files
    /// found by an eight-deep `$HOME` walk, so a machine with zero GLOBAL files
    /// is a normal state, not an edge case. Emitting nothing there means a
    /// broken project-local registration is reported by nothing on the machine
    /// — the exact failure this check was added to end.
    /// What: a `$HOME` with no `.claude` directory at all must still produce one
    /// result, and that result must name the unscanned project-local files.
    #[test]
    fn absent_global_claude_files_still_emit_a_scope_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let version = |_: Option<&McpRegistration>| "v".to_string();

        let results = check_claude_registrations(
            tmp.path(),
            "trusty-search",
            "http://127.0.0.1:7878",
            &version,
        );

        assert_eq!(
            results.len(),
            1,
            "silence is the defect; expected one verdict, got {results:?}"
        );
        assert!(results[0].is_warn(), "expected Warn, got {:?}", results[0]);
        let CheckResult::Warn(msg) = &results[0] else {
            unreachable!()
        };
        assert!(
            msg.contains("PROJECT-LOCAL"),
            "must name what was NOT scanned: {msg}"
        );
        assert!(
            msg.contains("trusty-search setup"),
            "must name the repair: {msg}"
        );
    }

    /// An existing global file is checked on its merits, and a broken one is an
    /// error rather than the scope warning.
    #[test]
    fn existing_global_claude_file_is_checked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join(".claude");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"mcpServers":{"trusty-search":{"command":"trusty-search","args":[]}}}"#,
        )
        .unwrap();
        let version = |_: Option<&McpRegistration>| "v".to_string();

        let results = check_claude_registrations(
            tmp.path(),
            "trusty-search",
            "http://127.0.0.1:7878",
            &version,
        );

        assert_eq!(results.len(), 1, "one verdict per existing file");
        assert!(
            results[0].is_error(),
            "an empty args vector is the #5264 defect: {:?}",
            results[0]
        );
    }

    #[test]
    fn reads_a_claude_registration() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"trusty-search":{"command":"trusty-search","args":["serve"]}}}"#,
        )
        .unwrap();

        let r = read_claude_registration(&path, "trusty-search").expect("registration");
        assert_eq!(r.client, "Claude Code");
        assert_eq!(r.args, vec!["serve".to_string()]);
    }
}
