//! Unit rows for the interpreter option walk (#10672): which word is the
//! script, which value words are skipped, and where the walk stops.

use super::super::Role;
use super::{Family, family_of, interpreter_candidates};

/// The `(word, role)` pairs `command`'s interpreter arguments name.
fn walk(command: &str) -> Vec<(String, Role)> {
    let argv: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    let family = family_of(&argv[0]).expect("a listed interpreter");
    interpreter_candidates(family, &argv[0], &argv[1..])
        .into_iter()
        .map(|c| (c.word, c.role))
        .collect()
}

/// The words `command` names with `role`.
fn words(command: &str, role: Role) -> Vec<String> {
    walk(command)
        .into_iter()
        .filter(|(_, r)| *r == role)
        .map(|(w, _)| w)
        .collect()
}

/// A value-taking option consumes its value word, separate, attached or at
/// the end of a cluster, so the script is the word after it.
#[test]
fn option_values_are_skipped_10672() {
    for (command, script) in [
        ("python3 -W in.docx s.py", "s.py"),
        ("python3 -Wignore s.py", "s.py"),
        ("python3 -X dev s.py", "s.py"),
        ("python3 -uX in.pdf s.py", "s.py"),
        ("python3 -Wd s.py", "s.py"),
        ("python3 --check-hash-based-pycs always s.py", "s.py"),
        ("bash -o pipefail s.sh", "s.sh"),
        ("bash -euo pipefail s.sh", "s.sh"),
        ("bash -oe pipefail s.sh", "s.sh"),
        ("bash +o in.docx s.sh", "s.sh"),
        ("bash +x s.sh", "s.sh"),
        ("bash --rcfile rc s.sh", "s.sh"),
        ("zsh --emulate sh s.sh", "s.sh"),
        ("ruby -I lib s.rb", "s.rb"),
        ("ruby -Ilib s.rb", "s.rb"),
        ("ruby -wr json s.rb", "s.rb"),
        ("ruby -rjson s.rb", "s.rb"),
        ("ruby -W s.rb", "s.rb"),
        ("ruby -W:no-deprecated s.rb", "s.rb"),
        ("ruby --encoding utf-8 s.rb", "s.rb"),
        ("perl -I lib s.pl", "s.pl"),
        ("perl -Ilib s.pl", "s.pl"),
        ("perl -i.bak s.pl", "s.pl"),
        ("perl -l s.pl", "s.pl"),
        ("node -C dev s.js", "s.js"),
        ("node --conditions dev s.js", "s.js"),
        ("node -r pre.js s.js", "s.js"),
    ] {
        assert_eq!(
            words(command, Role::Positional),
            vec![script.to_string()],
            "`{command}`"
        );
        assert!(
            words(command, Role::Doubtful).is_empty(),
            "`{command}` has no word in doubt"
        );
    }
}

/// A file an option loads as code is a candidate beside the script.
#[test]
fn a_loaded_option_value_is_judged_beside_the_script_10672() {
    for (command, loaded) in [
        ("ruby -r pre.rb s.rb", "pre.rb"),
        ("ruby -wrpre.rb s.rb", "pre.rb"),
        ("node -r pre.js s.js", "pre.js"),
        ("node --import=pre.mjs s.js", "pre.mjs"),
        ("bash --rcfile rc s.sh", "rc"),
    ] {
        assert_eq!(
            words(command, Role::Loaded),
            vec![loaded.to_string()],
            "`{command}`"
        );
    }
}

/// The walk stops at the script: a `-c`, `-` or data word after it is the
/// script's own argument, never inline code or a second script, and a
/// redirected stdin is data once a script is named.
#[test]
fn the_walk_stops_at_the_script_10672() {
    for command in [
        "python3 s.py -c x",
        "python3 s.py - x",
        "python3 s.py in.docx out.docx",
        "python3 s.py < in.docx",
        "python3 < in.docx s.py",
        "bash s.sh -c x",
        "bash s.sh -",
        "ruby s.rb -e x",
        "perl s.pl -e x",
        "node s.js -e x",
    ] {
        let script = command
            .split_whitespace()
            .find(|w| w.starts_with("s."))
            .expect("a script word");
        assert_eq!(
            walk(command),
            vec![(script.to_string(), Role::Positional)],
            "`{command}`"
        );
    }
}

/// An inline option before any script names no script.
#[test]
fn an_inline_option_before_the_script_names_none_10672() {
    for command in ["python3 -c x s.py", "bash -c x s.sh", "python3 - s.py"] {
        assert!(
            words(command, Role::Positional).is_empty(),
            "`{command}` runs inline code"
        );
    }
}

/// After a `node` or `ruby` long option this module does not list, the next
/// operand may be the script too, and a later inline option keeps both.
#[test]
fn an_unlisted_long_option_puts_the_next_operand_in_doubt_10672() {
    for (command, first, second) in [
        (
            "node --title notes.txt creds.js in.docx",
            "notes.txt",
            "creds.js",
        ),
        ("ruby --jit-x v s.rb", "v", "s.rb"),
    ] {
        assert_eq!(
            walk(command),
            vec![
                (first.to_string(), Role::Positional),
                (second.to_string(), Role::Doubtful)
            ],
            "`{command}`"
        );
    }
    // An inline option after a word in doubt may be the script's argument.
    assert_eq!(
        walk("node --title v -e x"),
        vec![("v".to_string(), Role::Positional)]
    );
    // A long option with `=` carries its value: no doubt.
    assert_eq!(
        walk("node --title=x s.js in.docx"),
        vec![("s.js".to_string(), Role::Positional)]
    );
    // Python and shell long options take no unlisted value.
    assert_eq!(
        walk("python3 --unknown s.py in.docx"),
        vec![("s.py".to_string(), Role::Positional)]
    );
    assert_eq!(family_of("python3.12"), Some(Family::Python));
}
