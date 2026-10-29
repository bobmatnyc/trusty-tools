//! Tests for [`super::VercelBackend`] over [`VercelShim`], a fake `vercel`
//! run by absolute path (#9069 PR 2). No test runs the real `vercel`, reads
//! the real machine config, or changes this process's environment; the one
//! test that needs hostile inherited variables re-runs the test binary with
//! them set from birth.
//!
//! Test: itself.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::settings::{SYSTEM_PATH, steering_vars};
use super::shim::{ORG, PROJECT, VercelShim};
use super::*;
use crate::api::{OwnerName, RepoName};

/// A value with no trailing newline, a quote, a space and non-ASCII text.
const VALUE: &str = "vcl-canary-9069 \"q\" é-0123456789abcdef";

/// The calls a clean push of `API_KEY` makes, `--cwd` and on cut off.
const CLEAN_PUSH: [&str; 6] = [
    "env ls production",
    "env ls preview",
    "env add API_KEY production --sensitive --force",
    "env add API_KEY preview --sensitive --force",
    "env ls production",
    "env ls preview",
];

fn web() -> VaultName {
    VaultName::new("trusty/acme/web").unwrap()
}

fn key(name: &str) -> SecretKey {
    SecretKey::new(name).unwrap()
}

fn identity(owner: &str, repo: &str) -> RemoteIdentity {
    RemoteIdentity::new(OwnerName::new(owner).unwrap(), RepoName::new(repo).unwrap())
}

/// Push `name` = `value` through `backend`.
fn push_value(backend: &VercelBackend, name: &str, value: &str) -> Result<(), SecretsError> {
    backend.set(&web(), &key(name), &SecretValue::new(value))
}

/// Push `name` = [`VALUE`] through `backend`.
fn push(backend: &VercelBackend, name: &str) -> Result<(), SecretsError> {
    push_value(backend, name, VALUE)
}

/// The `Backend` error's reason, or a panic for any other result.
fn reason(result: Result<(), SecretsError>) -> String {
    match result {
        Err(SecretsError::Backend {
            backend, reason, ..
        }) => {
            assert_eq!(backend, BackendId::VERCEL);
            reason
        }
        other => panic!("expected a backend error, got {other:?}"),
    }
}

/// Why: V2, C2 — the value reaches `vercel env add` on stdin only,
/// byte-exact with no newline added, once for `production` then once for
/// `preview`, bracketed by the baseline and the confirming listings.
/// Test: itself.
#[test]
fn vercel_push_runs_production_then_preview_with_the_value_on_stdin() {
    let shim = VercelShim::new();
    push(&shim.backend(), "API_KEY").unwrap();

    assert_eq!(shim.commands(), CLEAN_PUSH);
    assert_eq!(shim.calls().matches(" --cwd ").count(), 6);
    for target in TARGETS {
        assert_eq!(shim.stdin("API_KEY", target).as_deref(), Some(VALUE));
        assert_eq!(shim.stored(target, "API_KEY").as_deref(), Some(VALUE));
    }
    assert!(!shim.calls().contains(VALUE) && !shim.env_log().contains(VALUE));
    assert!(!format!("{:?}", shim.backend()).contains(VALUE));
}

/// Why: V2, D10 — `development` is never pushed, read or named, on a clean
/// push or a failed one.
/// Test: itself.
#[test]
fn vercel_push_never_targets_development() {
    let shim = VercelShim::new();
    push(&shim.backend(), "API_KEY").unwrap();
    shim.fail_add("OTHER", "preview");
    reason(push(&shim.backend(), "OTHER"));
    assert!(!shim.calls().contains("development"), "{}", shim.calls());
    assert_eq!(shim.stored("development", "API_KEY"), None);
    assert!(!TARGETS.contains(&"development"));
}

/// Why: V3 — `production` landed but `preview` failed, so the key fails and
/// the reason says a target may have been written; no confirming listing
/// runs. A failing `production` stops the key before `preview`.
/// Red when a key counts as pushed because one target took it.
/// Test: itself.
#[test]
fn vercel_one_target_failing_fails_the_key() {
    let shim = VercelShim::new();
    shim.fail_add("API_KEY", "preview");
    let failed = reason(push(&shim.backend(), "API_KEY"));
    assert!(
        failed.contains("a target may have been written"),
        "{failed}"
    );
    assert_eq!(
        shim.stored("production", "API_KEY").as_deref(),
        Some(VALUE),
        "production landed"
    );
    assert_eq!(shim.commands(), CLEAN_PUSH[..4]);

    let shim = VercelShim::new();
    shim.fail_add("API_KEY", "production");
    let failed = reason(push(&shim.backend(), "API_KEY"));
    assert!(
        failed.contains("a target may have been written"),
        "{failed}"
    );
    assert_eq!(shim.stdin("API_KEY", "preview"), None, "preview never ran");
}

/// Why: C4 — an add that hangs on `preview`, after `production` landed, is
/// killed at its timeout and fails the key with the may-have-been-written
/// reason.
/// Test: itself.
#[test]
fn vercel_timeout_mid_targets_fails_the_key() {
    let shim = VercelShim::new();
    shim.slow("preview");
    let mut settings = shim.settings();
    settings.timeout = Duration::from_millis(500);
    let started = Instant::now();
    let failed = reason(push(&shim.backend_with(settings), "API_KEY"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(
        failed.contains("a target may have been written"),
        "{failed}"
    );
    assert_eq!(shim.stored("production", "API_KEY").as_deref(), Some(VALUE));
}

/// Why: C4 — an add that reads part of the value and exits 0 is never a
/// push, even though the project then lists the variable.
/// Test: itself.
#[test]
fn vercel_short_stdin_read_fails_the_key() {
    let shim = VercelShim::new();
    shim.short();
    let big = "v".repeat(4 * 1024 * 1024);
    let failed = reason(push_value(&shim.backend(), "API_KEY", &big));
    assert!(
        failed.contains("a target may have been written"),
        "{failed}"
    );
    assert_eq!(shim.stored("production", "API_KEY").as_deref(), Some("x\n"));
    assert_eq!(shim.commands(), CLEAN_PUSH[..3]);
}

/// Why: V6, D8 — exit 0 alone never counts: an add that stores nothing
/// leaves the variable off the listing, and the key fails.
/// Red when exit 0 is trusted.
/// Test: itself.
#[test]
fn vercel_missing_from_the_listing_after_exit_zero_is_a_failure() {
    let shim = VercelShim::new();
    shim.noop();
    let failed = reason(push(&shim.backend(), "API_KEY"));
    assert!(failed.contains("does not list the variable"), "{failed}");
    assert_eq!(shim.stdin("API_KEY", "production").as_deref(), Some(VALUE));
    assert_eq!(shim.commands(), CLEAN_PUSH[..5]);
}

/// Why: V6 — a listing the parser does not recognise is a failure, never
/// "absent" and never "listed": before the push it stops the key with
/// nothing written; after it, the key fails as may-have-been-written.
/// Test: itself.
#[test]
fn vercel_unparseable_listing_is_a_failure() {
    let shim = VercelShim::new();
    shim.garbage_ls_call(1);
    let failed = reason(push(&shim.backend(), "API_KEY"));
    assert!(failed.contains("could not be interpreted"), "{failed}");
    assert_eq!(shim.stdin("API_KEY", "production"), None, "nothing pushed");

    let shim = VercelShim::new();
    shim.garbage_ls_call(4);
    let failed = reason(push(&shim.backend(), "API_KEY"));
    assert!(failed.contains("may have been written"), "{failed}");
    assert_eq!(shim.stored("preview", "API_KEY").as_deref(), Some(VALUE));
}

/// Why: C4, D8 — a push whose confirming listing fails is a failure, fail
/// closed, and says the value may have been written.
/// Test: itself.
#[test]
fn vercel_verify_read_failure_after_push_fails_the_key() {
    let shim = VercelShim::new();
    shim.fail_ls_call(3);
    let failed = reason(push(&shim.backend(), "API_KEY"));
    assert!(failed.contains("may have been written"), "{failed}");
    assert_eq!(shim.stdin("API_KEY", "preview").as_deref(), Some(VALUE));
}

/// Why: V5, D9 — an existing variable is overwritten in place with
/// `--force`, never removed and re-added, but a listing cannot prove the
/// value changed, so the key fails with the fixed reason. Red when an
/// update counts on presence alone.
/// Test: itself.
#[test]
fn vercel_existing_variable_is_updated_in_place_and_fails_unproven() {
    let shim = VercelShim::new();
    shim.holds("production", "API_KEY", "old-value");
    let failed = reason(push(&shim.backend(), "API_KEY"));
    assert!(failed.contains("already existed"), "{failed}");
    assert_eq!(shim.commands(), CLEAN_PUSH);
    assert!(!shim.calls().contains(" rm "), "{}", shim.calls());
    for target in TARGETS {
        assert_eq!(shim.stored(target, "API_KEY").as_deref(), Some(VALUE));
    }
}

/// Why: a signed-out CLI is `BackendLocked` with the fixed login hint,
/// found by the baseline listing, and the value is never pushed.
/// Test: itself.
#[test]
fn vercel_signed_out_preflight_is_backend_locked_and_never_pushes() {
    let shim = VercelShim::new();
    shim.locked();
    match push(&shim.backend(), "API_KEY") {
        Err(SecretsError::BackendLocked { backend, hint }) => {
            assert_eq!(backend, BackendId::VERCEL);
            assert!(hint.contains("vercel login"), "{hint}");
        }
        other => panic!("expected BackendLocked, got {other:?}"),
    }
    assert_eq!(shim.stdin("API_KEY", "production"), None);
    assert_eq!(shim.calls().lines().count(), 1);
}

/// Why: a key Vercel refuses as a variable name is refused before any
/// spawn; the name is pushed as given.
/// Test: itself.
#[test]
fn vercel_key_names_vercel_refuses_never_spawn() {
    let shim = VercelShim::new();
    for name in ["a.b", "a-b", "1ABC"] {
        let refused = reason(push(&shim.backend(), name));
        assert!(refused.contains("[A-Za-z0-9_]"), "{name}: {refused}");
    }
    assert_eq!(shim.calls(), "", "nothing spawned");
    push(&shim.backend(), "lower_case").unwrap();
    assert!(shim.calls().contains("env add lower_case production"));
}

/// Why: C12 — an add that echoes the value to stdout and stderr, then
/// fails, yields an error and a `Debug` with neither the value nor the
/// CLI's text; a read-back and a delete are refused without spawning.
/// Test: itself.
#[test]
fn vercel_errors_never_carry_the_value_or_cli_output() {
    let shim = VercelShim::new();
    shim.echo();
    shim.fail_add("API_KEY", "production");
    let backend = shim.backend();
    let failed = push(&backend, "API_KEY").unwrap_err();
    assert_eq!(
        shim.stdin("API_KEY", "production").as_deref(),
        Some(VALUE),
        "the CLI saw and echoed the value"
    );
    let text = format!("{failed} {failed:?} {backend:?}");
    assert!(
        !text.contains(VALUE) && !text.contains("status 500"),
        "{text}"
    );

    let calls = shim.calls();
    assert!(matches!(
        backend.get(&web(), &key("API_KEY")),
        Err(SecretsError::Unsupported {
            operation: "get",
            ..
        })
    ));
    assert!(matches!(
        backend.delete(&web(), &key("API_KEY")),
        Err(SecretsError::Unsupported {
            operation: "delete",
            ..
        })
    ));
    assert_eq!(shim.calls(), calls, "no read-back or delete spawned");
    assert_eq!(
        backend.capabilities(),
        Capabilities::WRITE | Capabilities::SYNC_TARGET
    );
}

/// Why: D16 — the child's `PATH` is the pinned program's directory and the
/// system directories, so the spawner's `PATH` never picks `node`; a
/// configured global config directory is passed as `--global-config`.
/// Test: itself.
#[test]
fn vercel_path_overlay_is_the_pinned_program_dir_and_system_dirs() {
    let shim = VercelShim::new();
    let config_dir = shim.dir().join("vercel-config");
    std::fs::create_dir(&config_dir).unwrap();
    let mut settings = shim.settings();
    settings.global_config = Some(config_dir.clone());
    push(&shim.backend_with(settings), "API_KEY").unwrap();

    let bin = shim.program().parent().unwrap().display().to_string();
    let want = format!("PATH={bin}:{SYSTEM_PATH}");
    let env = shim.env_log();
    let paths: Vec<&str> = env
        .lines()
        .filter(|line| line.starts_with("PATH="))
        .collect();
    assert_eq!(paths, [want.as_str(); 6]);
    let flag = format!("--global-config {}", config_dir.display());
    assert_eq!(shim.calls().matches(&flag).count(), 6, "{}", shim.calls());
}

/// Set in the child [`vercel_child_runs_in_an_empty_dir_with_mapping_ids_and_no_inherited_vercel_env`]
/// spawns; names the one test body that may run there.
const HOSTILE_CHILD: &str = "TRUSTY_SECRETS_TEST_9069_VERCEL_HOSTILE_CHILD";

/// The hostile values the child inherits; none may reach `vercel`.
///
/// #9069: D3, D4 — after the four `VERCEL_*` names, one name from each other
/// stripped group: `NODE_`, `NOW_`, `npm_config_`, `NPM_CONFIG_`, the proxy
/// names in both cases, and the `SSL_CERT_*` names.
const HOSTILE: [(&str, &str); 14] = [
    ("VERCEL_ORG_ID", "team_hostile9069"),
    ("VERCEL_PROJECT_ID", "prj_hostile9069"),
    ("VERCEL_TOKEN", "tok_hostile9069"),
    ("VERCEL_SCOPE", "scope_hostile9069"),
    ("NODE_OPTIONS", "--require=/hostile9069/node_options.js"),
    ("NODE_EXTRA_CA_CERTS", "/hostile9069/node_ca.pem"),
    ("NOW_TOKEN", "now_tok_hostile9069"),
    (
        "npm_config_registry",
        "http://hostile9069.invalid/npm_lower",
    ),
    ("NPM_CONFIG_USERCONFIG", "/hostile9069/npmrc_upper"),
    ("HTTPS_PROXY", "http://hostile9069.invalid:1/https_upper"),
    ("https_proxy", "http://hostile9069.invalid:2/https_lower"),
    ("NO_PROXY", "hostile9069-no-proxy-upper"),
    ("SSL_CERT_FILE", "/hostile9069/ssl_cert_file.pem"),
    ("SSL_CERT_DIR", "/hostile9069/ssl_cert_dir"),
];

/// Why: V4, D7 — a checkout's `.vercel/project.json` and inherited
/// `VERCEL_*` variables must not choose the project, team or token of a
/// push. Inherited variables exist only from a process's birth, so this
/// re-runs the test binary on [`vercel_hostile_env_child`] with hostile
/// `VERCEL_*` set and its working directory a checkout holding a hostile
/// `.vercel/project.json`, and checks that the child passed. The removal
/// list itself is checked here.
/// Test: itself.
#[test]
fn vercel_child_runs_in_an_empty_dir_with_mapping_ids_and_no_inherited_vercel_env() {
    let inherited = [
        "HOME",
        "VERCEL_ENV",
        "PATH",
        "VERCEL_DEBUG",
        "NOT_VERCEL_X",
        "NODE_PATH",
        "NODE_TLS_REJECT_UNAUTHORIZED",
        "NOW_REGION",
        "npm_config_cache",
        "NPM_CONFIG_PREFIX",
        "SSL_CERT_FILE",
        "MY_NODE_X",
        "npm_package_name",
        "SSL_CERT_X",
    ]
    .map(OsString::from);
    let removed = steering_vars(inherited);
    for name in [
        "VERCEL_ORG_ID",
        "VERCEL_PROJECT_ID",
        "VERCEL_TOKEN",
        "VERCEL_SCOPE",
        "VERCEL_ENV",
        "VERCEL_DEBUG",
        "NODE_PATH",
        "NODE_TLS_REJECT_UNAUTHORIZED",
        "NOW_REGION",
        "npm_config_cache",
        "NPM_CONFIG_PREFIX",
        "SSL_CERT_FILE",
        // Named without being inherited: removed from every run (D4).
        "SSL_CERT_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ] {
        assert!(removed.contains(&OsString::from(name)), "{name}");
    }
    for kept in [
        "HOME",
        "PATH",
        "NOT_VERCEL_X",
        "MY_NODE_X",
        "npm_package_name",
        "SSL_CERT_X",
    ] {
        assert!(!removed.contains(&OsString::from(kept)), "{kept}");
    }

    let checkout = TempDir::new().unwrap();
    std::fs::create_dir(checkout.path().join(".vercel")).unwrap();
    std::fs::write(
        checkout.path().join(".vercel").join("project.json"),
        r#"{"orgId":"team_file_hostile9069","projectId":"prj_file_hostile9069"}"#,
    )
    .unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "vercel_hostile_env_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .current_dir(checkout.path())
        .env(HOSTILE_CHILD, "1");
    for (name, value) in HOSTILE {
        child.env(name, value);
    }
    let out = child.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "child failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("1 passed"), "child did not run:\n{stdout}");
}

/// The body of the re-run above; a no-op outside that child.
/// Test: `vercel_child_runs_in_an_empty_dir_with_mapping_ids_and_no_inherited_vercel_env`.
#[test]
fn vercel_hostile_env_child() {
    if std::env::var_os(HOSTILE_CHILD).is_none() {
        return;
    }
    // The plant is live: the variables are inherited, the file is in reach.
    for (name, value) in HOSTILE {
        assert_eq!(std::env::var(name).as_deref(), Ok(value), "{name}");
    }
    let checkout = std::env::current_dir().unwrap();
    assert!(checkout.join(".vercel").join("project.json").is_file());

    let shim = VercelShim::new();
    push(&shim.backend(), "API_KEY").unwrap();

    let env = shim.env_log();
    for (_, value) in HOSTILE {
        assert!(!env.contains(value), "{value} reached vercel:\n{env}");
    }
    for line in env.lines().filter(|l| l.starts_with("VERCEL_")) {
        let mapped = [
            format!("VERCEL_ORG_ID={ORG}"),
            format!("VERCEL_PROJECT_ID={PROJECT}"),
        ];
        assert!(mapped.iter().any(|m| m == line), "{line}");
    }
    let links = shim.link_log();
    let want = format!("env:{ORG}/{PROJECT}");
    assert_eq!(links.lines().collect::<Vec<_>>(), [want.as_str(); 6]);
    assert!(!links.contains("hostile"), "{links}");
    assert_eq!(
        shim.pwd_log().lines().collect::<Vec<_>>(),
        ["has-link"; 6],
        "the checkout's file was in the child's working directory"
    );
    let root = shim.work_root();
    for line in shim.cwd_log().lines() {
        let mut fields = line.splitn(3, ' ');
        let (mode, entries, dir) = (fields.next(), fields.next(), fields.next());
        assert_eq!((mode, entries), (Some("drwx------"), Some("0")), "{line}");
        let dir = PathBuf::from(dir.unwrap());
        assert!(
            dir.starts_with(&root) && !dir.starts_with(&checkout),
            "{line}"
        );
        assert!(!dir.exists(), "the work dir outlived the push: {line}");
    }
    assert_eq!(shim.cwd_log().lines().count(), 6);
}

/// Why: the #9927 hold — `vercel env ls` prints the header `name value type
/// environments created`, with a `type` column the parser once refused, and
/// an empty target prints its notice on stderr only (stdout stays empty).
/// What: a synthetic listing in the real column layout (names, project and
/// team are invented; the CLI lists names and never values), plus the empty
/// and wrapped-environments shapes.
/// Test: itself.
#[test]
fn vercel_listing_parser_real_cli_table() {
    let listing = "\
 name                 value               type          environments                     created\n \
 API_TOKEN            Encrypted           encrypted     Production                       3d ago\n \
 DATABASE_URL         Encrypted           sensitive     Production, Preview              12d ago\n \
 FEATURE_FLAG_X       Encrypted           encrypted     Production, Preview, Development 2mo ago\n \
 _PRIVATE_NAME        Encrypted           encrypted     Preview (feature/demo-branch)    1y ago\n";
    let names = listing::names(listing).unwrap();
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        [
            "API_TOKEN",
            "DATABASE_URL",
            "FEATURE_FLAG_X",
            "_PRIVATE_NAME"
        ]
    );
    // The CLI's own banner lines precede the table when both streams merge.
    let banner = format!(
        "Vercel CLI 99.0.0\n> Environment Variables found for acme-demo/web-demo [412ms]\n\n{listing}"
    );
    assert_eq!(listing::names(&banner).unwrap().len(), 4);
    // The header alone is an empty table.
    assert_eq!(
        listing::names(" name value type environments created\n"),
        Some(Default::default())
    );
    // An empty target: stdout is empty and the notice is on stderr, so the
    // parser sees nothing it can call a listing and fails closed.
    assert_eq!(listing::names(""), None);
    assert_eq!(
        listing::names("> No Environment Variables found for acme-demo/web-demo [9ms]\n"),
        Some(Default::default())
    );
}

/// Why: V6 — the parser recognises a table and the empty notice, and
/// anything else is `None`, never a listing.
/// Test: itself.
#[test]
fn vercel_listing_parser_table() {
    let table = "Vercel CLI 99.0.0\n> Environment Variables found for acme/web\n\n \
                 name  value  environments  created\n API_KEY  Encrypted  Production  1d ago\n \
                 OTHER  Encrypted  Production  2d ago\n";
    let names = listing::names(table).unwrap();
    assert_eq!(names.into_iter().collect::<Vec<_>>(), ["API_KEY", "OTHER"]);
    assert_eq!(
        listing::names(" NAME VALUE ENVIRONMENTS CREATED\n"),
        Some(Default::default())
    );
    assert_eq!(
        listing::names("> No Environment Variables found for acme/web [9ms]\n"),
        Some(Default::default())
    );
    for unparseable in [
        "",
        "\n\n",
        "Upgrade available\n",
        "API_KEY Encrypted Production\n",
        " name value environments\n API_KEY ok\n --- footer ---\n",
        " name value environments\n 1BAD x\n",
    ] {
        assert_eq!(listing::names(unparseable), None, "{unparseable:?}");
    }
}

/// A machine config file under `dir` holding `yaml`.
fn machine_file(dir: &TempDir, yaml: &str) -> PathBuf {
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

/// The `secrets.vercel:` section pinning `program`, with `body` after it.
fn section(program: &std::path::Path, body: &str) -> String {
    format!(
        "secrets:\n  vercel:\n    program: {}\n{body}",
        program.display()
    )
}

/// The `projects:` block mapping `key` to the shim's ids.
fn mapping(key: &str) -> String {
    format!("    projects:\n      {key}:\n        org_id: {ORG}\n        project_id: {PROJECT}\n")
}

/// Why: V1, D6, D14 — the project comes only from the machine config's
/// mapping for the checkout's `<owner>/<repo>`; no section, a
/// `default_backend` alone, or a mapping for another repository is
/// `backend_not_enabled` before anything is spawned.
/// Test: itself.
#[test]
fn vercel_without_a_mapping_is_refused_before_any_spawn() {
    let shim = VercelShim::new();
    let tmp = TempDir::new().unwrap();
    let id = identity("acme", "web");
    let root = shim.work_root();
    let not_enabled = |yaml: &str| {
        let path = machine_file(&tmp, yaml);
        matches!(
            open_in(&path, &id, &root),
            Err(SecretsError::BackendNotEnabled { .. })
        )
    };
    assert!(not_enabled(""));
    assert!(not_enabled("secrets:\n  default_backend: vercel\n"));
    assert!(not_enabled(&section(shim.program(), "")));
    assert!(not_enabled(&section(
        shim.program(),
        &mapping("acme/other")
    )));
    assert!(not_enabled(&section(shim.program(), &mapping("other/web"))));
    let missing = tmp.path().join("absent.yaml");
    assert!(matches!(
        open_in(&missing, &id, &root),
        Err(SecretsError::BackendNotEnabled { .. })
    ));
    // An unmapped checkout is refused before the program pin is read.
    assert!(not_enabled(
        "secrets:\n  vercel:\n    program: relative/vercel\n"
    ));

    let path = machine_file(&tmp, &section(shim.program(), &mapping("Acme/Web")));
    let backend = open_in(&path, &id, &root).unwrap();
    assert_eq!(backend.id(), BackendId::vercel());
    assert_eq!(shim.calls(), "", "opening spawns nothing");
    backend
        .set(&web(), &key("API_KEY"), &SecretValue::new(VALUE))
        .unwrap();
    let env = shim.env_log();
    assert!(
        env.contains(&format!("VERCEL_PROJECT_ID={PROJECT}\n")),
        "{env}"
    );
}

/// Why: §3.3, ruling 74, D7 — `vercel` runs only from a machine pin, never
/// `PATH`; the pin is absolute and executable, `config_path` is an absolute
/// directory, `account` is refused, and mapped ids are plain. Each refusal
/// comes before any spawn.
/// Test: itself.
#[test]
fn vercel_unpinned_program_is_cli_not_installed() {
    let shim = VercelShim::new();
    let tmp = TempDir::new().unwrap();
    let id = identity("acme", "web");
    let open = |yaml: String| {
        let path = machine_file(&tmp, &yaml);
        open_in(&path, &id, &shim.work_root())
    };
    let unpinned = open(format!("secrets:\n  vercel:\n{}", mapping("acme/web")));
    assert!(
        matches!(unpinned, Err(SecretsError::CliNotInstalled { .. })),
        "{unpinned:?}"
    );
    let pinned = |body: &str| {
        open(section(
            shim.program(),
            &format!("{}{body}", mapping("acme/web")),
        ))
    };
    for (body, what) in [
        ("    account: someone\n", "account"),
        ("    config_path: vcfg\n", "relative config_path"),
    ] {
        let refused = pinned(body);
        assert!(
            matches!(refused, Err(SecretsError::Config { .. })),
            "{what}: {refused:?}"
        );
    }
    let relative = open(format!(
        "secrets:\n  vercel:\n    program: bin/vercel\n{}",
        mapping("acme/web")
    ));
    assert!(
        matches!(relative, Err(SecretsError::Config { .. })),
        "{relative:?}"
    );

    let plain = tmp.path().join("not-executable");
    std::fs::write(&plain, "").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let not_exec = open(section(&plain, &mapping("acme/web")));
    assert!(
        matches!(not_exec, Err(SecretsError::CliNotInstalled { .. })),
        "{not_exec:?}"
    );
    let bad_ids = open(section(
        shim.program(),
        "    projects:\n      acme/web:\n        org_id: \"team x\"\n        project_id: prj_1\n",
    ));
    assert!(
        matches!(bad_ids, Err(SecretsError::Config { .. })),
        "{bad_ids:?}"
    );
    assert_eq!(shim.calls(), "", "nothing spawned");
}
