//! #10548: node script bodies judged by body (#8879, #7266) — `node --check`
//! is a syntax check, and a JS member access (`x.key`) names no file. Every
//! fixture is synthetic; nothing is executed.

use super::*;

use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;

/// The whole credential verdict for `command` from `cwd`: the inline rules
/// and the script-body rule, as `pm_guard_architect_envfile` composes them.
fn verdict(command: &str, cwd: &Path) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read_in("Bash", Some(&input), Some(cwd))
        .or_else(|| evaluate_script_body_secret_read(command, cwd))
}

/// Write each `(name, body)` under a fresh directory and return it.
fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("dir");
    for (name, body) in files {
        std::fs::write(dir.path().join(name), body).expect("write");
    }
    dir
}

/// A body whose only secret-looking words are member accesses.
const MEMBERS: &str = "const x = load();\nconst cfg = x.cfg;\n\
     if (x.key && cfg.env) {\n  use(cfg.token, x.pem, obj.license, a?.key, this.#key);\n}\n";

/// The four bodies the brief lists, as synthetic stand-ins.
const BRIEF_SHAPES: &[(&str, &str)] = &[
    (
        "layer1-compare.mjs",
        "import { readFileSync } from 'node:fs';\n\
         // compare row.key totals between the old and new layer\n\
         const rows = JSON.parse(readFileSync(process.argv[2], 'utf8'));\n\
         const totals = {};\n\
         for (const row of rows) { totals[row.key] = (totals[row.key] ?? 0) + row.amount; }\n\
         console.log(`${rows.length} rows, home ${process.env.HOME}`);\n",
    ),
    (
        "compare-core.cjs",
        "'use strict';\n/* a.key and b.key are the join columns */\n\
         module.exports = {\n  compare(a, b) { return a.key === b.key && a.cents / 100 === b.cents / 100; },\n};\n",
    ),
    (
        "main.cjs",
        "const { app, BrowserWindow } = require('electron');\n\
         const lic = require('./license.json');\n\
         app.on('ready', () => {\n  const win = new BrowserWindow({ width: cfg.width });\n\
           win.loadURL(cfg.url + '?k=' + lic.key + '&t=' + session.token);\n});\n",
    ),
    (
        "adapter-routes.cjs",
        "const re = /^\\/api\\/(\\w+)$/;\n\
         router.get('/x', (req, res) => res.json({ key: req.query.key, token: req.headers.token }));\n\
         module.exports = router;\n",
    ),
];

/// `node --check` of a body naming `a.key` is a syntax check: it allows.
/// Test: this test (fails on bac374040d).
#[test]
fn node_check_of_a_member_access_body_allows_10548() {
    let dir = fixture(&[("t.mjs", "const a = { key: 1 };\nconsole.log(a.key);\n")]);
    for command in [
        "node --check t.mjs",
        "node -c t.mjs",
        "node --check ./t.mjs 2>&1",
    ] {
        assert_eq!(verdict(command, dir.path()), None, "{command}");
    }
}

/// `node --check` never runs the file, so even a secret-reading body allows.
#[test]
fn node_check_of_a_secret_reading_body_allows_10548() {
    let dir = fixture(&[("s.cjs", "require('fs').readFileSync('server.key');\n")]);
    assert_eq!(verdict("node --check s.cjs", dir.path()), None);
}

/// A node run of a body with only member accesses allows, in every extension.
#[test]
fn a_node_run_of_member_accesses_allows_10548() {
    let dir = fixture(&[("m.js", MEMBERS), ("m.mjs", MEMBERS), ("m.cjs", MEMBERS)]);
    for command in [
        "node m.js",
        "node m.mjs",
        "node m.cjs",
        "node --check m.cjs",
    ] {
        assert_eq!(verdict(command, dir.path()), None, "{command}");
    }
}

/// The brief's four refused shapes allow, run and syntax-checked.
#[test]
fn the_brief_node_shapes_allow_10548() {
    let dir = fixture(BRIEF_SHAPES);
    for (name, _) in BRIEF_SHAPES {
        for command in [format!("node {name}"), format!("node --check {name}")] {
            assert_eq!(verdict(&command, dir.path()), None, "{command}");
        }
    }
}

/// A node body naming a secret file by a literal still refuses when run.
#[test]
fn a_node_body_naming_a_secret_literal_refuses_10548() {
    let bodies = [
        "const fs = require('fs');\nfs.readFileSync(\"server.key\");\n",
        "const env = require('./.env');\n",
        "fs.readFileSync(path.join(home, '.ssh/id_rsa'));\n",
        "fs.readFileSync(`${home}/.env`, 'utf8');\n",
        "fs.readFileSync(`${cfg.dir}/server.key`);\n",
        "const re = /'/;\nfs.readFileSync(\"server.key\");\n",
        "const half = total / 2; const s = 'x';\nfs.readFileSync(\"server.key\");\n",
        // #10548 round 1: a regex after `)` is not division; the reader fails closed.
        "if (fs) /[/*]/.test('a'), console.log(fs.readFileSync(\"server.key\")); // */\n",
        "if (fs) /\"/.test(\"a\"), fs.readFileSync(\"server.key\"); // \"\n",
        "if (fs) /a\\//.test('b'); fs.readFileSync('.env')\n",
    ];
    let mut allowed = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let name = format!("r{i}.cjs");
        let dir = fixture(&[(&name, body)]);
        for command in [format!("node {name}"), format!("node ./{name}")] {
            if verdict(&command, dir.path()).is_none() {
                allowed.push(format!("{command}: {body}"));
            }
        }
    }
    assert!(allowed.is_empty(), "must refuse: {allowed:#?}");
}

/// Every form that runs the file, or loads code beside it, is still judged.
#[test]
fn a_node_run_beside_a_check_is_judged_10548() {
    let dir = fixture(&[
        ("a.cjs", "require('fs').readFileSync('server.key');\n"),
        ("clean.cjs", "console.log(1);\n"),
        ("steal.cjs", "require('fs').readFileSync('.env');\n"),
        ("x.js", "require('fs').readFileSync('server.key');\n"),
    ]);
    for command in [
        "node --check a.cjs && node a.cjs",
        "node --check a.cjs; node a.cjs",
        "node --check -r ./steal.cjs clean.cjs",
        "node --check --import ./steal.cjs clean.cjs",
        "node --check --inspect a.cjs",
        "NODE_OPTIONS='-r ./steal.cjs' node --check a.cjs",
        "env NODE_OPTIONS=-r ./x.js node --check x.js",
        "env NODE_OPTIONS='-r ./steal.cjs' node --check a.cjs",
    ] {
        assert!(verdict(command, dir.path()).is_some(), "{command}");
    }
}

/// Inline node code naming a secret file refuses (the inline rules).
#[test]
fn inline_node_code_naming_a_secret_refuses_10548() {
    let dir = fixture(&[]);
    for command in [
        "node -e \"require('fs').readFileSync('.env')\"",
        "node -p \"require('fs').readFileSync('server.key', 'utf8')\"",
        "node --check -e \"require('fs').readFileSync('.env')\"",
    ] {
        assert!(verdict(command, dir.path()).is_some(), "{command}");
    }
}

/// A body that can run its own comments keeps them judged.
#[test]
fn a_self_evaluating_body_keeps_its_comments_judged_10548() {
    let comment = "// require('fs').readFileSync('.env')\n";
    let bodies = [
        "eval(require('fs').readFileSync(__filename, 'utf8').slice(3));\n",
        // #10548 round 1: a destructured `vm` import, `require.main` and `module.filename`.
        "const { runInThisContext } = require('vm');\nrunInThisContext(src(__filename));\n",
        "const { runInThisContext: r } = require(\"node:vm\");\nr(src(__filename));\n",
        "eval(require('fs').readFileSync(require.main.filename, 'utf8').slice(3));\n",
        "eval(require('fs').readFileSync(module.filename, 'utf8').slice(3));\n",
    ];
    let mut allowed = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        let name = format!("e{i}.cjs");
        let full = format!("{comment}{body}");
        let dir = fixture(&[(name.as_str(), full.as_str())]);
        if verdict(&format!("node {name}"), dir.path()).is_none() {
            allowed.push(*body);
        }
    }
    assert!(allowed.is_empty(), "must refuse: {allowed:#?}");
}

/// Ordinary division after `)` still reads as JS: the member access allows.
#[test]
fn division_after_a_paren_reads_as_js_10548() {
    let body = "const avg = (a + b) / 2;\nconsole.log(cfg.key, arr[0] / avg);\n";
    assert!(node::js_reading(body).is_some());
    let dir = fixture(&[("avg.cjs", body)]);
    assert_eq!(verdict("node avg.cjs", dir.path()), None);
}

/// A body the JS reader cannot read in full is judged raw, as before.
#[test]
fn an_unreadable_js_body_is_judged_raw_10548() {
    let body = "const s = 'unterminated;\nfs.readFileSync(x.key);\n";
    assert_eq!(node::js_reading(body), None);
    let dir = fixture(&[("u.cjs", body)]);
    assert!(verdict("node u.cjs", dir.path()).is_some());
}

/// A non-JS file node loads or runs is judged raw, as before; a node-shebang
/// file with no extension is read as JS.
#[test]
fn a_non_js_file_node_loads_is_judged_raw_10548() {
    let dir = fixture(&[
        ("cat.sh", "cat keys/app.pem\n"),
        ("tool", "#!/usr/bin/env node\nconsole.log(cfg.key);\n"),
        ("app.js", "console.log(1);\n"),
    ]);
    assert!(verdict("node -r ./cat.sh app.js", dir.path()).is_some());
    assert!(verdict("node cat.sh", dir.path()).is_some());
    assert_eq!(verdict("node tool", dir.path()), None);
}
