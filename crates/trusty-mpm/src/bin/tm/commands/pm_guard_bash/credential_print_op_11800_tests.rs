//! #11800: a bare `op` trips the run-time rule only when its subcommand slot
//! is `$`, so prose `op` before a later backtick or `$` passes, and every `op`
//! shape that reads a secret still refuses. Every command here is fake; none
//! is ever run.

use super::super::evaluate_credential_print_command;
use crate::commands::pm_guard_secret_script::inline_equivalent;

/// A Python body as the script-body judge reads it.
fn python(body: &str) -> String {
    inline_equivalent("python3", body)
}

/// Every row that gets the wrong verdict, as `row -> verdict`.
fn wrong(want_deny: bool, rows: &[String]) -> Vec<String> {
    rows.iter()
        .filter_map(|c| {
            let got = evaluate_credential_print_command(c);
            (got.is_some() != want_deny).then(|| format!("{c:?} -> {got:?}"))
        })
        .collect()
}

/// 🔴 CONTROL (#11800): every `op` shape that reads a secret and refuses on
/// the pre-#11800 guard still refuses, the option-first `$` forms included.
#[test]
fn denies_every_secret_reading_op_shape_11800() {
    let mut rows: Vec<String> = [
        "op read op://v/i/f",
        "op read \"$REF\"",
        "op --account x read op://v/i/f",
        "op --account \"$A\" read \"$REF\"",
        "op --vault x $SUB",
        "op --vault x $SUB op://v/i/f",
        "op --account x $SUB op://v/i/f",
        "op -v x $SUB",
        "op --account=x $SUB op://v/i/f",
        "op $SUB op://v/i/f",
        "op `echo read` op://v/i/f",
        "op $(echo read) op://v/i/f",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    rows.extend(
        [
            "import subprocess\nsubprocess.run([\"op\", \"read\", ref])",
            "import subprocess\nsubprocess.run([\"op\", sub, ref])",
            "import subprocess\nsubprocess.run([\"op\", \"--vault\", \"x\", sub, ref])",
            "import subprocess\nsubprocess.run([\"op\", \"--account\", \"x\", \"read\", ref])",
            "import subprocess\nsubprocess.run([\"op\", \"item\", \"get\", item])",
        ]
        .iter()
        .map(|b| python(b)),
    );
    let wrong = wrong(true, &rows);
    assert!(wrong.is_empty(), "expected refused:\n{}", wrong.join("\n"));
}

/// 🔴 REGRESSION (#11800): a prose `op` whose next word is not `$` passes,
/// though a backtick or `$` follows later in the body (il_remind.py's
/// docstring, datg_ops.py's error text).
#[test]
fn allows_a_prose_op_before_a_later_backtick_or_dollar_11800() {
    let rows: Vec<String> = [
        "def fire():\n    \"\"\"A reminder fires again only after its repeat moved `at` past now: \
         the op re-checks `at` under datg's lock.\"\"\"",
        "def check(verb):\n    raise ValueError(f\"unknown op {verb!r}\")\n\nPRICE = \"costs $5\"",
        "# each op must be an object\nprint(\"$HOME\")",
    ]
    .iter()
    .map(|b| python(b))
    .collect();
    let wrong = wrong(false, &rows);
    assert!(wrong.is_empty(), "expected allowed:\n{}", wrong.join("\n"));
}

/// 🔴 REGRESSION (#11800): an `"op"` dict key, as il_remind.py writes its
/// ledger ops, passes with a backtick later in the same body.
#[test]
fn allows_an_op_dict_key_11800() {
    let rows: Vec<String> = [
        "write_ops_ev(p, [{\"op\": \"remind_fire\", \"now\": now}])\n\"\"\"moves `at`\"\"\"",
        "write_ops_ev(p, [{\"op\" : \"remind_done\", \"id\": i}])\n\"\"\"moves `at`\"\"\"",
    ]
    .iter()
    .map(|b| python(b))
    .collect();
    let wrong = wrong(false, &rows);
    assert!(wrong.is_empty(), "expected allowed:\n{}", wrong.join("\n"));
}
