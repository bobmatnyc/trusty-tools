//! #10685 before-state probe (scratch).

use super::*;

use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;

fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd))
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
}

#[test]
fn probe_before_state_10685() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path();
    std::fs::create_dir_all(cwd.join("scripts/example")).unwrap();
    std::fs::write(
        cwd.join("scripts/example/print-literals.sh"),
        "printf '%s\\n' 'process.env.TZ' '(secret|credential|token|[.]env)' '{\"tokens\": 3}'\n",
    )
    .unwrap();
    std::fs::write(cwd.join("dummy.sh"), "cat ./dummy-project/.env\n").unwrap();
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
