//! MCP registration for GUI clients launched by launchd (#6307).
//!
//! Why: a GUI app started by launchd inherits launchd's `PATH`
//! (`/usr/bin:/bin:/usr/sbin:/sbin` — `launchctl getenv PATH` is empty on a
//! stock macOS install). That list contains neither `~/.cargo/bin` nor any
//! shim directory, so a client entry whose `command` is the bare name
//! `trusty-memory` exits 127 before the server speaks a byte of MCP, and the
//! client reports only "no tools". The same entry works from Claude Code,
//! which inherits a login shell's `PATH`, so the defect is invisible until a
//! GUI client is the one spawning. `/usr/local/bin` is writable but is NOT on
//! launchd's `PATH` either, and `/usr/bin` and friends are SIP-restricted, so
//! placing the binary somewhere launchd already looks is not available without
//! sudo. What is available is writing the absolute path into the entry.
//!
//! The owner's ruling is that the user never types a cargo path: the installed
//! binary writes its own, read from [`std::env::current_exe`].
//!
//! What: [`crate::gui_mcp_client::running_binary_path`] resolves and
//! canonicalizes the running executable,
//! [`crate::gui_mcp_client::build_entry`] validates a `{command, args, cwd}`
//! registration (absolute command, existing working directory) and
//! [`crate::gui_mcp_client::configure`] either
//! writes it to the client's own config file or hands it back for the operator
//! to paste when the client keeps no file we can write. The JSON body is built
//! by [`crate::claude_config::mcp_server_entry`] and written by
//! [`crate::claude_config::patch_mcp_server`], so the atomic-write and
//! preserve-other-servers behaviour has exactly one implementation.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only --
//! gui_mcp_client`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::bin_resolve::is_ephemeral_build_path;
use crate::claude_config::{mcp_server_entry, patch_mcp_server};

/// A GUI MCP client that launchd (or an equivalent GUI launcher) starts.
///
/// Why (#6307): the failure this module exists for is not client-specific —
/// any GUI-launched client inherits the stripped `PATH`. Modelling the client
/// as an enum keeps the shared rendering in one place while letting each
/// client answer the one question that genuinely differs: whether it keeps a
/// local config file that a tool may write.
/// What: currently only ChatGPT desktop. Adding a client means adding a
/// variant plus its [`GuiMcpClient::local_config_path`] answer — the entry
/// rendering, validation, and write path are shared.
/// Test: `parse_accepts_known_spellings`, `parse_rejects_unknown_client`,
/// `chatgpt_has_no_writable_local_config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuiMcpClient {
    /// ChatGPT desktop for macOS.
    ChatGpt,
}

impl GuiMcpClient {
    /// Parse a `--client` value.
    ///
    /// Why: the CLI flag is a free-form string in both consuming crates, and
    /// accepting the obvious spellings costs nothing while a rejected
    /// `chat-gpt` would read as an unsupported client.
    /// What: case-insensitive match on `chatgpt`, `chat-gpt`, `chat_gpt`, and
    /// `openai`. Returns `None` for anything else so the caller can print the
    /// supported list.
    /// Test: `parse_accepts_known_spellings`, `parse_rejects_unknown_client`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "chatgpt" | "chat-gpt" | "chat_gpt" | "openai" => Some(Self::ChatGpt),
            _ => None,
        }
    }

    /// Every `--client` value this module accepts, for CLI help and errors.
    #[must_use]
    pub const fn supported() -> &'static [&'static str] {
        &["chatgpt"]
    }

    /// Human-readable client name for printed instructions.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT desktop",
        }
    }

    /// The client's own local MCP config file, when one exists that a tool may
    /// safely write.
    ///
    /// Why (#6307): a setup command may only write a file it can name. ChatGPT
    /// desktop keeps no such file — a sweep of
    /// `~/Library/Application Support/com.openai.chat/`,
    /// `~/Library/Preferences/com.openai.chat.plist`, `~/Library/Containers`,
    /// and the usual dotfile locations found no `mcp*.json` and no `mcpServers`
    /// key anywhere, and the connector form (the one whose working-directory
    /// field defaults to `~/code`) is filled in inside the app. Returning
    /// `None` is therefore the accurate answer, not a stub: the caller prints
    /// the entry and writes nothing.
    /// What: `None` for every client known today. A future client with a real
    /// file returns its path here and gets the shared write path for free.
    /// Test: `chatgpt_has_no_writable_local_config`.
    #[must_use]
    pub fn local_config_path(self, _home: &Path) -> Option<PathBuf> {
        match self {
            Self::ChatGpt => None,
        }
    }
}

/// A validated MCP registration for a GUI client.
///
/// Why (#6307): the two failures the issue reports are a bare `command` and a
/// working directory that does not exist. Making the type constructible only
/// through [`build_entry`] means neither shape can reach a client config or a
/// printed instruction.
/// What: an absolute `command`, its argument vector, and an existing
/// `working_dir`. Fields are public for reading; construction goes through
/// [`build_entry`].
/// Test: `build_entry_rejects_a_bare_command`,
/// `build_entry_rejects_a_missing_working_dir`, `entry_json_has_absolute_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuiClientEntry {
    /// The key the entry is registered under (e.g. `trusty-memory`).
    pub server_key: String,
    /// Absolute path to the executable the client must spawn.
    pub command: PathBuf,
    /// Argument vector passed to `command`.
    pub args: Vec<String>,
    /// An existing directory the client should start the server in.
    pub working_dir: PathBuf,
}

impl GuiClientEntry {
    /// Render the entry as the `{command, args, cwd}` JSON object clients use.
    ///
    /// Why: the `{command, args}` half is the same object every trusty-* MCP
    /// registration uses, so it is built by
    /// [`crate::claude_config::mcp_server_entry`] rather than by a second
    /// hand-rolled `json!` literal. `cwd` is the addition GUI clients need.
    /// What: `{"command": <abs path>, "args": [...], "cwd": <dir>}`.
    /// Test: `entry_json_has_absolute_command`, `entry_json_carries_cwd`.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let mut entry = mcp_server_entry(&self.command.to_string_lossy(), &args);
        if let Some(obj) = entry.as_object_mut() {
            obj.insert(
                "cwd".to_string(),
                Value::String(self.working_dir.to_string_lossy().into_owned()),
            );
        }
        entry
    }

    /// The exact values an operator types into a client's connector form.
    ///
    /// Why (#6307): when the client keeps no writable config, printing the
    /// three fields verbatim is the whole fix — the operator's error was a
    /// bare command name and a `~/code` working directory that does not exist,
    /// and both are values, not prose. A build-directory binary is called out
    /// because pasting a `target/debug` path into a GUI client leaves a
    /// registration that breaks the next `cargo clean`.
    /// What: a multi-line block naming Command, Arguments, and Working
    /// directory, followed by the caution line when the running binary is a
    /// build artifact.
    /// Test: `instructions_name_every_field`,
    /// `instructions_warn_about_a_build_directory_binary`.
    #[must_use]
    pub fn instructions(&self, client: GuiMcpClient) -> String {
        let mut out = format!(
            "Add an MCP server named `{}` in {} with these exact values:\n\
             \n  Command:           {}\n  Arguments:         {}\n  Working directory: {}\n",
            self.server_key,
            client.display_name(),
            self.command.display(),
            self.args.join(" "),
            self.working_dir.display(),
        );
        if is_ephemeral_build_path(&self.command) {
            out.push_str(
                "\nNote: that path is a build directory, not an installed binary. \
                 Run `cargo install --path <crate>` and re-run this command so the \
                 entry survives a `cargo clean`.\n",
            );
        }
        out
    }
}

/// What [`configure`] did.
///
/// Why: the two outcomes need different words from the caller — one ends in
/// "paste this", the other in "wrote it". Returning them as data keeps every
/// printing decision in the CLI layer and this module free of `println!`.
/// What: `PasteByHand` when the client keeps no writable config file;
/// `Wrote` otherwise, with `changed` false when the file already held an
/// identical entry.
/// Test: `configure_returns_paste_by_hand_for_chatgpt`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuiClientOutcome {
    /// The client has no config file a tool may write; print `entry`.
    PasteByHand {
        /// The registration the operator must enter by hand.
        entry: GuiClientEntry,
    },
    /// The registration was written to `path`.
    Wrote {
        /// The config file that was written.
        path: PathBuf,
        /// The registration that was written.
        entry: GuiClientEntry,
        /// False when the file already held an identical entry.
        changed: bool,
    },
}

/// Absolute, symlink-resolved path of the running executable.
///
/// Why (#6307): the owner's ruling is that the user never types a cargo path.
/// `current_exe` is the only source that is right no matter where the binary
/// was installed — on the machine that reported this bug the signed install is
/// not under `~/.cargo/bin` at all, so any assumed path would have been wrong.
/// Canonicalizing follows the symlink an installer may have left, which is
/// what a client needs to spawn directly.
/// What: `current_exe()`, then `canonicalize()`; a canonicalize failure falls
/// back to the raw path rather than failing the command. Errors when the path
/// is not absolute, which is the exact shape that breaks a GUI spawn.
/// Test: `running_binary_path_is_absolute`.
pub fn running_binary_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolve the running executable path")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    if !exe.is_absolute() {
        bail!(
            "the running executable resolved to a relative path ({}); \
             a GUI client cannot spawn it",
            exe.display()
        );
    }
    Ok(exe)
}

/// A working directory that exists, for clients that require one.
///
/// Why (#6307): the client's own form defaulted to `~/code`, which does not
/// exist on every machine, and a non-existent working directory fails the
/// spawn just as surely as a missing binary. The home directory always exists
/// and the servers do not care which directory they start in.
/// What: the user's home directory, checked for existence.
/// Test: `default_working_dir_exists`.
pub fn default_working_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
    if !home.is_dir() {
        bail!("home directory {} does not exist", home.display());
    }
    Ok(home)
}

/// Validate a GUI client registration.
///
/// Why (#6307): this is the gate the pre-fix code did not have. A bare command
/// name and a working directory that does not exist are the two ways the
/// registration the operator ends up with fails to spawn, and both are
/// checkable here, before any file is written or any instruction printed.
/// What: errors when `command` is relative (which includes a bare binary name)
/// or when `working_dir` is not an existing directory; returns the validated
/// [`GuiClientEntry`] otherwise.
/// Test: `build_entry_rejects_a_bare_command`,
/// `build_entry_rejects_a_missing_working_dir`, `build_entry_accepts_an_absolute_command`.
pub fn build_entry(
    server_key: &str,
    args: &[&str],
    command: &Path,
    working_dir: &Path,
) -> Result<GuiClientEntry> {
    if !command.is_absolute() {
        bail!(
            "MCP command `{}` is not an absolute path; a GUI client launched by \
             launchd sees only PATH=/usr/bin:/bin:/usr/sbin:/sbin and cannot find it",
            command.display()
        );
    }
    if !working_dir.is_dir() {
        bail!(
            "working directory {} does not exist; the client spawn fails before \
             the server starts",
            working_dir.display()
        );
    }
    Ok(GuiClientEntry {
        server_key: server_key.to_string(),
        command: command.to_path_buf(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
        working_dir: working_dir.to_path_buf(),
    })
}

/// Build — and where possible write — a GUI client's MCP registration.
///
/// Why (#6307): both `trusty-memory setup --client <name>` and `trusty-search
/// setup --client <name>` need the identical sequence: resolve the running
/// binary, pick an existing working directory, validate, then either write the
/// client's file or hand the values back to be printed. One implementation
/// keeps the two from drifting the way their Claude-settings phases once did.
/// What: composes [`running_binary_path`], [`default_working_dir`], and
/// [`build_entry`], then consults [`GuiMcpClient::local_config_path`]. When
/// that names a file, the entry is upserted through
/// [`crate::claude_config::patch_mcp_server`], which writes atomically and
/// leaves every other server's entry untouched; otherwise the entry comes back
/// as [`GuiClientOutcome::PasteByHand`].
/// Test: `configure_returns_paste_by_hand_for_chatgpt`,
/// `configure_writes_and_preserves_other_servers`.
pub fn configure(
    client: GuiMcpClient,
    server_key: &str,
    args: &[&str],
    home: &Path,
) -> Result<GuiClientOutcome> {
    let command = running_binary_path()?;
    let working_dir = default_working_dir()?;
    let entry = build_entry(server_key, args, &command, &working_dir)?;

    match client.local_config_path(home) {
        None => Ok(GuiClientOutcome::PasteByHand { entry }),
        Some(path) => {
            let changed = patch_mcp_server(&path, server_key, &entry.to_json())
                .with_context(|| format!("register {server_key} in {}", path.display()))?;
            Ok(GuiClientOutcome::Wrote {
                path,
                entry,
                changed,
            })
        }
    }
}

/// Run [`configure`] for one server and render the whole operator-facing report.
///
/// Why (#6307): `trusty-memory setup --client <name>` and `trusty-search setup
/// --client <name>` print the identical thing, differing only in the server key
/// and argument vector. Returning the finished text from here — rather than
/// letting each crate assemble its own `println!` sequence — is what keeps the
/// instruction wording from drifting between the two binaries, which is the
/// drift that produced three divergent Claude-settings writers before this
/// module family existed.
/// What: resolves the client name, runs [`configure`], and returns the block to
/// print: the paste-these-values instructions when the client keeps no writable
/// config, or a one-line confirmation of the file that was written. An unknown
/// client name is an error naming [`GuiMcpClient::supported`].
/// Test: `report_for_chatgpt_names_the_manual_step`,
/// `report_rejects_an_unknown_client`.
pub fn report(client_name: &str, server_key: &str, args: &[&str], home: &Path) -> Result<String> {
    let client = GuiMcpClient::parse(client_name).ok_or_else(|| {
        anyhow!(
            "unknown GUI client `{client_name}`; supported: {}",
            GuiMcpClient::supported().join(", ")
        )
    })?;

    Ok(match configure(client, server_key, args, home)? {
        GuiClientOutcome::PasteByHand { entry } => format!(
            "{}\n{} keeps no local MCP config file this command may write, so \
             nothing on disk was changed.\n",
            entry.instructions(client),
            client.display_name(),
        ),
        GuiClientOutcome::Wrote {
            path,
            entry,
            changed,
        } => {
            let verb = if changed {
                "Registered"
            } else {
                "Already registered"
            };
            format!(
                "{verb} `{}` for {} in {}\n  Command:           {}\n  \
                 Arguments:         {}\n  Working directory: {}\n",
                entry.server_key,
                client.display_name(),
                path.display(),
                entry.command.display(),
                entry.args.join(" "),
                entry.working_dir.display(),
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trusty-gui-mcp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    #[test]
    fn parse_accepts_known_spellings() {
        for raw in ["chatgpt", "ChatGPT", " chat-gpt ", "chat_gpt", "OpenAI"] {
            assert_eq!(
                GuiMcpClient::parse(raw),
                Some(GuiMcpClient::ChatGpt),
                "should parse {raw}"
            );
        }
    }

    #[test]
    fn parse_rejects_unknown_client() {
        assert_eq!(GuiMcpClient::parse("claude-desktop"), None);
        assert_eq!(GuiMcpClient::parse(""), None);
    }

    #[test]
    fn chatgpt_has_no_writable_local_config() {
        let home = Path::new("/Users/nobody");
        assert_eq!(GuiMcpClient::ChatGpt.local_config_path(home), None);
    }

    /// #6307 regression: the pre-fix registration is a bare binary name, which
    /// is exactly what exits 127 under launchd's PATH. It must not build.
    #[test]
    fn build_entry_rejects_a_bare_command() {
        let dir = tempdir();
        let err = build_entry(
            "trusty-memory",
            &["serve"],
            Path::new("trusty-memory"),
            &dir,
        )
        .expect_err("a bare command name must be rejected");
        assert!(
            err.to_string().contains("not an absolute path"),
            "unexpected error: {err}"
        );
    }

    /// #6307 regression: the client form's `~/code` default does not exist on
    /// every machine and fails the spawn on its own.
    #[test]
    fn build_entry_rejects_a_missing_working_dir() {
        let dir = tempdir();
        let missing = dir.join("code-that-does-not-exist");
        let exe = dir.join("trusty-memory");
        std::fs::write(&exe, b"").expect("write fake binary");
        let err = build_entry("trusty-memory", &["serve"], &exe, &missing)
            .expect_err("a missing working directory must be rejected");
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn build_entry_accepts_an_absolute_command() {
        let dir = tempdir();
        let exe = dir.join("trusty-search");
        std::fs::write(&exe, b"").expect("write fake binary");
        let entry =
            build_entry("trusty-search", &["serve"], &exe, &dir).expect("entry should build");
        assert_eq!(entry.command, exe);
        assert_eq!(entry.args, vec!["serve".to_string()]);
        assert_eq!(entry.working_dir, dir);
    }

    /// #6307 regression: what reaches the client must be an absolute command,
    /// never the bare name the pre-fix `mcp_server_entry` call produced, and
    /// its `cwd` must name a directory that exists.
    #[test]
    fn entry_json_has_absolute_command() {
        let dir = tempdir();
        let exe = dir.join("trusty-memory");
        std::fs::write(&exe, b"").expect("write fake binary");
        let entry =
            build_entry("trusty-memory", &["serve"], &exe, &dir).expect("entry should build");

        let rendered = entry.to_json();
        let command = rendered["command"]
            .as_str()
            .expect("command is a JSON string");
        assert!(
            Path::new(command).is_absolute(),
            "command must be absolute, got {command}"
        );
        assert_ne!(
            command, "trusty-memory",
            "the bare binary name is the pre-fix shape that exits 127 under launchd"
        );

        let cwd = rendered["cwd"].as_str().expect("cwd is a JSON string");
        assert!(
            Path::new(cwd).is_dir(),
            "cwd must exist at write time, got {cwd}"
        );
    }

    #[test]
    fn entry_json_carries_cwd() {
        let dir = tempdir();
        let exe = dir.join("trusty-memory");
        std::fs::write(&exe, b"").expect("write fake binary");
        let entry =
            build_entry("trusty-memory", &["serve"], &exe, &dir).expect("entry should build");
        assert_eq!(
            entry.to_json(),
            json!({
                "command": exe.to_string_lossy(),
                "args": ["serve"],
                "cwd": dir.to_string_lossy(),
            })
        );
    }

    #[test]
    fn instructions_name_every_field() {
        let dir = tempdir();
        // An installed location, not the tempdir: `is_ephemeral_build_path`
        // counts anything under the system temp root as a build artifact, so a
        // tempdir binary would trip the caution this case asserts is absent.
        // `build_entry` validates the command's shape, never its existence.
        let exe = Path::new("/usr/local/bin/trusty-memory");
        let entry = build_entry("trusty-memory", &["serve"], exe, &dir).expect("entry builds");
        let text = entry.instructions(GuiMcpClient::ChatGpt);
        assert!(text.contains("ChatGPT desktop"), "{text}");
        assert!(text.contains("/usr/local/bin/trusty-memory"), "{text}");
        assert!(text.contains("serve"), "{text}");
        assert!(text.contains(&dir.display().to_string()), "{text}");
        assert!(!text.contains("build directory"), "{text}");
    }

    #[test]
    fn instructions_warn_about_a_build_directory_binary() {
        let dir = tempdir();
        let build_dir = dir.join("target").join("debug");
        std::fs::create_dir_all(&build_dir).expect("create build dir");
        let exe = build_dir.join("trusty-memory");
        std::fs::write(&exe, b"").expect("write fake binary");
        let entry =
            build_entry("trusty-memory", &["serve"], &exe, &dir).expect("entry should build");
        let text = entry.instructions(GuiMcpClient::ChatGpt);
        assert!(text.contains("build directory"), "{text}");
    }

    #[test]
    fn running_binary_path_is_absolute() {
        let exe = running_binary_path().expect("the test binary path resolves");
        assert!(exe.is_absolute(), "got {}", exe.display());
    }

    #[test]
    fn default_working_dir_exists() {
        let dir = default_working_dir().expect("home directory resolves");
        assert!(dir.is_dir(), "got {}", dir.display());
    }

    #[test]
    fn configure_returns_paste_by_hand_for_chatgpt() {
        let home = tempdir();
        let outcome = configure(GuiMcpClient::ChatGpt, "trusty-memory", &["serve"], &home)
            .expect("configure should succeed");
        match outcome {
            GuiClientOutcome::PasteByHand { entry } => {
                assert!(entry.command.is_absolute());
                assert!(entry.working_dir.is_dir());
            }
            other => panic!("expected PasteByHand, got {other:?}"),
        }
    }

    #[test]
    fn report_for_chatgpt_names_the_manual_step() {
        let home = tempdir();
        let text =
            report("chatgpt", "trusty-memory", &["serve"], &home).expect("report should render");
        assert!(text.contains("ChatGPT desktop"), "{text}");
        assert!(text.contains("Working directory:"), "{text}");
        assert!(text.contains("nothing on disk was changed"), "{text}");
    }

    #[test]
    fn report_rejects_an_unknown_client() {
        let home = tempdir();
        let err = report("cursor", "trusty-memory", &["serve"], &home)
            .expect_err("an unknown client must be rejected");
        assert!(err.to_string().contains("chatgpt"), "{err}");
    }

    /// The write path is shared with `claude_config::patch_mcp_server`, so this
    /// pins the property that matters for a future file-backed client: an
    /// unrelated server's entry survives the upsert.
    #[test]
    fn configure_writes_and_preserves_other_servers() {
        let dir = tempdir();
        let path = dir.join("config.json");
        std::fs::write(
            &path,
            br#"{"mcpServers":{"other":{"command":"/bin/true"}}}"#,
        )
        .expect("seed config");

        let exe = dir.join("trusty-search");
        std::fs::write(&exe, b"").expect("write fake binary");
        let entry =
            build_entry("trusty-search", &["serve"], &exe, &dir).expect("entry should build");

        let changed = patch_mcp_server(&path, "trusty-search", &entry.to_json())
            .expect("upsert should succeed");
        assert!(changed, "first write changes the file");

        let raw = std::fs::read_to_string(&path).expect("read back");
        let value: Value = serde_json::from_str(&raw).expect("parse back");
        assert_eq!(value["mcpServers"]["other"]["command"], "/bin/true");
        assert_eq!(
            value["mcpServers"]["trusty-search"]["command"],
            exe.to_string_lossy().as_ref()
        );

        let again = patch_mcp_server(&path, "trusty-search", &entry.to_json())
            .expect("second upsert should succeed");
        assert!(!again, "an identical entry must not rewrite the file");
    }
}
