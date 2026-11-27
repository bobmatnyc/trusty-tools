//! #11800: the `op` shapes the #11800 reviews raised, pinned as refusals of
//! the unchanged guard.
//!
//! Why: #11800 made three seeded Architect scripts pass by rewording their
//! prose, not by narrowing the guard. These rows keep every reviewed
//! secret-reading `op` shape refused, so a later guard change that lets one
//! through fails here.
//! What: plain Bash commands judged by `evaluate_credential_print_command`,
//! and Python bodies written to a temp dir and judged, as `python3 s.py`, by
//! `evaluate_script_body_secret_read`. Every row must refuse. Every command
//! and body is fake; none is ever run.
//! Test: `the_bash_path_refuses_every_op_review_shape_11800`,
//! `a_python_body_with_a_secret_reading_op_refuses_11800`.

use crate::commands::pm_guard_bash::evaluate_credential_print_command;
use crate::commands::pm_guard_secret_script::evaluate_script_body_secret_read;

/// Plain Bash commands from the #11800 reviews; each refuses.
// #11800: `op <<EOF $SUB` and `op {fd}>/dev/null $SUB` allow and are tracked
// in #12011, so they are not listed.
const SECRET_BASH_COMMANDS: &[&str] = &[
    // The plain reads.
    "op read op://v/i/f",
    "op read \"$REF\"",
    "op --account x read op://v/i/f",
    "op --account \"$A\" read \"$REF\"",
    // A `$` or backtick in the subcommand slot.
    "op $SUB op://v/i/f",
    "op `echo read` op://v/i/f",
    "op $(echo read) op://v/i/f",
    // A `$` or backtick glued to a literal in the slot.
    "E=ead; op r$E op://v/i/f",
    "op re`printf a`d op://v/i/f",
    "op re$(printf a)d op://v/i/f",
    "op r${E}d op://v/i/f",
    "op \"r\"$E op://v/i/f",
    "op --account x r$E op://v/i/f",
    "op -a$X r$E",
    "op --account=x r$E",
    "op r'$E'",
    // Redirections in the slot.
    "op 2>/dev/null $SUB op://v/i/f",
    "op >&2 $SUB",
    "op 2>&1 $SUB",
    "op </dev/null $SUB",
    "op <<<x $SUB",
    // Quoted or escaped whitespace in an option value.
    "op --account=\"a b\" $SUB",
    "op --account \"a b\" $SUB",
    "op --account=a\\ b $SUB",
    // Option values, with and without `=`.
    "op --vault x $SUB",
    "op --vault x $SUB op://v/i/f",
    "op --account x $SUB op://v/i/f",
    "op -v x $SUB",
    "op --account=x $SUB op://v/i/f",
    "python3 -c '[\"op\",\"--vault\",\"x\",sub,ref]'",
    // A prose `op` typed inline.
    "python3 <<'EOF'\n\"\"\"the op re-checks `at` under datg's lock.\"\"\"\nEOF\n",
];

/// Python bodies that read a secret, or that the guard cannot tell from one;
/// each refuses when run as a script.
// #11800: the `op <<EOF $SUB` and `op {fd}>/dev/null $SUB` bodies are #12011's.
const SECRET_PY_BODIES: &[&str] = &[
    // A `$` or backtick glued into the subcommand slot.
    "os.system(\"E=ead; op r$E op://v/i/f\")",
    "os.system(\"op re`printf a`d op://v/i/f\")",
    "os.system(\"op re$(printf a)d op://v/i/f\")",
    "os.system(\"op r${E}d op://v/i/f\")",
    "os.system('op \"r\"$E op://v/i/f')",
    "os.system(\"op --account x r$E op://v/i/f\")",
    "os.system(\"op --account=x r$E\")",
    "os.system(\"op r'$E'\")",
    // Redirections in the slot.
    "os.system(\"op 2>/dev/null $SUB op://v/i/f\")",
    "os.system(\"op >&2 $SUB\")",
    "os.system(\"op 2>&1 $SUB\")",
    "os.system(\"op </dev/null $SUB\")",
    "os.system(\"op <<<x $SUB\")",
    // Quoted or escaped whitespace in an option value.
    "os.system('op --account=\"a b\" $SUB')",
    "os.system('op --account \"a b\" $SUB')",
    "os.system(\"op --account=a\\\\ b $SUB\")",
    // Option values, with and without `=`, and `--`.
    "os.system(\"op --vault x $SUB\")",
    "os.system(\"op --vault x $SUB op://v/i/f\")",
    "os.system(\"op --account x $SUB op://v/i/f\")",
    "os.system(\"op -v x $SUB\")",
    "os.system(\"op --account=x $SUB op://v/i/f\")",
    "os.system(\"op -- $SUB op://v/i/f\")",
    // Wrappers, a path-invoked op, op after an operator or a newline.
    "os.system(\"env op $SUB\")",
    "os.system(\"sudo op $SUB\")",
    "os.system(\"exec op $SUB\")",
    "os.system(\"command op $SUB\")",
    "os.system(\"/usr/local/bin/op $SUB\")",
    "os.system(\"./op $SUB\")",
    "os.system(\"true | op $SUB\")",
    "os.system(\"true && op $SUB\")",
    "os.system(\"true; op $SUB\")",
    "os.system(\"\"\"true\nop $SUB\"\"\")",
    "os.system(\"\"\"bash <<EOF\nop $SUB op://v/i/f\nEOF\"\"\")",
    // The plain reads, and a literal subcommand that reads a secret.
    "os.system(\"op read op://v/i/f\")",
    "os.system(\"op item get $ITEM --reveal\")",
    "os.system(\"op items get $ITEM\")",
    "os.system(\"op re $X\")",
    "os.system(\"OP $SUB\")",
    "os.system(\"o''p $SUB\")",
    "os.system(\"op must $(op $SUB)\")",
    "os.system(\"op must; op $SUB\")",
    "os.system(\"op must\"); os.system(\"op $SUB\")",
    // Argv lists, concatenation and f-strings beside an unrelated `$`.
    "subprocess.run([\"op\", sub, ref])",
    "subprocess.run([\"op\", \"--vault\", \"x\", sub, ref])",
    "subprocess.run([\"op\", \"read\", ref])",
    "import subprocess\nsubprocess.run([\"op\", \"read\", ref])",
    "import subprocess\nsubprocess.run([\"op\", sub, ref])",
    "import subprocess\nsubprocess.run([\"op\", \"--vault\", \"x\", sub, ref])",
    "import subprocess\nsubprocess.run([\"op\", \"--account\", \"x\", \"read\", ref])",
    "import subprocess\nsubprocess.run([\"op\", \"item\", \"get\", item])",
    "os.system(\"op \" + sub)\nprint(\"$HOME\")",
    "subprocess.run(f\"sudo op {sub} {ref}\", shell=True)\nprint(\"$HOME\")",
    "os.system(\"op\" + sub)\nprint(\"$HOME\")",
    "os.system(\"op\" \" \" + sub)\nprint(\"$HOME\")",
    "os.system((\"op\") + sub)\nprint(\"$HOME\")",
    // String building around a quoted or literal `op`.
    "os.system(str(\"op\") + \" read op://v/i/f\")",
    "os.system(str(\"op\") + \" \" + sub)\nprint(\"$HOME\")",
    "os.system(d.get(\"op\") + \" read op://v/i/f\")",
    "cmd = {\"op\" : \"read\"}\nos.system(\" \".join(*cmd.items()) + \" op://v/i/f\")",
    "os.system(\"op VERB op://v/i/f\".replace(\"VERB\", \"read\"))",
    "os.system(repr(\"op\")[1:-1] + \" read op://v/i/f\")",
    "os.system(\"op x read op://v/i/f\".replace(\"x \", \"\"))",
    "os.system(\" \".join(next(iter({\"op\" : \"read\"}.items()))) + \" op://v/i/f\")",
    "os.system(((\"op\")) + \" read op://v/i/f\")",
    "\"\"\"the op script\"\"\"\nos.system(\"op $SUB\")",
    "\"\"\"the op script and the op tool\"\"\"\nos.system(\"op read op://v/i/f\")",
    "os.system(\"op VERB op://v/i/f\".replace(\"VERB\", sub))\nprint(\"$HOME\")",
    // A docstring that is read, run, continued or not a statement.
    "def f():\n    \"\"\"op read op://v/i/f\"\"\"\n\nexec(f.__doc__)\nprint(\"$HOME\")",
    "def f():\n    \"\"\"op VERB op://v/i/f\"\"\"\n\nexec(f.__doc__.replace(\"VERB\", \"read\"))",
    "def f():\n    \"\"\"op VERB op://v/i/f\"\"\"\n\nos.system(f.__doc__.replace(\"VERB\", \"read\"))",
    "x = 1\n\"\"\"op VERB op://v/i/f\"\"\".replace(\"VERB\", \"read\")\nprint(\"$HOME\")",
    "\"\"\"op VERB op://v/i/f\"\"\" \\\n.replace(\"VERB\", \"read\")\nprint(\"$HOME\")",
    "\"\"\"op VERB op://v/i/f\"\"\"; print(\"read\")",
    "s = \"\"\"op VERB op://v/i/f\"\"\"\nprint(s.replace(\"VERB\", \"read\"))",
    "x = \\\n\"\"\"op VERB op://v/i/f\"\"\"\nprint(x.replace(\"VERB\", \"read\"))",
    "x = (\n    \"\"\"op VERB op://v/i/f\"\"\"\n)\nprint(x.replace(\"VERB\", \"read\"))",
    "os.system(\"\"\"op VERB op://v/i/f\"\"\".replace(\"VERB\", \"read\"))",
    "os.system(\n\"\"\"op VERB op://v/i/f\"\"\"\n.replace(\"VERB\", \"read\"))",
    "def f():\n    \"\"\"op VERB op://v/i/f\"\"\"\n\nprint(inspect.getdoc(f).replace(\"VERB\", \"read\"))",
    "def f():\n    \"\"\"op VERB op://v/i/f\"\"\"\n\nsrc = open(__file__).read()\nprint(src.replace(\"VERB\", \"read\"))",
    "\"\"\"op VERB op://v/i/f\"\"\"\nprint(eval(\"1\"), \"read\")",
    "\"\"\"op VERB op://v/i/f\"\"\"\nc = compile(\"x\", \"f\", \"exec\")\nprint(\"read\")",
];

/// The script-body verdict for `body` run as `python3 s.py` from a temp dir.
fn body_verdict(body: &str) -> Option<String> {
    let dir = tempfile::tempdir().expect("dir");
    std::fs::write(dir.path().join("s.py"), body).expect("write script");
    evaluate_script_body_secret_read("python3 s.py", dir.path())
}

/// Every row of `rows` that `verdict` allows.
fn allowed(rows: &[&str], verdict: impl Fn(&str) -> Option<String>) -> Vec<String> {
    rows.iter()
        .filter(|row| verdict(row).is_none())
        .map(|row| format!("{row:?}"))
        .collect()
}

/// 🔴 CONTROL (#11800): every plain Bash `op` shape the reviews found refuses.
#[test]
fn the_bash_path_refuses_every_op_review_shape_11800() {
    let bad = allowed(SECRET_BASH_COMMANDS, evaluate_credential_print_command);
    assert!(bad.is_empty(), "expected refused:\n{}", bad.join("\n"));
}

/// 🔴 CONTROL (#11800): every Python body with a secret-reading `op` refuses
/// when run as a script.
#[test]
fn a_python_body_with_a_secret_reading_op_refuses_11800() {
    let bad = allowed(SECRET_PY_BODIES, body_verdict);
    assert!(bad.is_empty(), "expected refused:\n{}", bad.join("\n"));
}
