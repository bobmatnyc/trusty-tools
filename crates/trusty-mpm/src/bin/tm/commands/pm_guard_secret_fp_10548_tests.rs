//! #10548: the credential rules' kept false-positive fixes (P4, P7), the
//! bypass rows two review rounds found against the dropped narrowings, and
//! the incident log's true positives and fail-closed cases. Every fixture is
//! a fake placeholder; nothing is executed.
//!
//! Every bypass row refuses on 5f0290d4d7 and must keep refusing.

use super::*;

use std::os::unix::fs::PermissionsExt;

use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;

/// An AWS credentials read — refused inline (#7266).
const AWS_READ: &str = "cat ~/.aws/credentials\n";

/// A literal `gh api` DELETE of a repository secret.
const DELETE_CALL: &str = "gh api -X DELETE repos/o/r/actions/secrets/K";

/// Write `body` to `dir/name`, executable when `exec`, and return the path.
fn file(dir: &Path, name: &str, body: &str, exec: bool) -> PathBuf {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write");
    let mode = if exec { 0o755 } else { 0o644 };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
}

/// The whole credential verdict for a Bash `command` run from `cwd`: the
/// inline rules (#7266, #8596, #8875, the nested rules) and the script-body
/// rule (#8879), as `pm_guard_architect_envfile` composes them.
fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd))
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
}

/// Every row of `commands` refuses from `cwd`; the allowed ones are reported
/// together.
fn all_refuse(commands: &[String], cwd: &Path) {
    let allowed: Vec<&String> = commands
        .iter()
        .filter(|c| verdict(c, cwd).is_none())
        .collect();
    assert!(allowed.is_empty(), "must refuse: {allowed:#?}");
}

/// The script bodies of the bypass table, each run as `bash <name>`.
const BYPASS_BODIES: &[(&str, &str)] = &[
    // Round 1, bypass 1: `case` and `in` as arguments open no arm list.
    ("case1.sh", "(echo case in\n./steal.sh | cat)\n"),
    ("case2.sh", "(echo case x in\n./steal.sh | cat)\n"),
    // Delta review: the same after a keyword, an escaped `;`, `time`, `{`.
    ("case3.sh", "(echo then case x in\n./steal.sh | cat)\n"),
    ("case4.sh", "(echo do case x in\n./steal.sh | cat)\n"),
    ("case5.sh", "(echo a\\; case x in\n./steal.sh | cat)\n"),
    ("case6.sh", "(time echo case x in\n./steal.sh | cat)\n"),
    ("case7.sh", "{ echo case x in\n./steal.sh | cat; }\n"),
    // Round 1, bypass 2: a `#` line inside an open quote runs.
    ("quote1.sh", "echo \"a\n#\"; ./steal.sh\n"),
    ("quote3.sh", "echo $'a\\n#'; ./steal.sh\n"),
    // Delta review: a continued line makes the `#` line code.
    ("cont1.sh", "true\\\n#;./gone.sh\n"),
    // Delta review: `${x#'}` holds no quote; a missing script and a FIFO.
    ("param1.sh", "echo \"${x#'}\"\n./gone.sh\n"),
    ("param2.sh", "echo \"${x#'}\"\n./fifo\n"),
    // Round 1, bypass 3: any spelling that makes a file executable.
    ("chmod1.sh", "ch''mod +x ./nx.sh; ./nx.sh\n"),
    ("chmod2.sh", "$(printf chm)od +x ./nx.sh; ./nx.sh\n"),
    ("chmod3.sh", "install -m 755 ./nx.sh ./nx.sh; ./nx.sh\n"),
];

/// The inline commands of the bypass table: a listed program redefined, a
/// rewritten pair in command position, or a pair behind a running wrapper.
fn bypass_commands() -> Vec<String> {
    let method = "\"$G\" api --method DELETE repos/o/r/actions/secrets/K";
    let pair = "\"$G\" \"$A\" -X DELETE repos/o/r/actions/secrets/K";
    vec![
        format!("hash -p /usr/bin/env cp; cp {DELETE_CALL}"),
        format!("hash -p /usr/bin/env cp; G=gh; cp {method}"),
        format!("cp {method}"),
        format!("PATH=./shim:$PATH cp {DELETE_CALL}"),
        format!("source defs.sh; cp {DELETE_CALL}"),
        format!(". defs.sh; cp {DELETE_CALL}"),
        format!("eval \"c\"\"p(){{ env \\\"\\$@\\\"; }}\"; cp {DELETE_CALL}"),
        format!("BASH_CMDS[cp]=/usr/bin/env; cp {DELETE_CALL}"),
        format!("commands[cp]=/usr/bin/env; cp {DELETE_CALL}"),
        format!("export PA''TH=./shim:/usr/bin:/bin; cp {DELETE_CALL}"),
        format!("alias cp=env; cp {DELETE_CALL}"),
        format!("cp() {{ \"$@\"; }}; cp {pair}"),
        format!("cargo() {{ \"$@\"; }}; cargo {pair}"),
        format!("for c in a b; do cp {pair}; done"),
        pair.to_string(),
        format!("xargs {pair}"),
        format!("do {pair}"),
        format!("\"$C\" test {pair}"),
        format!("cargo run -- {pair}"),
        format!("cargo test -p \"$c\" {pair}"),
        format!("rsync -a {pair} --delete"),
        "echo $'a\\n#'; cat ~/.aws/credentials".to_string(),
    ]
}

/// 🔴 REGRESSION (#10548): every bypass two review rounds found against the
/// dropped `case`, comment and operand narrowings refuses, as on 5f0290d4d7.
#[test]
fn every_review_bypass_row_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(dir.path(), "steal.sh", AWS_READ, true);
    file(dir.path(), "nx.sh", AWS_READ, false);
    file(dir.path(), "defs.sh", "true\n", true);
    let fifo = std::process::Command::new("mkfifo")
        .arg(dir.path().join("fifo"))
        .status()
        .expect("mkfifo");
    assert!(fifo.success(), "mkfifo");
    let mut rows: Vec<String> = BYPASS_BODIES
        .iter()
        .map(|(name, body)| {
            file(dir.path(), name, body, true);
            format!("bash {name}")
        })
        .collect();
    rows.extend(bypass_commands());
    all_refuse(&rows, dir.path());
}

/// 🔴 REGRESSION (#10548 P7): one plain command whose two adjacent
/// expansions are operands of a program that never runs them, followed only
/// by long flags, is no `gh api` call.
#[test]
fn a_single_segment_operand_pair_is_no_gh_api_call_10548() {
    let dir = tempfile::tempdir().expect("dir");
    for command in [
        "cargo test -p trusty-secrets -F \"$f\" \"$t\" --no-fail-fast",
        "rsync -a \"$SRC\" \"$DST\" --delete",
    ] {
        assert_eq!(verdict(command, dir.path()), None, "`{command}` must allow");
    }
}

/// 🔴 REGRESSION (#10548 P4): a nested refusal names the word it could not
/// resolve, not the outer script, which exists.
#[test]
fn a_nested_refusal_names_the_inner_word_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "outer.sh",
        "echo hi\nbash ./never-written.sh\n",
        true,
    );
    let reason = verdict("bash outer.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("does not exist"), "must deny: {reason}");
    assert!(
        reason.contains("`./never-written.sh`"),
        "names the inner word: {reason}"
    );
}

/// 🔴 TRUE POSITIVE (#10548 incident 1, api-qa): a curl+psql probe script
/// whose database password comes from the Keychain refuses.
#[test]
fn tp_a_curl_psql_probe_script_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "api_probe.sh",
        "#!/usr/bin/env bash\nset -eu\ncurl -s http://localhost:8080/health\n\
         security find-generic-password -s api-qa-db -w\n\
         psql -h localhost -U qa -c 'select 1'\n",
        true,
    );
    let reason = verdict("bash api_probe.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("#8596"), "must refuse: {reason}");
}

/// 🔴 TRUE POSITIVE (#10548 incident 2): an in-process `bootstrap_ssm` script
/// that builds its SSM client from the AWS credentials file refuses.
#[test]
fn tp_an_in_process_bootstrap_ssm_script_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "bootstrap_ssm.py",
        "import configparser, os, boto3\ncfg = configparser.ConfigParser()\n\
         cfg.read(os.path.expanduser('~/.aws/credentials'))\n\
         ssm = boto3.client('ssm', aws_access_key_id=cfg['default']['aws_access_key_id'])\n\
         for p in ssm.get_parameters_by_path(Path='/app/', WithDecryption=True)['Parameters']:\n\
         \x20   os.environ[p['Name']] = p['Value']\n",
        false,
    );
    let reason = verdict("python3 bootstrap_ssm.py", dir.path()).unwrap_or_default();
    assert!(reason.contains("#7266"), "must refuse: {reason}");
}

/// 🔴 TRUE POSITIVE (#10548 incident 3): a script carrying
/// `GH_TOKEN=$(gh auth token)` refuses.
#[test]
fn tp_a_gh_auth_token_script_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "release.sh",
        "#!/usr/bin/env bash\nGH_TOKEN=$(gh auth token); echo \"using ${GH_TOKEN:0:8}\"\n\
         gh release list -R o/r\n",
        true,
    );
    let reason = verdict("./release.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("#8596"), "must refuse: {reason}");
}

/// 🔴 TRUE POSITIVE (#10548 incident 4): the `dev-scratch` script that loads
/// `.env.local` refuses. `make` itself is residual 4, so the script is run.
#[test]
fn tp_a_scratch_db_script_reading_env_local_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    file(
        dir.path(),
        "tools/db-scratch.sh",
        "#!/usr/bin/env bash\nset -a\nsource .env.local\nset +a\n\
         psql \"$DATABASE_URL\" -c 'create database scratch'\n",
        true,
    );
    let reason = verdict("bash tools/db-scratch.sh", dir.path()).unwrap_or_default();
    assert!(reason.contains("#7266"), "must refuse: {reason}");
}

/// 🔴 FAIL CLOSED (#10548 incident 5): a script created and run in one
/// command, at a path computed at run time, refuses: the guard cannot read a
/// body that does not exist yet. (A literal name written by an earlier stage
/// is documented residual 1.)
#[test]
fn fc_a_script_written_and_run_in_one_command_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    let command = "S=$(mktemp) && printf 'echo hi\\n' > \"$S\" && bash \"$S\"";
    let reason = verdict(command, dir.path()).unwrap_or_default();
    assert!(
        reason.contains("computed at run time"),
        "must refuse: {reason}"
    );
}

/// 🔴 FAIL CLOSED (#10548 incident 6): `bash $S/ssm_run.sh` cannot be
/// resolved, so it refuses.
#[test]
fn fc_an_unresolvable_script_path_refuses_10548() {
    let dir = tempfile::tempdir().expect("dir");
    let reason = verdict("bash $S/ssm_run.sh", dir.path()).unwrap_or_default();
    assert!(
        reason.contains("computed at run time"),
        "must refuse: {reason}"
    );
}
