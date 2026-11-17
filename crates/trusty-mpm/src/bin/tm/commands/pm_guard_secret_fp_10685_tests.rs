//! #10685: literal text a `printf`/`echo`-only command prints is not a file
//! read, while every control that really opens a dummy credential path, and
//! every near miss that can hand printed text to a reader, keeps refusing.
//! Every fixture is a fake placeholder; nothing is executed.

use super::*;

use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;

/// The whole credential verdict for a Bash `command` run from `cwd`, composed
/// as `pm_guard_secret_fp_10548_tests::verdict` composes it.
fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd))
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
}

/// Write `body` to `dir/name`, creating parent directories.
fn file(dir: &Path, name: &str, body: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(&path, body).expect("write");
}

/// `command` is allowed from a fresh temporary directory.
fn assert_allows(command: &str) {
    let dir = tempfile::tempdir().expect("tempdir");
    let got = verdict(command, dir.path());
    assert!(got.is_none(), "{command:?} must allow, got {got:?}");
}

/// `command` refuses from `cwd`, and the reason names `needle`.
fn assert_refuses_naming(command: &str, cwd: &Path, needle: &str) {
    let got = verdict(command, cwd).unwrap_or_else(|| panic!("{command:?} must refuse"));
    assert!(
        got.contains(needle),
        "{command:?}: {got:?} must name {needle:?}"
    );
}

/// The before-state probe: prints each fixture's verdict, asserts nothing.
#[test]
fn probe_before_state_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path();
    file(
        cwd,
        "scripts/example/print-literals.sh",
        "printf '%s\\n' 'process.env.TZ' '(secret|credential|token|[.]env)' '{\"tokens\": 3}'\n",
    );
    file(cwd, "dummy.sh", "cat ./dummy-project/.env\n");
    let rows = [
        "printf '%s\\n' 'process.env.TZ'",
        "printf '%s\\n' '(secret|credential|token|[.]env)'",
        "printf '%s\\n' '{\"tokens\": 3}'",
        "cat <<'EOF'\nSet the zone via process.env.TZ in the test.\n**e.** second item\nEOF",
        "bash scripts/example/print-literals.sh",
        "cat ./dummy-project/.env",
        "node -e 'require(\"fs\").readFileSync(\"./dummy-project/.env\",\"utf8\")'",
        "sed -n '1,5p' ./dummy-project/credentials.json",
        "bash dummy.sh",
        "printf '%s\\n' 'process\\.env\\.TZ'",
        "echo '**e.** second item'",
        "printf '%s\\n' 'tokens.length'",
    ];
    for (i, c) in rows.iter().enumerate() {
        eprintln!("PROBE {} => {:?}", i + 1, verdict(c, cwd));
    }
}

#[test]
fn literal_timezone_printf_allows_10685() {
    assert_allows("printf '%s\\n' 'process.env.TZ'");
}

#[test]
fn literal_regex_printf_allows_10685() {
    assert_allows("printf '%s\\n' '(secret|credential|token|[.]env)'");
}

#[test]
fn literal_json_printf_allows_10685() {
    assert_allows("printf '%s\\n' '{\"tokens\": 3}'");
}

#[test]
fn literal_markdown_heredoc_allows_10685() {
    assert_allows(
        "cat <<'EOF'\nSet the zone via process.env.TZ in the test.\n**e.** second item\nEOF",
    );
}

#[test]
fn literal_script_allows_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    file(
        dir.path(),
        "scripts/example/print-literals.sh",
        "printf '%s\\n' 'process.env.TZ' '(secret|credential|token|[.]env)' '{\"tokens\": 3}'\n",
    );
    let got = verdict("bash scripts/example/print-literals.sh", dir.path());
    assert!(got.is_none(), "must allow, got {got:?}");
}

#[test]
fn escaped_dot_timezone_printf_allows_10685() {
    assert_allows("printf '%s\\n' 'process\\.env\\.TZ'");
}

#[test]
fn emphasis_label_echo_allows_10685() {
    assert_allows("echo '**e.** second item'");
}

/// Several printer lines, inline and as a script body: no segment is `lone`.
#[test]
fn multi_line_printer_script_allows_10685() {
    let body = "printf '%s\\n' 'process\\.env\\.TZ'\n\
                printf '%s\\n' '(secret|credential|token|[.]env)'\n\
                echo '**e.** second item'\n";
    assert_allows(body);
    let dir = tempfile::tempdir().expect("tempdir");
    file(dir.path(), "scripts/example/print-many.sh", body);
    let got = verdict("bash scripts/example/print-many.sh", dir.path());
    assert!(got.is_none(), "must allow, got {got:?}");
}

#[test]
fn dotenv_path_cat_refuses_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_refuses_naming("cat ./dummy-project/.env", dir.path(), "dummy-project/.env");
}

#[test]
fn node_dotenv_path_refuses_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_refuses_naming(
        "node -e 'require(\"fs\").readFileSync(\"./dummy-project/.env\",\"utf8\")'",
        dir.path(),
        "dummy-project/.env",
    );
}

#[test]
fn credentials_path_sed_refuses_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_refuses_naming(
        "sed -n '1,5p' ./dummy-project/credentials.json",
        dir.path(),
        "credentials.json",
    );
}

#[test]
fn script_dotenv_path_refuses_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    file(dir.path(), "dummy.sh", "cat ./dummy-project/.env\n");
    assert_refuses_naming("bash dummy.sh", dir.path(), "dummy.sh");
}

/// Fail-open mutations: each row can hand printed text to a reader or a
/// shell, names a path-shaped operand, or is no closed printer text at all.
#[test]
fn near_miss_rows_still_refuse_10685() {
    let rows = [
        "printf '%s\\n' 'cat ./dummy-project/.env' > s.sh && bash s.sh",
        "exec > s.sh; printf 'cat ./dummy-project/.env\\n'; bash s.sh",
        "{ printf 'cat ./dummy-project/.env\\n'; } > s.sh; bash s.sh",
        "printf 'cat ./dummy-project/.env\\n' | sh",
        "printf 'cat ./dummy-project/.env\\n' | tee s.sh",
        "printf '%s\\n' \"$(cat ./dummy-project/.env)\"",
        "printf '%s\\n' 'x' ; cat ./dummy-project/.env",
        "printf '%s\\n' '(x|[.]env)' | xargs cat",
        "printf -v v '%s' 'process\\.env\\.TZ'",
        "env printf '%s\\n' 'process\\.env\\.TZ'",
        "printf '%s\\n' 'process\\.env\\.TZ' .env",
        "echo .env",
        // #10685 review: a path-qualified program is no printer the guard knows.
        "./echo '.env '",
        "/tmp/x/printf '.env '",
        "x/echo '.env '",
        "cat ./dummy-project/.env <<'EOF'\nhi\nEOF",
        "cat <<'EOF' | sh\ncat ./dummy-project/.env\nEOF",
        "cat <<EOF\n$(cat ./dummy-project/.env)\nEOF",
        "python3 - <<'EOF'\nopen('./dummy-project/.env')\nEOF",
        "cat <<'EOF'\n.env\nEOF",
    ];
    let dir = tempfile::tempdir().expect("tempdir");
    let allowed: Vec<&str> = rows
        .iter()
        .copied()
        .filter(|c| verdict(c, dir.path()).is_none())
        .collect();
    assert!(allowed.is_empty(), "must refuse: {allowed:#?}");
}

/// A quote the lexer cannot close keeps the raw byte scan (fail closed).
#[test]
fn unbalanced_quote_printer_refuses_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(verdict("printf '%s\\n' '(secret|[.]env", dir.path()).is_some());
}

/// A CR byte inside an otherwise inert operand withdraws the release (rule D).
#[test]
fn cr_in_printer_text_refuses_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(verdict("printf '%s\\n' 'process\\.env\\.TZ\r'", dir.path()).is_some());
}

/// A path-qualified program is not the shell's printer: `./echo` can be any
/// executable, so its operands stay screened (#10685 review, MEDIUM). Each
/// program exists and runs a harmless body, so only the file rule can refuse.
#[test]
fn path_qualified_printer_refuses_10685() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path();
    for name in ["echo", "bin/printf"] {
        file(cwd, name, "printf 'hi\\n'\n");
        let mode = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(cwd.join(name), mode).expect("chmod");
    }
    let absolute = format!("{}/bin/printf '.env '", cwd.display());
    for command in ["./echo '.env '", "bin/printf '.env '", absolute.as_str()] {
        assert!(verdict(command, cwd).is_some(), "{command:?} must refuse");
    }
}

/// A quote inside a shell comment is no quote: `# it's` hides the next line
/// from the quote scan, but the shell runs it (#10685 review, H1). The glob
/// rows use `if=.en?`: a bare `cat .en[!x]` is allowed with or without a
/// comment, because `!` cuts the word before the bracket class is read.
#[test]
fn comment_hidden_reader_refuses_10685() {
    let rows = [
        "echo x # it's\ndd if=.env\n# '",
        "echo x # it's\ndd if=.en?\n# '",
        "echo x # it\"s\ndd if=.env\n# \"",
        "echo x # it\"s\ndd if=.en?\n# \"",
        "printf 'a' # it's\ncat .env\n# '",
    ];
    let dir = tempfile::tempdir().expect("tempdir");
    file(dir.path(), ".env", "PLACEHOLDER=dummy\n");
    let allowed: Vec<&str> = rows
        .iter()
        .copied()
        .filter(|c| verdict(c, dir.path()).is_none())
        .collect();
    assert!(allowed.is_empty(), "must refuse: {allowed:#?}");
}

/// A trailing comment withdraws the printer release (conservative): a
/// secret-shaped literal is screened again and refuses, while a printer whose
/// operands name nothing secret still allows.
#[test]
fn trailing_comment_printer_release_withdrawn_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    let command = "printf '%s\\n' '(secret|[.]env)' # note";
    assert!(
        verdict(command, dir.path()).is_some(),
        "{command:?} must refuse"
    );
    assert_allows("echo 'hello world' # note");
}
