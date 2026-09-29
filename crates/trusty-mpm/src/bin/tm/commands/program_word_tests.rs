//! #8735: the shared program-word resolver.

use super::{COMMAND_WRAPPERS, GRAMMARS, precommand_index, resolve_program_word, sample_operand};

/// The program word `command` resolves to, or `None` when resolution fails.
fn program(command: &str) -> Option<String> {
    let argv = shlex::split(command).expect("test rows lex");
    let word = resolve_program_word(&argv).ok()?;
    Some(argv.get(word.index).cloned().unwrap_or_default())
}

#[test]
fn resolves_past_each_wrapper_and_its_options() {
    for (command, want) in [
        ("noglob echo x", "echo"),
        ("nocorrect echo x", "echo"),
        ("nice -n 5 echo x", "echo"),
        ("nice -n5 echo x", "echo"),
        ("nice -10 echo x", "echo"),
        ("nice --adjustment=3 echo x", "echo"),
        ("timeout 5 echo x", "echo"),
        ("timeout -s KILL -k 2 1.5m cat x", "cat"),
        ("timeout --signal=9 --preserve-status 30s cat x", "cat"),
        ("timeout -vs9 5 cat x", "cat"),
        ("command -p cat x", "cat"),
        ("builtin echo x", "echo"),
        ("exec -a name -c cat x", "cat"),
        ("env -i PATH=/bin cat x", "cat"),
        ("env - FOO=1 cat x", "cat"),
        ("env -u BAR FOO=1 cat x", "cat"),
        ("/usr/bin/env cat x", "cat"),
        ("sudo -u x timeout 5 cat x", "cat"),
        ("sudo -E --user=root FOO=1 cat x", "cat"),
        ("time -p echo x", "echo"),
        ("stdbuf -oL -e 0 cat x", "cat"),
        ("ionice -c 3 -n7 nice cat x", "cat"),
        ("nohup cat x", "cat"),
        ("caffeinate -i -t 60 cat x", "cat"),
        ("xargs -n 1 -0 echo", "echo"),
        ("FOO=1 '\\timeout' 5 cat x", "cat"),
        ("sudo -- cat x", "cat"),
        // #8735 round 2: Homebrew's g-prefixed coreutils and the runners.
        ("gtimeout 5 echo x", "echo"),
        ("gnice -n 5 echo x", "echo"),
        ("gstdbuf -oL echo x", "echo"),
        ("gnohup echo x", "echo"),
        ("genv -i FOO=1 echo x", "echo"),
        ("setsid -f echo x", "echo"),
        ("chrt -f 10 echo x", "echo"),
        ("taskset -c 0-3,5 echo x", "echo"),
        ("taskset 0x3 echo x", "echo"),
        ("unbuffer -p echo x", "echo"),
        ("flock -n -w 5 /tmp/lock echo x", "echo"),
        ("sudo -k rm -f x", "rm"),
        ("xargs -J % rm %", "rm"),
        ("xargs -R 2 -I % rm %", "rm"),
        // A wrapper with nothing after it runs as itself.
        ("env", "env"),
        ("env -i", "env"),
        ("FOO=1", ""),
        ("cargo test", "cargo"),
    ] {
        assert_eq!(program(command).as_deref(), Some(want), "{command}");
    }
}

#[test]
fn an_unknown_or_unmeasurable_option_fails() {
    for command in [
        "timeout --bogus 5 echo x",
        "timeout echo x",
        "timeout 5x echo x",
        "timeout",
        "nice -z echo x",
        "nice -n",
        "sudo -X cat x",
        "sudo --bogus cat x",
        "env -S 'cat x'",
        "genv -S 'cat x'",
        "noglob -x echo",
        "chrt -f high echo x",
        "taskset zz echo x",
        "flock /tmp/lock -c 'cat x'",
        "flock -c 'cat x' /tmp/lock",
        "xargs --bogus echo",
        "sudo -u",
    ] {
        assert_eq!(program(command), None, "{command}");
    }
}

#[test]
fn every_wrapper_has_a_grammar() {
    assert_eq!(COMMAND_WRAPPERS.len(), GRAMMARS.len());
    for wrapper in COMMAND_WRAPPERS {
        let command = format!("{wrapper} {} cat x", sample_operand(wrapper));
        assert_eq!(program(&command).as_deref(), Some("cat"), "{command}");
    }
}

/// #8735 round 2: `command -v rm` and `sudo -l rm` print where `rm` lives or
/// whether it may run; the wrapper is the program and runs nothing.
#[test]
fn a_lookup_option_runs_nothing() {
    for (command, at) in [
        ("command -v rm", 0),
        ("command -V rm -rf /", 0),
        ("command -pv rm", 0),
        ("sudo -l rm -rf /", 0),
        ("sudo -u root -l rm", 0),
        ("nice -n 5 command -v rm", 3),
    ] {
        let argv = shlex::split(command).expect("test rows lex");
        let word = resolve_program_word(&argv).expect("a lookup resolves");
        assert!(word.lookup && word.index == at, "{command}: {word:?}");
    }
    let argv = shlex::split("sudo -u l rm x").expect("lexes");
    let word = resolve_program_word(&argv).expect("resolves");
    assert!(
        !word.lookup && word.index == 3,
        "`-u l` is a value, not `-l`"
    );
}

/// The `strip_wrapper_prefix` contract: `xargs` is a program, and any wrapper
/// option, or a bare wrapper, answers `None`.
#[test]
fn precommand_index_keeps_the_legacy_none_answers() {
    let index = |command: &str| precommand_index(&shlex::split(command).expect("lexes"));
    assert_eq!(index("timeout 5 make"), Some(2));
    assert_eq!(index("noglob make"), Some(1));
    assert_eq!(index("xargs make"), Some(0));
    assert_eq!(index("nice xargs make"), Some(1));
    assert_eq!(index("FOO=1"), Some(1));
    assert_eq!(index("sudo -u root make"), None);
    assert_eq!(index("env -i make"), None);
    assert_eq!(index("sudo"), None);
    assert_eq!(index("timeout make"), None);
}
