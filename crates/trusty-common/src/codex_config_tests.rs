//! Tests for [`super`] — one per config SHAPE, not per happy path.
//!
//! Why: every CRITICAL defect this module has had was a shape the author did
//! not picture. `mcp_servers = { … }` is inline TOML that serde accepts, and
//! treating it as "broken" deleted every other registered server; a per-server
//! entry in inline form carried the operator's provider secret in `env`, and
//! replacing the entry deleted it. Shape coverage is the point of this file.
//!
//! Test: `cargo test -p trusty-common --features codex-config codex_config`

use super::*;

/// The spec every test registers unless it is testing spec fields themselves.
fn serve_spec() -> McpServerSpec {
    McpServerSpec::new("trusty-search", &["serve"])
}

/// Seed `config.toml` in a fresh tempdir and return `(dir, path)`.
fn seeded(contents: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = codex_config_path(tmp.path());
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir .codex");
    std::fs::write(&path, contents).expect("seed config");
    (tmp, path)
}

/// Parse `path` and return the argument vector registered for `key`.
fn args_of(path: &Path, key: &str) -> Vec<String> {
    let doc: DocumentMut = std::fs::read_to_string(path)
        .expect("read")
        .parse()
        .expect("valid TOML");
    doc[MCP_SERVERS_TABLE][key]["args"]
        .as_array()
        .expect("args array")
        .iter()
        .map(|v| v.as_str().expect("string arg").to_owned())
        .collect()
}

/// Why: the path is the contract with the Codex CLI; getting it wrong makes
/// every other function in this module write to a file nobody reads.
#[test]
fn codex_config_path_is_under_dot_codex() {
    let p = codex_config_path(Path::new("/Users/x"));
    assert_eq!(p, PathBuf::from("/Users/x/.codex/config.toml"));
}

/// Why (#5264): a machine with no Codex config yet must still end up with a
/// working registration rather than an error.
#[test]
fn patch_mcp_server_creates_missing_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = codex_config_path(tmp.path());

    let outcome = patch_mcp_server(&path, "trusty-search", &serve_spec()).expect("patch");
    assert_eq!(outcome, PatchOutcome::Created);
    assert_eq!(args_of(&path, "trusty-search"), vec!["serve".to_string()]);
}

/// Why: setup commands are re-run constantly. An already-correct entry must not
/// be rewritten — a republish churns the mtime and burns an `fsync` even when
/// the bytes are identical, which is why the no-op is signalled to `json_rmw`
/// rather than left to a byte comparison.
#[test]
fn patch_mcp_server_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = codex_config_path(tmp.path());

    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Created
    );
    let before = std::fs::read_to_string(&path).unwrap();
    let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

    let outcome = patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap();
    assert_eq!(outcome, PatchOutcome::Unchanged);
    assert!(!outcome.wrote());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        mtime_before,
        "an unchanged entry must not republish the file"
    );
}

/// Why (#5264): the reporter's registration. Codex exec'd the bare binary,
/// which printed help and exited before MCP initialization while the connection
/// still showed as enabled.
#[test]
fn patch_mcp_server_repairs_empty_args() {
    let (_tmp, path) =
        seeded("[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\nargs = []\n");
    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Repaired
    );
    assert_eq!(args_of(&path, "trusty-search"), vec!["serve".to_string()]);
}

/// Why: a single joined string is one `argv[1]`, so the process is launched as
/// `trusty-search "serve --port 7878"` and never parses. It passes any
/// "is args non-empty?" check.
#[test]
fn patch_mcp_server_repairs_joined_args() {
    let (_tmp, path) = seeded(
        "[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\nargs = [\"serve --port 7878\"]\n",
    );
    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Repaired
    );
    assert_eq!(args_of(&path, "trusty-search"), vec!["serve".to_string()]);
}

/// Why (#5264 follow-up): after hand-editing, the reporter's config held
/// `args = ["[\"serve\"]"]` — one literal argument whose TEXT looks like an
/// argument vector. The process receives `["serve"]` as a single token.
#[test]
fn patch_mcp_server_repairs_nested_json_string_args() {
    let (_tmp, path) = seeded(
        "[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\nargs = [\"[\\\"serve\\\"]\"]\n",
    );
    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Repaired
    );
    assert_eq!(args_of(&path, "trusty-search"), vec!["serve".to_string()]);
}

/// CRITICAL regression (#5264 review): `mcp_servers` written as an INLINE table
/// is not an `Item::Table`, and the first cut replaced it wholesale — silently
/// deleting every other registered MCP server, returning "wrote", and then
/// reporting "already correct" on the second run over the wreckage.
///
/// This is valid TOML that serde accepts, not a broken config.
#[test]
fn patch_mcp_server_preserves_an_inline_mcp_servers_table() {
    let (_tmp, path) =
        seeded("mcp_servers = { other = { command = \"other-server\", args = [\"run\"] } }\n");

    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Created
    );

    let text = std::fs::read_to_string(&path).unwrap();
    let doc: DocumentMut = text.parse().expect("still valid TOML");
    let servers = doc[MCP_SERVERS_TABLE]
        .as_table_like()
        .expect("mcp_servers still table-like");
    assert!(
        servers.get("other").is_some(),
        "another registered server was deleted:\n{text}"
    );
    assert_eq!(
        servers
            .get("other")
            .and_then(Item::as_table_like)
            .and_then(|e| e.get("command"))
            .and_then(Item::as_str),
        Some("other-server"),
        "the surviving entry was mangled:\n{text}"
    );
    assert_eq!(args_of(&path, "trusty-search"), vec!["serve".to_string()]);
}

/// CRITICAL regression (#5264 review): a per-server entry in INLINE form.
/// Replacing it deleted `env`, where Codex stores provider credentials — a
/// secret the operator may have no other copy of — while reporting a write on a
/// config that needed nothing but an `args` repair.
#[test]
fn patch_mcp_server_preserves_an_inline_server_entry() {
    let (_tmp, path) = seeded(
        "[mcp_servers]\ntrusty-search = { command = \"trusty-search\", args = [], \
         env = { OPENROUTER_API_KEY = \"sk-secret\" } }\n",
    );

    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Repaired
    );

    let text = std::fs::read_to_string(&path).unwrap();
    let doc: DocumentMut = text.parse().expect("still valid TOML");
    let entry = doc[MCP_SERVERS_TABLE]["trusty-search"]
        .as_table_like()
        .expect("entry still table-like");
    assert_eq!(
        entry
            .get("env")
            .and_then(Item::as_table_like)
            .and_then(|e| e.get("OPENROUTER_API_KEY"))
            .and_then(Item::as_str),
        Some("sk-secret"),
        "a provider credential was deleted:\n{text}"
    );
    assert_eq!(args_of(&path, "trusty-search"), vec!["serve".to_string()]);
}

/// Why: `env` is additive. A caller setting one variable must not remove the
/// operator's others.
#[test]
fn patch_mcp_server_merges_env_without_dropping_existing_keys() {
    let (_tmp, path) = seeded(
        "[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\nargs = [\"serve\"]\n\
         [mcp_servers.trusty-search.env]\nKEEP_ME = \"yes\"\n",
    );

    let spec = serve_spec().with_env("RUST_LOG", "info");
    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &spec).unwrap(),
        PatchOutcome::Repaired
    );

    let text = std::fs::read_to_string(&path).unwrap();
    let doc: DocumentMut = text.parse().unwrap();
    let env = doc[MCP_SERVERS_TABLE]["trusty-search"]["env"]
        .as_table_like()
        .expect("env table");
    assert_eq!(
        env.get("KEEP_ME").and_then(Item::as_str),
        Some("yes"),
        "an existing env key was dropped:\n{text}"
    );
    assert_eq!(env.get("RUST_LOG").and_then(Item::as_str), Some("info"));
}

/// Why: `cwd` and `startup_timeout_sec` are the fields the old positional API
/// could not express at all.
#[test]
fn patch_mcp_server_writes_optional_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = codex_config_path(tmp.path());

    let spec = serve_spec()
        .with_cwd("/srv/project")
        .with_startup_timeout_sec(45);
    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &spec).unwrap(),
        PatchOutcome::Created
    );
    // And a re-run with the same spec is still a no-op.
    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &spec).unwrap(),
        PatchOutcome::Unchanged
    );

    let doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    let entry = &doc[MCP_SERVERS_TABLE]["trusty-search"];
    assert_eq!(entry["cwd"].as_str(), Some("/srv/project"));
    assert_eq!(entry["startup_timeout_sec"].as_integer(), Some(45));
}

/// Why: when a key genuinely holds neither table spelling, the only safe move is
/// to stop. Overwriting it is what caused the CRITICALs above; the operator
/// needs to be told which key to look at.
#[test]
fn patch_mcp_server_rejects_a_non_table_mcp_servers() {
    let (_tmp, path) = seeded("mcp_servers = \"nonsense\"\n");
    let before = std::fs::read_to_string(&path).unwrap();

    let err = patch_mcp_server(&path, "trusty-search", &serve_spec())
        .expect_err("a scalar mcp_servers must not be overwritten");
    assert!(
        matches!(&err, CodexConfigError::NotATable { key } if key == "mcp_servers"),
        "got {err:?}"
    );
    assert!(err.to_string().contains("mcp_servers"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "a rejected patch must leave the file untouched"
    );
}

/// Why: same rule one level down — a scalar where the server entry belongs.
#[test]
fn patch_mcp_server_rejects_a_non_table_entry() {
    let (_tmp, path) = seeded("[mcp_servers]\ntrusty-search = 42\n");
    let before = std::fs::read_to_string(&path).unwrap();

    let err = patch_mcp_server(&path, "trusty-search", &serve_spec())
        .expect_err("a scalar entry must not be overwritten");
    assert!(
        matches!(&err, CodexConfigError::NotATable { key } if key == "mcp_servers.trusty-search"),
        "got {err:?}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// Why: this writes into a file the operator owns and maintains by hand.
/// Clobbering their comments, other servers, or unrelated settings would be a
/// worse defect than the one being fixed.
#[test]
fn patch_mcp_server_preserves_other_servers_and_comments() {
    let (_tmp, path) = seeded(
        "# my codex config\nmodel = \"gpt-5\"\n\n\
         [mcp_servers.other]\ncommand = \"other-server\"\nargs = [\"run\"]\n",
    );

    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Created
    );

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# my codex config"), "comment lost:\n{text}");
    assert!(text.contains("model = \"gpt-5\""), "setting lost:\n{text}");
    let doc: DocumentMut = text.parse().unwrap();
    assert_eq!(
        doc[MCP_SERVERS_TABLE]["other"]["command"].as_str(),
        Some("other-server"),
        "another server's registration was clobbered:\n{text}"
    );
}

/// Why: an operator who ran `chmod 600 ~/.codex/config.toml` did so because the
/// file holds credentials. Publishing through `File::create` would reset it to
/// 0644 and quietly hand it to every account on the machine.
#[cfg(unix)]
#[test]
fn patch_mcp_server_preserves_file_mode() {
    use std::os::unix::fs::PermissionsExt;

    let (_tmp, path) =
        seeded("[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\nargs = []\n");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Repaired
    );

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "publish widened the config to {mode:o}");
}

/// Why: a config symlinked into a dotfiles repo is a normal setup. Renaming
/// over the LINK detaches it from the repo and leaves the real file stale, so
/// the operator's next `git diff` shows nothing and their change is lost.
#[cfg(unix)]
#[test]
fn patch_mcp_server_writes_through_a_symlink() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let real = tmp.path().join("dotfiles").join("codex.toml");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(
        &real,
        "[mcp_servers.trusty-search]\ncommand = \"trusty-search\"\nargs = []\n",
    )
    .unwrap();

    let link = codex_config_path(tmp.path());
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    assert_eq!(
        patch_mcp_server(&link, "trusty-search", &serve_spec()).unwrap(),
        PatchOutcome::Repaired
    );

    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the symlink was replaced by a regular file"
    );
    assert!(
        std::fs::read_to_string(&real)
            .unwrap()
            .contains("\"serve\""),
        "the link target was not updated"
    );
}

/// #5264 HIGH: a config this call CREATES holds a provider credential the first
/// time an operator adds one. Mode preservation cannot help a file that does not
/// exist yet, so the create path declares its own mode.
#[cfg(unix)]
#[test]
fn patch_mcp_server_creates_a_private_file() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let path = codex_config_path(tmp.path());
    assert!(!path.exists(), "precondition: nothing there yet");

    let spec = serve_spec().with_env("OPENROUTER_API_KEY", "sk-live-SECRET");
    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &spec).unwrap(),
        PatchOutcome::Created
    );

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "a fresh Codex config is {mode:o}, not 600");
    let dir_mode = std::fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "the created .codex dir is {dir_mode:o}");
}

/// #5264 (review): `McpServerSpec::new(cmd, &[])` cannot infer its element type
/// (E0283), so the natural "no arguments" spelling needs its own constructor.
/// This test exists to fail at COMPILE time if that regresses.
#[test]
fn stdio_spec_needs_no_turbofish() {
    let spec = McpServerSpec::stdio("trusty-memory");
    assert_eq!(spec.command, "trusty-memory");
    assert!(spec.args.is_empty());
}

/// #5264 (review): a `u64` seconds field would let `u64::MAX` write `-1` into a
/// TOML integer and then round-trip as `Unchanged`. The field is `u32`, so the
/// widest value it can hold is still lossless.
#[test]
fn a_maximal_startup_timeout_round_trips() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = codex_config_path(tmp.path());
    let spec = serve_spec().with_startup_timeout_sec(u32::MAX);

    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &spec).unwrap(),
        PatchOutcome::Created
    );
    let doc: DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
    assert_eq!(
        doc[MCP_SERVERS_TABLE]["trusty-search"]["startup_timeout_sec"].as_integer(),
        Some(i64::from(u32::MAX)),
        "the timeout must not wrap negative"
    );
    assert_eq!(
        patch_mcp_server(&path, "trusty-search", &spec).unwrap(),
        PatchOutcome::Unchanged,
        "a wrapped value would have compared equal by accident"
    );
}
