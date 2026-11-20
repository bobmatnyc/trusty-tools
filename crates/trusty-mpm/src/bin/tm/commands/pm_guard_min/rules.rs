//! Rule dispatch, rule 1 (trust-anchor writes) and rule 5 (production
//! deletes) for the minimal pm-guard (#11431).
//!
//! Why: each rule decides on a structured fact — a canonical path or a
//! parsed argv — never on words in the command text (brief §2).
//! What: [`evaluate`] turns one `PreToolUse` payload into facts and returns
//! the first [`Hit`] of the rules the profile runs, plus the unparsed count.
//! Rules 2 and 3 live in [`super::rules_git`].
//! Test: `rules_tests.rs`, the replay in `replay_tests.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::facts::{Cmd, parse_bash};
use super::paths::{canon, is_trust_anchor, resolve};
use super::rules_git::{r2_destructive, r3_cross_repo};

/// What a rule evaluation may read besides the payload.
#[derive(Debug, Clone)]
pub(crate) struct GuardEnv {
    pub home: PathBuf,
    /// `CLAUDE_PROJECT_DIR`: the session's repository.
    pub project_dir: Option<PathBuf>,
    /// The payload `cwd`, or the process cwd.
    pub cwd: PathBuf,
    /// Rule 5 allow list for this session: argv prefixes (`gh api -X DELETE repos/a/b`).
    pub r5_allow: Vec<String>,
    /// The Architect runs rule 1 only (brief §2).
    pub architect: bool,
}

/// One would-refuse.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Hit {
    pub rule: &'static str,
    pub fact: Value,
}

/// The first would-refuse for one tool call, and how many commands were unparsed.
///
/// Why: see the module doc.
/// What: Write/Edit/MultiEdit/NotebookEdit → rule 1 on the target path.
/// Bash → rule 1 on every write target, then (PM profile) rule 2 on every
/// command, rules 3 and 5 on parsed commands only. Other tools → nothing.
/// Test: `r1_flags_writes_to_each_anchor`, `r5_flags_delete_verbs_unless_allowed`.
pub(crate) fn evaluate(tool: &str, input: &Value, env: &GuardEnv) -> (Option<Hit>, usize) {
    match tool {
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => (r1_tool_write(input, env), 0),
        "Bash" => {
            let command = input["command"].as_str().unwrap_or("");
            let facts = parse_bash(command, &env.cwd, &env.home);
            let r1 = facts
                .redirects
                .iter()
                .filter_map(|r| r.target.lit().map(|t| (t, &r.cwd, None)))
                .chain(facts.cmds.iter().flat_map(|c| {
                    write_targets(c)
                        .into_iter()
                        .map(move |t| (t, &c.cwd, Some(c)))
                }))
                .find_map(|(t, cwd, cmd)| r1_path(&resolve(cwd, t), env, false, cmd));
            let hit = r1.or_else(|| {
                if env.architect {
                    return None;
                }
                facts.cmds.iter().find_map(|c| {
                    r2_destructive(c, env).or_else(|| {
                        (!c.unparsed)
                            .then(|| r3_cross_repo(c, env).or_else(|| r5_delete(c, env)))
                            .flatten()
                    })
                })
            });
            (hit, facts.unparsed)
        }
        _ => (None, 0),
    }
}

fn r1_tool_write(input: &Value, env: &GuardEnv) -> Option<Hit> {
    let path = input["file_path"]
        .as_str()
        .or_else(|| input["notebook_path"].as_str())?;
    let real = canon(&resolve(&env.cwd, path));
    // Decide on the canonical path first; only a project-tier settings file is read.
    if is_trust_anchor(&real, &env.home, true) {
        return r1_path(&real, env, true, None);
    }
    if !is_trust_anchor(&real, &env.home, false) {
        return None;
    }
    // A project-tier settings write is a rule-1 fact only when it touches a guard entry.
    let existing = std::fs::read_to_string(&real).unwrap_or_default();
    let touches = input.to_string().contains("--pm-guard") || existing.contains("--pm-guard");
    r1_path(&real, env, !touches, None)
}

fn r1_path(path: &Path, env: &GuardEnv, no_guard_entry: bool, cmd: Option<&Cmd>) -> Option<Hit> {
    let real = canon(path);
    is_trust_anchor(&real, &env.home, no_guard_entry).then(|| {
        let mut fact = cmd.map_or_else(|| json!({}), argv_fact);
        fact["target"] = json!(real.display().to_string());
        Hit { rule: "R1", fact }
    })
}

/// The files a command writes, read from its argv.
///
/// Why: rule 1 must see `cp x ~/.cargo/bin/tm` and the #8439 in-place
/// `plutil -extract`, not only `>` redirects.
/// What: `tee` args; the last arg of `cp`/`mv`/`install`/`ln`; `rm`,
/// `unlink`, `truncate`, `shred` args; `sed -i`/`perl -i` args; `plutil`'s
/// last arg for an editing verb with no `-o`, or its `-o <file>`; `dd of=`.
/// Test: `r1_flags_writes_to_each_anchor`.
pub(crate) fn write_targets(cmd: &Cmd) -> Vec<&str> {
    let args = cmd.lit_args();
    let pos: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let has = |f: &str| args.iter().any(|a| a.starts_with(f));
    match cmd.name() {
        "tee" | "rm" | "unlink" | "truncate" | "shred" => pos,
        "cp" | "mv" | "install" | "ln" if pos.len() >= 2 => {
            pos.last().copied().into_iter().collect()
        }
        "sed" if has("-i") || has("--in-place") => pos,
        "perl"
            if args
                .iter()
                .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('i')) =>
        {
            pos
        }
        "plutil" => {
            let edits = ["-extract", "-convert", "-insert", "-replace", "-remove"]
                .iter()
                .any(|v| args.contains(v));
            // `-o <file>` writes <file>; `-o -` writes stdout; no `-o` writes the input.
            match args.iter().position(|a| *a == "-o") {
                Some(i) => args
                    .get(i + 1)
                    .filter(|o| **o != "-")
                    .copied()
                    .into_iter()
                    .collect(),
                None if edits => pos.last().copied().into_iter().collect(),
                None => Vec::new(),
            }
        }
        "dd" => args.iter().filter_map(|a| a.strip_prefix("of=")).collect(),
        _ => Vec::new(),
    }
}

/// Rule 5: a production-delete verb not on this session's allow list.
///
/// Why: a remote delete cannot be undone from the session (#8875).
/// What: `gh api` with method DELETE, `gh repo delete`, `aws` `delete*` /
/// `terminate*` / `s3 rb|rm`, `vercel|vc rm|remove`, `fly|flyctl destroy`,
/// `heroku apps:destroy`, `kubectl delete`, `terraform destroy`, `gcloud ...
/// delete`. An allow-list entry is an argv prefix.
/// Test: `r5_flags_delete_verbs_unless_allowed`.
pub(crate) fn r5_delete(cmd: &Cmd, env: &GuardEnv) -> Option<Hit> {
    let args = cmd.lit_args();
    let pos: Vec<&str> = args
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let first = pos.first().copied().unwrap_or("");
    let hit = match cmd.name() {
        "gh" if first == "api" => args.iter().enumerate().any(|(i, a)| {
            let method = match *a {
                "-X" | "--method" => args.get(i + 1).copied().unwrap_or(""),
                other => other
                    .strip_prefix("--method=")
                    .or_else(|| other.strip_prefix("-X"))
                    .unwrap_or(""),
            };
            method.eq_ignore_ascii_case("delete")
        }),
        "gh" => first == "repo" && pos.get(1) == Some(&"delete"),
        "aws" => pos.get(1).is_some_and(|op| {
            op.starts_with("delete")
                || op.starts_with("terminate")
                || (first == "s3" && matches!(*op, "rb" | "rm"))
        }),
        "vercel" | "vc" => matches!(first, "rm" | "remove"),
        "fly" | "flyctl" => pos.contains(&"destroy"),
        "heroku" => pos.iter().any(|p| p.ends_with("destroy")),
        "kubectl" => first == "delete",
        "terraform" | "tofu" => first == "destroy",
        "gcloud" => pos.contains(&"delete"),
        _ => false,
    };
    if !hit {
        return None;
    }
    let spelled = std::iter::once(cmd.name())
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    if env
        .r5_allow
        .iter()
        .any(|a| !a.is_empty() && spelled.starts_with(a.as_str()))
    {
        return None;
    }
    Some(Hit {
        rule: "R5",
        fact: argv_fact(cmd),
    })
}

/// The logged fact for a command; never an argument value.
///
/// Why: an argument can carry a credential (`https://u:tok@host/..`,
/// `--token=X`, `-H "Authorization: .."`), and the log must hold none
/// (#11431 review).
/// What: `argv0`; `subcommand`, the first bare lowercase word among the
/// first two positionals (`push`, `api`, `delete`); flag NAMES only, long
/// flags cut at `=` and short flags at two characters (`-XDELETE` → `-X`);
/// and the count of positional words. Rules add their own derived facts
/// (`target`, `branch`, repo slugs).
/// Test: `the_log_holds_no_argument_values`.
pub(crate) fn argv_fact(cmd: &Cmd) -> Value {
    let mut flags = Vec::new();
    let mut positional = Vec::new();
    for word in cmd.argv.iter().skip(1) {
        match word.lit() {
            Some(f) if f.starts_with("--") && f.len() > 2 => {
                flags.push(f.split('=').next().unwrap_or(f).to_owned());
            }
            Some(f) if f.starts_with('-') && f.len() > 1 => flags.push(f.chars().take(2).collect()),
            other => positional.push(other),
        }
    }
    let bare = |w: &&Option<&str>| {
        w.is_some_and(|s| {
            !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_lowercase() || c == '-')
        })
    };
    let subcommand = positional.iter().take(2).find(bare).copied().flatten();
    json!({
        "argv0": cmd.name(),
        "subcommand": subcommand,
        "flags": flags,
        "positional_count": positional.len(),
        "unparsed": cmd.unparsed,
    })
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod tests;
