//! #10570, #10598, #10548 P2: a Python body is read as Python and a shell
//! comment is not a run, with every real credential read still refused. Every
//! fixture is a fake placeholder; nothing is executed.

use super::comments::blank_run_scan_comments;
use super::python::{Reading, python_reading, read_python};
use super::*;

use crate::commands::pm_guard_secret_read::{
    Scan, evaluate_secret_file_read_command, evaluate_secret_file_read_in, names_a_secret_file,
};

/// The whole credential verdict for a Bash `command` run from `cwd`, as
/// `pm_guard_architect_envfile` composes it.
fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd))
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
}

/// The verdict for `body` run as `python3 s.py` from a fresh directory.
fn python_verdict(body: &str) -> Option<String> {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("s.py"), body).expect("write");
    verdict("python3 s.py", dir.path())
}

/// 🔴 REGRESSION (#10598, #10570): each Python shape the shell judges refused
/// on origin/main allows: `ps = …`, `refs.key`, `token.lower()`, a deny list,
/// `op` beside prose and `.read()`, a tally name, a docstring.
#[test]
fn a_python_body_is_read_as_python_10598() {
    let rows = [
        "panes = {}\nps = panes.setdefault(\"k\", {})\nps[\"idle_since\"] = None\n",
        "def f(refs):\n    return refs.key\n",
        "def f(token):\n    return token.lower()\n",
        "DENY = (\"*secret*\", \"*credential*\", \"*.env*\", \"*.pem\", \"*.key\", \"*id_rsa*\")\n\
         import fnmatch\ndef ok(n):\n    return not any(fnmatch.fnmatch(n, p) for p in DENY)\n",
        "import sys\ndef check(op, arg):\n    if op == \"update\":\n        \
         raise ValueError(f\"cannot read {arg}\")\nprint(sys.stdin.read())\n",
        "\"\"\"Record a batch op; the log verb can read it back.\"\"\"\nprint(1)\n",
        "# op read is what a comment may say; so may `cat .env`\nprint(1)\n",
        "import os\nT = os.path.join(\"local\", \"usage\", \"token-hourly.jsonl\")\nprint(T)\n",
        "\"\"\"Doc.\"\"\"\nimport argparse\np = argparse.ArgumentParser(description=__doc__)\n",
    ];
    let refused: Vec<String> = rows
        .iter()
        .filter_map(|b| python_verdict(b).map(|why| format!("{b}\n => {why}")))
        .collect();
    assert!(refused.is_empty(), "refused:\n{}", refused.join("\n"));
}

/// 🔴 #10570's acceptance: a real credential read in a Python body refuses.
#[test]
fn a_real_credential_read_in_python_still_refuses_10598() {
    let rows = [
        "import subprocess\nsubprocess.run([\"security\", \"find-generic-password\", \"-s\", \"x\", \"-w\"])\n",
        "import subprocess\nsubprocess.run([\"op\", \"read\", \"op://fake/fake/fake\"])\n",
        "import subprocess\nsubprocess.run(\"op read op://fake/fake/fake\", shell=True)\n",
        "print(open(\".env\").read())\n",
        "print(open(\"token.json\").read())\n",
        "import os\nprint(open(os.path.expanduser(\"~/.aws/credentials\")).read())\n",
        "import os\nprint(open(os.path.join(os.environ[\"HOME\"], \".aws\", \"credentials\")).read())\n",
        "from pathlib import Path\nprint((Path.home() / \".aws\" / \"credentials\").read_text())\n",
        "HOME = \"x\"\nprint(open(f\"{HOME}/.aws/credentials\").read())\n",
        "import glob\nprint(glob.glob(\"*.env*\"))\n",
        "import glob\nP = \"*.env*\"\nprint(glob.glob(P))\n",
        "import os\nos.system(\"cat ~/.ssh/id_rsa\")\n",
        "import subprocess\nsubprocess.run([\"gh\", \"auth\", \"token\"])\n",
        "import subprocess\nsubprocess.run([\"security\", \"-i\"])\n",
        "import subprocess\nsubprocess.run([\"ps\", \"eww\", \"1\"])\n",
    ];
    let allowed: Vec<&&str> = rows
        .iter()
        .filter(|b| python_verdict(b).is_none())
        .collect();
    assert!(allowed.is_empty(), "must refuse: {allowed:#?}");
}

/// 🔴 The bypass rows the critic raised against held head 4b75e949f6 (#10570
/// hold comment, 02:20Z): a subcommand mid-literal, a run-time CLI name, and
/// `sh -c` around a here-document, which keeps the shell judges.
#[test]
fn the_10570_hold_probes_still_refuse_10598() {
    let which = "import os, shutil, string\nOP_BIN = shutil.which(\"op\")\nR = \"x\"\n";
    let rows = [
        "os.system(OP_BIN + \" read op://fake/fake/fake\")\n",
        "os.system(OP_BIN + \" --account work read \" + R)\n",
        "os.system(\"%(b)s read %(r)s\" % {\"b\": OP_BIN, \"r\": R})\n",
        "os.system(string.Template(\"$b read $r\").substitute(b=OP_BIN, r=R))\n",
        "os.system(f\"{OP_BIN} read {R}\")\n",
        "os.system(\"{} read {}\".format(OP_BIN, R))\n",
        "os.system(\"%s read %s\" % (OP_BIN, R))\n",
        "os.system(\"op \" + \"read \" + R)\n",
        "os.system(\"op {}\".format(R))\n",
    ];
    let allowed: Vec<String> = rows
        .iter()
        .map(|row| format!("{which}{row}"))
        .filter(|b| python_verdict(b).is_none())
        .collect();
    assert!(allowed.is_empty(), "must refuse: {allowed:#?}");

    let dir = tempfile::tempdir().expect("dir");
    for body in [
        "import os, shutil\nOP_BIN = shutil.which(\"op\")\nos.system(f\"{OP_BIN} read {R}\")\n",
        "import os, shutil\nOP_BIN = shutil.which(\"op\")\nos.system(\"{} read {}\".format(OP_BIN, R))\n",
    ] {
        let command = format!("sh -c 'python3 - <<EOF\n{}\nEOF'", body.replace('\'', ""));
        assert!(
            verdict(&command, dir.path()).is_some(),
            "must refuse: {command}"
        );
    }
}

/// 🔴 FAIL CLOSED (#10598): each shape the reader cannot vouch for is not
/// clean, so the shell judges decide. A nested f-string can hide a `#`, `\r`
/// ends a comment, `__doc__` can carry a call, a cookie can respell a quote.
#[test]
fn a_body_the_reader_cannot_read_is_not_clean_10598() {
    for body in [
        "x = \"open\n",
        "x = \"\"\"open\n",
        "f\"{\"#\"}\"; print(1)\n",
        "# -*- coding: utf-7 -*-\nprint(1)\n",
        "\"\"\"doc\"\"\"\nimport os\nos.system(__doc__)\n",
        "import inspect\nprint(inspect.getsource(print))\n",
        "exec(open(__file__).read())\n",
    ] {
        assert!(read_python(body).is_none(), "must not read: {body}");
        assert!(
            matches!(python_reading(body), Reading::Unread),
            "must go to the shell judges raw: {body}"
        );
    }
    // Each hides a real read from a reader that loses track, and refuses.
    for body in [
        "import os\nf\"{\"#\"}\"; os.system(\"op read op://fake/fake/fake\")\n",
        "import os\n# c\ros.system(\"op read op://fake/fake/fake\")\n",
        "\"\"\"op read op://fake/fake/fake\"\"\"\nimport os\nos.system(__doc__)\n",
        "\"\"\"cat .env\"\"\"\nimport os\nos.system(__doc__)\n",
        "# cat .env\nexec(open(__file__).read().split(\"#\")[1])\n",
    ] {
        assert!(python_verdict(body).is_some(), "must refuse: {body}");
    }
}

/// 🔴 #10570 criterion 3: an allow comes from the body, never the name. Every
/// seeded name with hostile content refuses, by interpreter and by path.
#[test]
fn a_seeded_name_with_hostile_content_refuses_10570() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::create_dir_all(dir.path().join("scripts")).expect("mkdir");
    let py = "#!/usr/bin/env python3\nprint(open(\".env\").read())\n";
    let sh = "#!/bin/sh\ncat .env\n";
    for (name, body) in [
        ("dashboard-db.py", py),
        ("datg.py", py),
        ("dashboard-serve.py", py),
        ("fleet-poll.py", py),
        ("pm-notify.sh", sh),
        ("self-clear.sh", sh),
    ] {
        let path = dir.path().join("scripts").join(name);
        std::fs::write(&path, body).expect("write");
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        let runner = if name.ends_with(".sh") {
            "bash"
        } else {
            "python3"
        };
        for command in [
            format!("{runner} scripts/{name}"),
            format!("./scripts/{name}"),
        ] {
            assert!(
                verdict(&command, dir.path()).is_some(),
                "must refuse: {command}"
            );
        }
    }
}

/// 🔴 #10570: a padded body stays judged, the read it hides refuses.
#[test]
fn a_padded_python_body_still_refuses_10598() {
    let body = format!(
        "PAD = ({})\nprint(open(\".env\").read())\n",
        "\"op\" ".repeat(30_000)
    );
    assert!(python_verdict(&body).is_some(), "the .env read refuses");
}

/// 🔴 REGRESSION (#10598): `pm-notify.sh`'s comment shape is not a run; on
/// origin/main it refused with "does not exist". A real run after it refuses.
#[test]
fn a_comment_cannot_start_a_nested_run_10598() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(
        dir.path().join("ok.sh"),
        "#!/bin/sh\n# head, blocker; ack/progress/fyi wait for the digest.\necho hi # trailing; ./gone.sh\n",
    )
    .expect("write");
    assert_eq!(verdict("bash ok.sh", dir.path()), None);
    std::fs::write(
        dir.path().join("bad.sh"),
        "#!/bin/sh\n# head; ack/progress/fyi wait.\n./gone.sh\n",
    )
    .expect("write");
    assert!(
        verdict("bash bad.sh", dir.path()).is_some(),
        "a real run refuses"
    );
}

/// 🔴 REGRESSION (#10548 P2): an apostrophe in a comment no longer unbalances
/// the scan, so a `|` in a quoted expression stays quoted; on origin/main this
/// refused as "not a regular file".
#[test]
fn a_comment_cannot_unbalance_the_run_scan_10548() {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(
        dir.path().join("check.sh"),
        "#!/usr/bin/env bash\n# don't split here\nsed 's/a|/|b/' notes.md\n",
    )
    .expect("write");
    assert_eq!(verdict("bash check.sh", dir.path()), None);
}

/// 🔴 #10598: the walker blanks only real comments. A `#` in a quote, after a
/// continuation or in a here-document stays; a shape it cannot follow answers
/// the raw body. The here-document row is the desync a comment-blind pass gets.
#[test]
fn blank_run_scan_comments_keeps_every_code_hash_10598() {
    for raw in [
        "echo \"a\n#\"; ./steal.sh\n",
        "echo $'a\\n#'; ./steal.sh\n",
        "true\\\n#;./gone.sh\n",
        "echo \"${x#'}\"\n./gone.sh\n",
        "cat <<EOF\nit's\nEOF\necho 'a\n#'; ./steal.sh\n",
        "echo \"$(echo \" # x\"); ./steal.sh\"\n",
        "x=$(a # b\n)\n./steal.sh\n",
        "echo 'open\n# c\n",
        "cat <<$D\n# c\n$D\n# d\n",
        "echo `a # b`\n# c\n",
    ] {
        assert_eq!(blank_run_scan_comments(raw), raw, "must stay raw");
    }
    assert_eq!(
        blank_run_scan_comments("# a; b\necho hi # c\n"),
        "      \necho hi    \n"
    );
    assert_eq!(
        blank_run_scan_comments("cat <<'E'\n# kept\nE\n# gone\n"),
        "cat <<'E'\n# kept\nE\n      \n"
    );
}

/// 🔴 REGRESSION (#10570 review): a `token` prefix never cancels another secret
/// family, and a tally needs a tally word and a log extension.
#[test]
fn a_token_tally_exempts_only_the_token_family_10570() {
    let wrong: Vec<String> = [
        ("cat token-credentials.jsonl", true),
        ("cat token_secrets.csv", true),
        ("cat token-db.tfstate.log", true),
        ("cat /etc/secrets/token-api.ndjson", true),
        ("cat tokens-export.csv", true),
        ("cat token-hourly-credentials.jsonl", true),
        ("cat token.json", true),
        ("cat local/usage/token-hourly.jsonl", false),
    ]
    .iter()
    .filter(|(c, deny)| evaluate_secret_file_read_command(c).is_some() != *deny)
    .map(|(c, deny)| format!("{c}: want deny={deny}"))
    .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
    assert!(!names_a_secret_file(
        "token-hourly.jsonl",
        Scan::ProgramText
    ));
}
