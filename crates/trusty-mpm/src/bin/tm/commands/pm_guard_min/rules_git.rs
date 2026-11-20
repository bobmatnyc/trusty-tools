//! Rule 2 (destructive git and recursive deletes of protected paths) and
//! rule 3 (cross-repo push) for the minimal pm-guard (#11431).
//!
//! Why: these are the actions a session cannot undo; each is judged on the
//! parsed argv and on git's own answers, never on the command text.
//! What: [`r2_destructive`] and [`r3_cross_repo`] take one parsed command;
//! [`git_probe`] asks git a read-only question with a 2 s bound.
//! Test: `rules_tests.rs` (`r2_*`, `r3_*`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use super::facts::{Cmd, Word};
use super::paths::{canon, hits_protected, resolve};
use super::rules::{GuardEnv, Hit, argv_fact};

/// Branches whose rewrite or deletion rule 2 flags.
const PROTECTED_BRANCHES: &[&str] = &["main", "master"];
/// The bound on one `git` probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Run `git -C <cwd> <args>` read-only; `None` on any failure or the timeout.
///
/// Why: rule 3 needs the remote URL and rule 2 the current branch; a hook
/// must never hang on a slow filesystem or a credential prompt.
/// What: stdin closed, stderr dropped, `GIT_TERMINAL_PROMPT=0`, killed after
/// [`PROBE_TIMEOUT`]. Returns trimmed stdout of a zero exit.
/// Test: `r3_flags_a_push_to_another_repo`.
pub(crate) fn git_probe(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if start.elapsed() > PROBE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out.trim().to_owned()).filter(|s| !s.is_empty())
}

/// A `git` command split into its effective cwd, subcommand and the rest.
fn git_parts(cmd: &Cmd) -> Option<(PathBuf, &str, &[Word])> {
    if cmd.name() != "git" {
        return None;
    }
    let mut cwd = cmd.cwd.clone();
    let mut i = 1;
    while let Some(w) = cmd.argv.get(i) {
        match w.lit()? {
            "-C" => {
                cwd = resolve(&cwd, cmd.argv.get(i + 1)?.lit()?);
                i += 2;
            }
            "-c" | "--git-dir" | "--work-tree" | "--namespace" => i += 2,
            flag if flag.starts_with('-') => i += 1,
            sub => return Some((cwd, sub, &cmd.argv[i + 1..])),
        }
    }
    None
}

/// Every short-flag letter and long flag in `words`.
fn flag_set(words: &[Word]) -> (String, Vec<&str>) {
    let mut short = String::new();
    let mut long = Vec::new();
    for w in words.iter().filter_map(Word::lit) {
        if let Some(l) = w.strip_prefix("--") {
            long.push(l);
        } else if let Some(s) = w.strip_prefix('-') {
            short.push_str(s);
        }
    }
    (short, long)
}

fn positionals(words: &[Word]) -> Vec<&str> {
    words
        .iter()
        .filter_map(Word::lit)
        .filter(|w| !w.starts_with('-'))
        .collect()
}

/// Rule 2: destructive git, discarding work, raw-disk writes, or `rm -r` /
/// `git worktree remove` of a protected path.
///
/// Why: brief §2 rule 2, widened by the owner's must-catch corpus (#11431):
/// each shape below destroyed or nearly destroyed real work. A dynamic word
/// is never guessed at, so an unparsed command is judged only on what is
/// literal.
/// What: `push` forcing a protected branch (`--force`, `-f`,
/// `--force-with-lease`, a `+` refspec; no refspec → the current branch);
/// `reset --hard`; `clean` with `f` and `d`; any `branch -D` (merged-PR
/// state is not known cheaply); any `worktree remove --force`, or one whose
/// target is protected; in a main checkout, `checkout ... -- <path>`,
/// `checkout .` and `restore` without `--staged`; `dd`/`gdd of=`, `wipefs`,
/// `gpt` on `/dev/(r)disk*` and `diskutil erase*`/`zeroDisk`/`partitionDisk`;
/// a recursive `rm` whose canonical target equals or contains `/`, the home
/// dir, the session repo or the main checkout.
/// Test: `r2_flags_each_destructive_shape`, `r2_leaves_everyday_git_alone`.
pub(crate) fn r2_destructive(cmd: &Cmd, env: &GuardEnv) -> Option<Hit> {
    let hit = |extra: Option<PathBuf>| {
        let mut fact = argv_fact(cmd);
        if let Some(t) = extra {
            fact["target"] = json!(t.display().to_string());
        }
        Some(Hit { rule: "R2", fact })
    };
    if raw_disk_write(cmd) {
        return hit(None);
    }
    if cmd.name() == "rm" {
        let (short, long) = flag_set(&cmd.argv[1..]);
        if !(short.contains(['r', 'R']) || long.contains(&"recursive")) {
            return None;
        }
        let protected = protected_paths(env, &cmd.cwd);
        return positionals(&cmd.argv[1..])
            .into_iter()
            .map(|t| resolve(&cmd.cwd, t))
            .find(|t| hits_protected(t, &protected))
            .and_then(|t| hit(Some(canon(&t))));
    }
    let (cwd, sub, rest) = git_parts(cmd)?;
    let (short, long) = flag_set(rest);
    let pos = positionals(rest);
    let destructive = match sub {
        "reset" => long.contains(&"hard"),
        "clean" => (short.contains('f') || long.contains(&"force")) && short.contains('d'),
        "branch" => {
            short.contains('D')
                || ((short.contains('d') || long.contains(&"delete"))
                    && (short.contains('f') || long.contains(&"force")))
        }
        "checkout" => {
            let discards = rest.iter().any(|w| w.lit() == Some("--")) || pos == ["."];
            discards && is_main_checkout(&cwd)
        }
        "restore" => !long.contains(&"staged") && !short.contains('S') && is_main_checkout(&cwd),
        "push" => {
            let branch = forced_protected_branch(rest, &cwd)?;
            let mut found = hit(None)?;
            found.fact["branch"] = json!(branch);
            return Some(found);
        }
        "worktree" if pos.first() == Some(&"remove") => {
            if short.contains('f') || long.contains(&"force") {
                return hit(None);
            }
            let protected = protected_paths(env, &cwd);
            if let Some(t) = pos
                .get(1)
                .map(|p| resolve(&cwd, p))
                .filter(|t| hits_protected(t, &protected))
            {
                return hit(Some(canon(&t)));
            }
            false
        }
        _ => false,
    };
    if destructive { hit(None) } else { None }
}

/// `cwd` is a repo's main checkout, not a linked worktree (git-dir == common-dir).
fn is_main_checkout(cwd: &Path) -> bool {
    let out = git_probe(
        cwd,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    );
    out.as_deref()
        .and_then(|o| o.split_once('\n'))
        .is_some_and(|(dir, common)| dir == common)
}

/// A raw-disk writer aimed at a `/dev/disk*` or `/dev/rdisk*` device.
fn raw_disk_write(cmd: &Cmd) -> bool {
    let args = cmd.lit_args();
    let is_disk = |a: &str| a.starts_with("/dev/disk") || a.starts_with("/dev/rdisk");
    match cmd.name() {
        "dd" | "gdd" => args
            .iter()
            .filter_map(|a| a.strip_prefix("of="))
            .any(is_disk),
        "wipefs" | "gpt" => args.iter().any(|a| is_disk(a)),
        "diskutil" => args.first().is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.starts_with("erase")
                || matches!(
                    v.as_str(),
                    "zerodisk" | "randomdisk" | "partitiondisk" | "secureerase"
                )
        }),
        _ => false,
    }
}

/// `git push` positionals: the remote, then refspecs; option values skipped.
fn push_positionals(rest: &[Word]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for w in rest {
        let Some(w) = w.lit() else { continue };
        if skip {
            skip = false;
        } else if matches!(
            w,
            "-o" | "--push-option" | "--repo" | "--receive-pack" | "--exec"
        ) {
            skip = true;
        } else if !w.starts_with('-') {
            out.push(w);
        }
    }
    out
}

/// The protected branch a forced push rewrites (`*` for `--all`/`--mirror`).
fn forced_protected_branch(rest: &[Word], cwd: &Path) -> Option<String> {
    let (short, long) = flag_set(rest);
    let refspecs = push_positionals(rest);
    let plus = refspecs.iter().skip(1).any(|r| r.starts_with('+'));
    let force = plus
        || short.contains('f')
        || long
            .iter()
            .any(|l| *l == "force" || l.starts_with("force-with-lease"));
    if !force {
        return None;
    }
    if long
        .iter()
        .any(|l| matches!(*l, "all" | "mirror" | "branches"))
    {
        return Some("*".to_owned());
    }
    let protected = |b: String| PROTECTED_BRANCHES.contains(&b.as_str()).then_some(b);
    let current = || git_probe(cwd, &["symbolic-ref", "--short", "-q", "HEAD"]).and_then(protected);
    if refspecs.len() <= 1 {
        return current();
    }
    refspecs[1..].iter().find_map(|r| {
        let r = r.trim_start_matches('+');
        let dst = r.rsplit_once(':').map_or(r, |(_, d)| d);
        let dst = dst.strip_prefix("refs/heads/").unwrap_or(dst);
        if dst == "HEAD" {
            current()
        } else {
            protected(dst.to_owned())
        }
    })
}

/// `/`, the home dir, the session repo and the main checkout of `cwd`'s repo.
fn protected_paths(env: &GuardEnv, cwd: &Path) -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/"), env.home.clone()];
    out.extend(env.project_dir.clone());
    let common = git_probe(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    if let Some(parent) = common
        .as_deref()
        .map(Path::new)
        .filter(|p| p.ends_with(".git"))
        .and_then(Path::parent)
    {
        out.push(parent.to_path_buf());
    }
    out
}

/// Rule 3: a `git push` whose remote is not the session's repository.
///
/// Why: a push to another repo publishes work where the session has no mandate.
/// What: the remote is the first positional after option values (a URL, or
/// a name resolved with `git remote get-url`), default `origin`; a local
/// path is not a cross-repo push; the session repo is `origin` of
/// `CLAUDE_PROJECT_DIR` (else the payload cwd). Both normalise to
/// `owner/repo`; an unknown on either side is no fact, so no hit.
/// Test: `r3_flags_a_push_to_another_repo`, `r3_allows_a_push_to_the_session_repo`.
pub(crate) fn r3_cross_repo(cmd: &Cmd, env: &GuardEnv) -> Option<Hit> {
    let (cwd, sub, rest) = git_parts(cmd)?;
    if sub != "push" {
        return None;
    }
    let remote = push_positionals(rest).first().copied().unwrap_or("origin");
    // A local-path remote publishes nothing off this machine.
    if remote.starts_with(['/', '.', '~']) || remote.starts_with("file://") {
        return None;
    }
    let url = if remote.contains("://") || remote.contains(':') {
        remote.to_owned()
    } else {
        git_probe(&cwd, &["remote", "get-url", remote])?
    };
    let session_dir = env.project_dir.as_deref().unwrap_or(&env.cwd);
    let session = git_probe(session_dir, &["remote", "get-url", "origin"])?;
    let (pushed, own) = (repo_slug(&url), repo_slug(&session));
    if pushed.is_empty() || own.is_empty() || pushed == own {
        return None;
    }
    let mut fact = argv_fact(cmd);
    fact["remote_repo"] = json!(pushed);
    fact["session_repo"] = json!(own);
    Some(Hit { rule: "R3", fact })
}

/// `owner/repo` of a git URL, lowercased, without `.git`; userinfo never survives.
pub(crate) fn repo_slug(url: &str) -> String {
    // #11431: a query or fragment can carry a token; cut it first.
    let base = url.split(['?', '#']).next().unwrap_or("");
    let trimmed = base
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_lowercase();
    let parts: Vec<&str> = trimmed
        .split(['/', ':'])
        .filter(|p| !p.is_empty())
        .collect();
    match parts.as_slice() {
        [.., owner, repo] => format!("{owner}/{repo}"),
        _ => String::new(),
    }
}
