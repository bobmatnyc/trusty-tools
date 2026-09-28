//! Which part of a leased command holds the build slot (#8261 repair r3).
//!
//! Why: a slot rations BUILDS. `cargo run` and `cargo watch` start a build and
//! then keep running for as long as the program or the watcher lives — a
//! server, a REPL, a watch loop left open for hours. Leasing the whole command
//! pinned a slot for that whole time, and a few of them starved every other
//! build on the machine.
//! What: [`split`] classifies the lease's argv. `cargo run` becomes
//! [`Split::BuildThenRun`]: `cargo build` with the same flags runs leased, then
//! the original `cargo run` runs unleased and finds the build fresh.
//! `cargo watch` becomes [`Split::Watch`]: the watcher runs unleased, and every
//! command it starts is handed to it as a `-s` shell command that the hook's
//! own rewrite has wrapped in `tm build-lease`, so each build takes its own
//! lease. Anything else is [`Split::Whole`]. Only a direct `cargo` argv is
//! split; a wrapped one (`sudo cargo run`, `sh -c '…'`) is leased whole.
//! Test: the suite below; `a_long_lived_cargo_run_releases_its_slot_after_the_build`,
//! `cargo_watch_leases_each_build_not_the_watcher` in `tests/tm_build_lease.rs`.

use super::pm_guard_bash::build_lease_rewrite::{LeaseRewrite, rewrite_for_lease};

/// How `tm build-lease` runs one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Split {
    /// Lease the command for its whole life.
    Whole,
    /// Lease `build`, release, then run the original command unleased.
    BuildThenRun {
        /// `cargo build` with the `cargo run` flags, program arguments dropped.
        build: Vec<String>,
    },
    /// Run this `cargo watch` argv unleased; `Err` names why it cannot be.
    Watch(Result<Vec<String>, String>),
}

/// Cargo's global options that take the next word as their value.
const CARGO_VALUE_FLAGS: &[&str] = &["-C", "-Z", "--config", "--color"];

/// `cargo watch` long options that take a value, besides `-x`/`-s`/`--features`.
const WATCH_VALUE_LONG: &[&str] = &[
    "--delay",
    "--ignore",
    "--package",
    "--watch",
    "--use-shell",
    "--workdir",
    "--env",
    "--env-file",
];

/// `cargo watch` short options that take a value, besides `-x`/`-s`.
const WATCH_VALUE_SHORT: &[char] = &['d', 'i', 'p', 'w', 'C', 'E', 'B', 'L'];

/// The cargo verbs `cargo watch` accepts as a bare subcommand (`cargo watch test`).
const WATCH_SUBCOMMANDS: &[&str] = &["bench", "build", "clippy", "run", "test"];

/// Classify `argv`; `heavy` and `prefix` are the hook's rewrite inputs.
///
/// Test: `cargo_run_leases_only_the_build`, `other_commands_are_leased_whole`,
/// `cargo_watch_commands_each_take_a_lease`.
pub(crate) fn split(argv: &[String], heavy: &[(String, Option<String>)], prefix: &str) -> Split {
    let Some(idx) = cargo_subcommand(argv) else {
        return Split::Whole;
    };
    match argv[idx].as_str() {
        "run" | "r" => {
            let mut build = argv[..idx].to_vec();
            build.push("build".to_string());
            build.extend(argv[idx + 1..].iter().take_while(|w| *w != "--").cloned());
            Split::BuildThenRun { build }
        }
        "watch" => Split::Watch(watch_argv(argv, idx, heavy, prefix)),
        _ => Split::Whole,
    }
}

/// The index of cargo's subcommand word, when `argv` runs `cargo` directly.
fn cargo_subcommand(argv: &[String]) -> Option<usize> {
    let program = argv.first()?;
    if program.rsplit('/').next() != Some("cargo") {
        return None;
    }
    let mut i = 1;
    while let Some(word) = argv.get(i) {
        if CARGO_VALUE_FLAGS.contains(&word.as_str()) {
            i += 2;
        } else if word.starts_with('+') || word.starts_with('-') {
            i += 1;
        } else {
            return Some(i);
        }
    }
    None
}

/// What one `cargo watch` argument is, per cargo-watch 8's own parser.
enum WatchArg {
    Exec(String),
    Shell(String),
    Features(String),
    /// Passed through unchanged: the option and its value, if any.
    Keep(Vec<String>),
}

/// The `cargo watch` argv with every command it runs as a leased `-s`.
///
/// What: mirrors cargo-watch 8's `set_commands` — `-x` commands (and a bare
/// `cargo watch test …`) first, `--features` injected after their verb, then
/// `-s` commands; a trailing `-- <cmd>` replaces them all; none at all means
/// `cargo check`. Each resulting command is passed through the hook's rewrite
/// and handed back as `-s`, in the same order. `Err` for a shape this cannot
/// mirror exactly (an unknown positional, a short-option cluster hiding a
/// value option, `--use-shell none` with a trailing command) — the caller
/// refuses rather than guess.
fn watch_argv(
    argv: &[String],
    idx: usize,
    heavy: &[(String, Option<String>)],
    prefix: &str,
) -> Result<Vec<String>, String> {
    let rest = &argv[idx + 1..];
    let (mut cargo_cmds, mut shell_cmds, mut kept) = (Vec::new(), Vec::new(), Vec::new());
    let (mut features, mut trailing) = (None, None);
    let mut i = 0;
    while let Some(word) = rest.get(i) {
        i += 1;
        if word == "--" {
            trailing = Some(&rest[i..]);
            break;
        }
        if WATCH_SUBCOMMANDS.contains(&word.as_str()) {
            cargo_cmds.push(rest[i - 1..].join(" "));
            break;
        }
        match watch_arg(word, rest, &mut i)? {
            WatchArg::Exec(cmd) => cargo_cmds.push(cmd),
            WatchArg::Shell(cmd) => shell_cmds.push(cmd),
            WatchArg::Features(list) => features = Some(list),
            WatchArg::Keep(words) => kept.extend(words),
        }
    }
    let mut commands: Vec<String> = cargo_cmds
        .iter()
        .map(|c| {
            format!(
                "cargo {}",
                with_features(c.trim_start(), features.as_deref())
            )
        })
        .collect();
    commands.extend(shell_cmds);
    if let Some(trail) = trailing {
        if kept
            .windows(2)
            .any(|w| w[0] == "--use-shell" && w[1].eq_ignore_ascii_case("none"))
        {
            return Err(
                "`cargo watch --use-shell none -- <cmd>` cannot be leased per build".into(),
            );
        }
        let joined = shlex::try_join(trail.iter().map(String::as_str))
            .map_err(|e| format!("the trailing `cargo watch` command cannot be quoted: {e}"))?;
        commands = vec![joined];
    }
    if commands.is_empty() {
        // cargo-watch appends `--features` to its default with no trailing space.
        let features = features.map(|f| format!(" --features {f}"));
        commands.push(format!("cargo check{}", features.unwrap_or_default()));
    }
    let mut out = argv[..=idx].to_vec();
    out.extend(kept);
    for command in commands {
        let leased = match rewrite_for_lease(&command, heavy, prefix) {
            LeaseRewrite::Rewrite(leased) => leased,
            LeaseRewrite::None => command,
            LeaseRewrite::Refuse(why) => return Err(why),
        };
        out.push("-s".to_string());
        out.push(leased);
    }
    Ok(out)
}

/// Classify `word`, consuming its value from `rest[*i]` when it takes one.
fn watch_arg(word: &str, rest: &[String], i: &mut usize) -> Result<WatchArg, String> {
    let (name, attached) = if let Some(long) = word.strip_prefix("--") {
        match long.split_once('=') {
            Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
            None => (word.to_string(), None),
        }
    } else if let Some(short) = word.strip_prefix('-').filter(|s| !s.is_empty()) {
        let mut chars = short.chars();
        let letter = chars.next().unwrap_or_default();
        let tail: String = chars.collect();
        if !matches!(letter, 'x' | 's') && !WATCH_VALUE_SHORT.contains(&letter) {
            if tail
                .chars()
                .any(|c| matches!(c, 'x' | 's') || WATCH_VALUE_SHORT.contains(&c))
            {
                return Err(format!(
                    "the `cargo watch` option cluster `{word}` hides an option that takes a \
                     value; spell it separately"
                ));
            }
            return Ok(WatchArg::Keep(vec![word.to_string()]));
        }
        (format!("-{letter}"), (!tail.is_empty()).then_some(tail))
    } else {
        return Err(format!("unrecognised `cargo watch` argument `{word}`"));
    };
    let takes_value = matches!(
        name.as_str(),
        "-x" | "--exec" | "-s" | "--shell" | "--features"
    ) || WATCH_VALUE_LONG.contains(&name.as_str())
        || (name.len() == 2
            && name
                .chars()
                .nth(1)
                .is_some_and(|c| WATCH_VALUE_SHORT.contains(&c)));
    if !takes_value {
        return Ok(WatchArg::Keep(vec![word.to_string()]));
    }
    let value = match attached {
        Some(value) => value,
        None => {
            let value = rest
                .get(*i)
                .ok_or_else(|| format!("`cargo watch {name}` needs a value"))?;
            *i += 1;
            value.clone()
        }
    };
    Ok(match name.as_str() {
        "-x" | "--exec" => WatchArg::Exec(value),
        "-s" | "--shell" => WatchArg::Shell(value),
        "--features" => WatchArg::Features(value),
        _ => WatchArg::Keep(vec![name, value]),
    })
}

/// cargo-watch's `--features` injection: after the verb, for the verbs it names.
fn with_features(cargo: &str, features: Option<&str>) -> String {
    let Some(features) = features else {
        return cargo.to_string();
    };
    let applies = ["b", "check", "doc", "r", "test", "install"]
        .iter()
        .any(|verb| cargo.starts_with(verb));
    if !applies {
        return cargo.to_string();
    }
    let boundary = cargo.find(char::is_whitespace).unwrap_or(cargo.len());
    let (verb, args) = cargo.split_at(boundary);
    format!("{verb} --features {features} {args}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    fn heavy() -> Vec<(String, Option<String>)> {
        ["test", "build", "run", "check", "clippy"]
            .iter()
            .map(|s| ("cargo".to_string(), Some((*s).to_string())))
            .collect()
    }

    fn watch(words: &[&str]) -> Result<Vec<String>, String> {
        match split(&argv(words), &heavy(), "tm build-lease --") {
            Split::Watch(out) => out,
            other => panic!("not a watch: {other:?}"),
        }
    }

    #[test]
    fn cargo_run_leases_only_the_build() {
        let got = split(
            &argv(&[
                "/bin/cargo",
                "+nightly",
                "--locked",
                "run",
                "-p",
                "x",
                "--release",
                "--",
                "--port",
                "8080",
            ]),
            &heavy(),
            "tm build-lease --",
        );
        let want = argv(&[
            "/bin/cargo",
            "+nightly",
            "--locked",
            "build",
            "-p",
            "x",
            "--release",
        ]);
        assert_eq!(got, Split::BuildThenRun { build: want });
        let alias = split(&argv(&["cargo", "-Z", "x", "r"]), &heavy(), "p");
        assert_eq!(
            alias,
            Split::BuildThenRun {
                build: argv(&["cargo", "-Z", "x", "build"])
            }
        );
    }

    #[test]
    fn other_commands_are_leased_whole() {
        for words in [
            &["cargo", "test", "--", "run"][..],
            &["sudo", "cargo", "run"],
            &["sh", "-c", "cargo run"],
            &["cargo"],
        ] {
            assert_eq!(
                split(&argv(words), &heavy(), "p"),
                Split::Whole,
                "{words:?}"
            );
        }
    }

    #[test]
    fn cargo_watch_commands_each_take_a_lease() {
        let got = watch(&[
            "cargo",
            "watch",
            "-c",
            "-w",
            "src",
            "-x",
            "test -p x",
            "-s",
            "ls",
        ]);
        let want = argv(&[
            "cargo",
            "watch",
            "-c",
            "-w",
            "src",
            "-s",
            "tm build-lease -- cargo test -p x",
            "-s",
            "ls",
        ]);
        assert_eq!(got, Ok(want));
        // No command means `cargo check`, with `--features` injected.
        let got = watch(&["cargo", "watch", "--features=a"]).expect("ok");
        assert_eq!(
            got[2..],
            argv(&["-s", "tm build-lease -- cargo check --features a"])
        );
        // `-x` runs before `-s`; a bare `cargo watch run` is a cargo command.
        let got =
            watch(&["cargo", "watch", "-s", "echo", "-xclippy", "run", "-p", "a"]).expect("ok");
        assert_eq!(
            got[2..],
            argv(&[
                "-s",
                "tm build-lease -- cargo clippy",
                "-s",
                "tm build-lease -- cargo run -p a",
                "-s",
                "echo"
            ])
        );
        // A trailing command replaces the rest; light cargo verbs stay unleased.
        let got = watch(&["cargo", "watch", "-x", "test", "--", "cargo", "fmt"]).expect("ok");
        assert_eq!(got[2..], argv(&["-s", "cargo fmt"]));
    }

    #[test]
    fn unmirrorable_cargo_watch_shapes_are_refused() {
        for words in [
            &["cargo", "watch", "stray"][..],
            &["cargo", "watch", "-cx", "test"],
            &["cargo", "watch", "-x"],
            &[
                "cargo",
                "watch",
                "--use-shell",
                "none",
                "--",
                "cargo",
                "test",
            ],
        ] {
            assert!(watch(words).is_err(), "{words:?}");
        }
    }
}
