//! #8439: a read-only dispatch runs only allowlisted read shapes. Every probe
//! the round-3 critic reproduced against the first cut is a deny row here.

use std::time::{Duration, Instant};

use super::{READ_ONLY_DISPATCH_AGENTS, evaluate_read_only_dispatch_command, judge};
use crate::commands::pm_guard_bash::worktree_remove::DispatchIdentity;

fn run(agent: Option<&'static str>, command: &str) -> Option<String> {
    let identity = DispatchIdentity {
        agent_id: agent.map(|_| "agent-abc123"),
        agent_type: agent,
    };
    evaluate_read_only_dispatch_command(command, identity)
}

/// Every command in `rows` gets `want` (true = allowed); all misses at once.
fn check(want_allow: bool, rows: &[&str]) {
    let wrong: Vec<String> = rows
        .iter()
        .filter_map(|c| {
            let got = judge(c);
            (got.is_ok() != want_allow).then(|| format!("{c:?} -> {got:?}"))
        })
        .collect();
    let verdict = if want_allow { "allowed" } else { "refused" };
    assert!(
        wrong.is_empty(),
        "expected {verdict}:\n{}",
        wrong.join("\n")
    );
}

/// 🔴 REGRESSION (#8439): the exact incident form is refused to every bound
/// agent. Allowed on origin/main, which has no such rule.
#[test]
fn refuses_the_incident_plutil_extract_json_form() {
    let incident = "plutil -extract ProgramArguments json /Users/masa/Library/LaunchAgents/a.plist";
    for agent in READ_ONLY_DISPATCH_AGENTS {
        let reason = run(Some(agent), incident).expect("the incident form must be refused");
        assert!(
            reason.contains("#8439") && reason.contains(agent),
            "{reason}"
        );
    }
}

/// #8439: only the six read-only roles are bound; ops agents and the PM are not.
#[test]
fn only_the_read_only_agents_are_bound() {
    for agent in [
        None,
        Some("local-ops"),
        Some("version-control"),
        Some("rust-engineer"),
        Some("RESEARCH"),
    ] {
        assert_eq!(run(agent, "sed -i s/a/b/ f.txt"), None, "{agent:?}");
    }
}

/// #8439: the PM brief's reads, and the old branch's genuine read cases.
#[test]
fn legitimate_reads_stay_allowed() {
    check(
        true,
        &[
            "git status",
            "git status --short",
            "git log --oneline -5",
            "git diff HEAD~1",
            "git diff --stat",
            "git diff origin/main -- crates",
            "git show HEAD:Cargo.toml | grep -n version",
            "git grep -n unwrap -- crates",
            "git rev-parse --show-toplevel",
            "git ls-files crates",
            "git merge-base HEAD origin/main",
            "git ls-remote origin",
            "git branch --list 'fix/*'",
            "git branch -a",
            "git branch --show-current",
            "git worktree list --porcelain",
            "git -C /repo log -1",
            "git --no-pager log -3",
            "git log --format='%h %s' -3 2>&1",
            "git log --oneline | head -3",
            "tmux capture-pane -p -t s:0 | grep -n error",
            "tmux capture-pane -pt main -S -200 | grep -E 'fail|error' | tail -5",
            "plutil -p /Users/masa/Library/LaunchAgents/x.plist",
            "plutil -lint a.plist",
            "sed -n 1,20p Cargo.toml",
            "sed -n '1,5p' f.txt",
            "sed -n '/-i/p' f.txt",
            "sed -n -e 1p -e '$p' f.txt",
            "defaults read com.apple.dock",
            "defaults read",
            "launchctl print gui/501",
            "launchctl list",
            "cat Cargo.toml | grep -c x",
            "head -20 f.txt",
            "tail -n 5 f.txt",
            "wc -l f.txt",
            "ls -la /tmp 2>/dev/null",
            "ls -i",
            "grep -rn foo crates",
            "grep -i foo f.txt",
            "rg -n foo crates | head -5",
            "rg -i foo",
            "find . -name '*.plist' -type f",
            "find /Users/masa/Library/LaunchAgents -name x.plist -print",
            "if git diff --quiet; then echo clean; fi",
            "if git diff --quiet; then echo clean; else echo dirty; fi",
            "for f in /Users/masa/Library/LaunchAgents/*.plist; do plutil -p \"$f\"; done",
            "for f in ~/Library/LaunchAgents/*.plist; do plutil -p \"$f\"; done",
            "for f in a.txt b.txt; do head -1 \"${f}\"; done",
            "cargo metadata --no-deps --format-version 1",
            "cargo tree -p trusty-mpm",
            "echo hi",
            "pwd",
        ],
    );
}

/// #8439: every probe from the round-3 critic (p1–p7) that writes, and every
/// construct the brief names, is refused.
#[test]
fn critic_round_three_probes_are_refused() {
    check(
        false,
        &[
            // CRITICAL: non-ASCII / CR whitespace used to spin the lexer.
            "sed -i s/a/b/ Cargo.toml ;\r",
            "ls \r",
            "ls ;\r",
            "ls \u{a0}",
            "ls \u{b}",
            "ls \u{c}",
            // HIGH: checkout paths judged on unexpanded text.
            "git checkout ~/x/Cargo.toml",
            "for f in Cargo.toml; do git checkout \"$f\"; done",
            "git checkout ':/Cargo.toml'",
            "git checkout ':!crates'",
            "git checkout ':^crates'",
            "git checkout ':(top)Cargo.toml'",
            "git checkout deleted.txt",
            "git checkout Cargo.toml",
            "git checkout -- Cargo.toml",
            "git checkout main",
            "git restore Cargo.toml",
            "git -C crates checkout Cargo.toml",
            "git switch --orphan x",
            // HIGH: unknown git global options.
            "git --attr-source status checkout -- Cargo.toml",
            "git --attr-source=status checkout -- Cargo.toml",
            "git --shallow-file status checkout -- Cargo.toml",
            "git --shallow-file log reset --hard",
            // HIGH: a for-var's text reaching git.
            "for x in expire; do git reflog \"$x\" --expire=now --all; done",
            "for r in +main:main; do git fetch origin \"$r\"; done",
            // HIGH: `#` inside `$(`.
            "echo $(echo # ) '\nsed -i s/a/b/ Cargo.toml )\necho \\'",
            // HIGH: zsh `${(e)X}` in a heredoc.
            "cat <<EOF\n${(e)X}\nEOF",
            // HIGH: sudo flag values.
            "sudo -p lv sed -i s/a/b/ Cargo.toml",
            "sudo -n -p Vl sed -i s/a/b/ Cargo.toml",
            "sudo sed -i s/a/b/ Cargo.toml",
            // MEDIUM: for-var reassignment.
            "echo ${f::=-i}",
            "for f in a; do echo \"${f:=x}\"; done",
            // p1/p2 writes.
            "plutil -extract ProgramArguments json a.plist",
            "sed -i s/a/b/ tracked.txt",
            "echo $(sed -i s/a/b/ tracked.txt)",
            "echo `sed -i s/a/b/ tracked.txt`",
            "echo hi > tracked.txt",
            "echo hi >> tracked.txt",
            "echo hi >| tracked.txt",
            "echo hi &> tracked.txt",
            "echo hi >&tracked.txt",
            "cat <> tracked.txt",
            "echo hi 2>tracked.txt",
            "exec 3>tracked.txt",
            "echo hi | tee tracked.txt",
            "ls; sed -i s/a/b/ tracked.txt",
            "ls && sed -i s/a/b/ tracked.txt",
            "ls || sed -i s/a/b/ tracked.txt",
            "ls | sed -i s/a/b/ tracked.txt",
            "FOO=1 sed -i s/a/b/ tracked.txt",
            "GIT_DIR=/tmp git status",
            "echo tracked.txt | xargs sed -i s/a/b/",
            "find . -name tracked.txt -exec sed -i s/a/b/ {} \\;",
            "find . -delete",
            "find . -fprint /tmp/x",
            "sh -c 'sed -i s/a/b/ tracked.txt'",
            "bash -c \"echo hi > tracked.txt\"",
            "env -S 'sed -i s/a/b/ tracked.txt'",
            "timeout 5 sed -i s/a/b/ tracked.txt",
            "\"s\"ed -i s/a/b/ tracked.txt",
            "s\\ed -i s/a/b/ tracked.txt",
            "/usr/bin/sed -i s/a/b/ tracked.txt",
            "$X -i s/a/b/ tracked.txt",
            "sed $'-i' s/a/b/ tracked.txt",
            "sed \"-i\" s/a/b/ tracked.txt",
            "for s in 'w /etc/owned'; do sed -n \"$s\" tracked.txt; done",
            "sed -n '1w /tmp/x' f.txt",
            "sed 's/a/b/w /tmp/x' f.txt",
            "git format-patch -1",
            "git diff --output=/tmp/d.diff",
            "git diff --outp=/tmp/d.diff",
            "git grep -Ovim foo",
            "git grep --open foo",
            "git ls-remote --upload-pack=x origin",
            "git ls-remote -u x origin",
            "git -c core.pager=x log",
            "git fetch origin",
            "git branch topic",
            "git branch -D topic",
            "git worktree add ../x",
            "case x in x) sed -i s/a/b/ tracked.txt;; esac",
            "sed -i s/a/b/ tracked.txt &",
            "eval 'sed -i s/a/b/ tracked.txt'",
            "cd /tmp && echo hi > tracked.txt",
            "echo hi > /tmp/scratch-ok.txt",
            "( ls )",
            "{ ls; }",
            "ls # > x",
            "ls *.txt",
            "ls {a,b}",
            "plutil -p ~/x.plist",
            "rg --pre cat foo",
            "tmux capture-pane -t s:0",
            "tmux capture-pane -p -b buf",
            "cargo test -p trusty-mpm 2>&1 | tail -5",
            "cargo --config x tree",
            "awk '{print}' Cargo.toml | head",
            "defaults write com.x k v",
            "launchctl bootout gui/501/x",
            "echo \"$(git rev-parse HEAD)\"",
            "if git diff --quiet; then sed -i s/a/b/ f; fi",
            "for f in /a/*.plist; do plutil -extract K json \"$f\"; done",
            "for f in -rf; do ls \"$f\"; done",
            "for f in *.plist; do plutil -p \"$f\"; done",
            "ls \"$HOME\"",
            "=sed -n p f",
            "ls | xargs cat",
            "",
        ],
    );
}

/// #8439 CRITICAL: the lexer terminates on every input. Arbitrary bytes —
/// every ASCII control byte, every Unicode whitespace class, random mixes —
/// each finish well inside the budget, and none outside the alphabet allows.
#[test]
fn lexing_terminates_on_arbitrary_bytes() {
    let started = Instant::now();
    let whitespace = [
        '\u{9}', '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{20}', '\u{85}', '\u{a0}', '\u{1680}',
        '\u{2000}', '\u{2007}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}',
        '\u{3000}', '\u{feff}', '\u{0}', '\u{7f}',
    ];
    for ws in whitespace {
        for shape in [
            "ls {ws}",
            "{ws}",
            "sed -i s/a/b/ f ;{ws}",
            "'{ws}'",
            "\"{ws}\" x",
        ] {
            let command = shape.replace("{ws}", &ws.to_string());
            let verdict = judge(&command);
            if !matches!(ws, '\u{9}' | '\u{a}' | '\u{20}') {
                assert!(verdict.is_err(), "{command:?} must be refused");
            }
        }
    }
    // xorshift: deterministic pseudo-random byte strings over the whole range.
    let mut state: u64 = 0x8439_8439_dead_beef;
    for _ in 0..20_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let len = (state % 64) as usize;
        let bytes: Vec<u8> = (0..len)
            .map(|k| (state.rotate_left(k as u32 * 5) & 0xff) as u8)
            .collect();
        let _ = judge(&String::from_utf8_lossy(&bytes));
    }
    // A long command of every accepted operator repeated also terminates.
    let _ = judge(&"ls | ; \"a\" 'b' 2>&1 ~/x ".repeat(4096));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "lexing took {:?}",
        started.elapsed()
    );
}

/// #8439: each byte outside the lexer's alphabet is refused on its own.
#[test]
fn lexer_refuses_every_byte_outside_its_alphabet() {
    for b in (0u8..=0x7f).filter(|b| !(b.is_ascii_graphic() || *b == b' ')) {
        if matches!(b, b'\t' | b'\n') {
            continue;
        }
        let command = format!("ls {}", char::from(b));
        assert!(judge(&command).is_err(), "byte 0x{b:02x} must be refused");
    }
    for c in [
        '$', '`', '\\', '<', '>', '&', '(', ')', '{', '}', '[', ']', '#', '!',
    ] {
        assert!(
            judge(&format!("ls a{c}b")).is_err(),
            "{c:?} must be refused"
        );
    }
}

/// #8439: git reads pass; anything that moves a ref or writes is refused.
#[test]
fn git_reads_pass_and_everything_else_is_refused() {
    check(
        true,
        &[
            "git log --output-indicator-new=+ -p",
            "git ls-files --exclude-standard -o",
            "git branch --merged main",
            "git branch --list --contains HEAD 'fix/*'",
            "git -P diff",
        ],
    );
    check(
        false,
        &[
            "git",
            "git -C",
            "git stash",
            "git reset --hard",
            "git clean -fdx",
            "git commit -m x",
            "git apply p.diff",
            "git config user.name x",
            "git tag v9",
            "git switch main",
            "git pull",
            "git --git-dir=/x status",
            "git --exec-path=/tmp log",
            "git worktree remove x",
            "git branch -m a b",
        ],
    );
}

/// #8439 round 2 CRITICAL 1: git reads `--` as the value of `-e`, so the
/// option scan never stops there; a pathspec spelled like an option is refused.
#[test]
fn git_options_after_a_double_dash_are_still_judged() {
    check(
        false,
        &[
            "git grep -e -- -O",
            "git grep -e -- --open-files-in-pager",
            "git grep -e -- -Ovim foo",
            "git log -- --output=/tmp/x",
            "git diff --ext-diff",
            "git log --textconv -p",
            "git show --show-signature",
            "git log --show-sig",
            "git ls-remote 'ext::sh -c x'",
            "git log --format=%G?",
            "git show -s '--pretty=format:%GS'",
            "git branch --format='%(signature)'",
            "git branch --format='%(*signature)'",
            "git log -1 '--format=%-G?'",
            "git log -1 '--format=% G?'",
            "git show -s --pretty=tformat:%+GS HEAD",
            "git log '--format=%+G?'",
            "git log --pretty 'format:%-GK'",
        ],
    );
    check(
        true,
        &[
            "git log -- -p",
            "git grep -e -- -n",
            "git diff -- crates",
            "git log --no-ext-diff -p",
        ],
    );
}

/// #8439 round 2 CRITICAL 2: BSD `sed` takes the first operand as the script
/// when no `-e` came first; the guard judges that order.
#[test]
fn sed_is_judged_in_the_order_bsd_sed_reads_it() {
    check(
        false,
        &[
            "for f in 1p; do sed -n \"$f\" -e 1p x.txt; done",
            "sed -n 2p -e 1p two.txt",
            "sed -n f.txt -e 1p",
            "sed -n",
            "sed -n 1p f.txt -i",
            "for f in a.txt; do sed -n \"$f\"; done",
        ],
    );
    check(
        true,
        &[
            "sed -n -e 1p f.txt",
            "sed -nE 1,3p f.txt g.txt",
            "for f in a.txt; do sed -n 1p \"$f\"; done",
        ],
    );
}

/// #8439 round 2 CRITICAL 3: tmux format-expands `-S`/`-E`; values stay plain.
#[test]
fn tmux_values_cannot_carry_a_format() {
    check(
        false,
        &[
            "tmux capture-pane -p -S '#{e|+:1,1}'",
            "tmux capture-pane -p -E '#(touch /tmp/x)'",
            "tmux capture-pane -p -t '#{session_name}'",
            "tmux capture-pane -p -S1x",
            "tmux capture-pane -p -t 'a b'",
            "tmux capture-pane -p -S",
        ],
    );
    check(
        true,
        &[
            "tmux capture-pane -p -S - -E -",
            "tmux capture-pane -p -S-200 -t main:0.1",
            "tmux capture-pane -pJ -t %3",
        ],
    );
}

/// #8439 round 2 MEDIUM/LOW: loop names are fixed; `tail` never follows.
#[test]
fn loop_names_are_fixed_and_tail_never_follows() {
    check(
        false,
        &[
            "for HOME in /tmp; do git status; done",
            "for PATH in /tmp; do ls; done",
            "for path in /tmp; do ls; done",
            "for IFS in a; do ls; done",
            "tail -f f.txt",
            "tail -F f.txt",
            "tail -5f f.txt",
            "tail --fol f.txt",
            "git log | tail -f",
        ],
    );
    check(
        true,
        &[
            "for file in a.txt; do ls \"$file\"; done",
            "tail -n 5 f.txt",
        ],
    );
}

/// #8439 round 3 HIGH: a `for` variable's text is unknown, so it is refused
/// wherever that text would matter — both forms the critic ran, plus each
/// other position the audit closed.
#[test]
fn a_for_variable_never_reaches_a_git_value_or_remote() {
    check(
        false,
        &[
            "for x in '%(signature)'; do git branch --format \"$x\"; done",
            "for x in 'foo::bar'; do git ls-remote \"$x\"; done",
            "for f in /a/*.git; do git ls-remote \"$f\"; done",
            "for x in '%G?'; do git log --format \"$x\"; done",
            "for x in a; do git log --grep -- --format \"$x\"; done",
            "for x in a; do git show --pretty \"$x\"; done",
            "for x in a; do git grep -e \"$x\"; done",
            "for x in a; do git branch --contains \"$x\"; done",
            "for x in a; do git branch --sort \"$x\" --list; done",
            "for x in a; do git -C \"$x\" status; done",
            "for x in a; do git worktree list \"$x\"; done",
            "for x in a; do sed -n -e \"$x\" f; done",
            "for x in a; do find . -name \"$x\"; done",
            "for x in a; do tmux capture-pane -p -t \"$x\"; done",
            "for x in a; do cargo tree -p \"$x\"; done",
        ],
    );
    check(
        true,
        &[
            "for f in a.txt; do git log --oneline -- \"$f\"; done",
            "for f in a.txt; do git diff \"$f\"; done",
            "for p in 'fix/*'; do git branch --list \"$p\"; done",
        ],
    );
}

/// The 2026-09-25 live refusal: a `security` agent scanning another worktree.
const LIVE_CD_SCAN: &str = "cd /Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/\
                            .claude/worktrees/agent-a1 && git diff c5e9a7d2c..HEAD";

/// 🔴 REGRESSION (#8578): one leading `cd <plain dir> &&` before an allowed
/// read is allowed. Refused on origin/main, whose lexer refuses every `&`.
#[test]
fn a_leading_cd_reaches_another_worktree() {
    for agent in READ_ONLY_DISPATCH_AGENTS {
        assert_eq!(run(Some(agent), LIVE_CD_SCAN), None, "{agent}");
    }
    check(
        true,
        &[
            "cd /repo && git log --oneline -5",
            "cd /repo && git show abc123 --stat",
            "cd crates/trusty-mpm && rg -n foo src | head -5",
            "cd ../other-wt && git status --short 2>&1",
            "cd '/path with space' && git rev-parse HEAD",
            "cd /repo && if git diff --quiet; then echo clean; fi",
            "cd /repo && for f in a.txt b.txt; do head -1 \"$f\"; done",
        ],
    );
}

/// #8578: `git -C <dir> <verb> <args>` is judged exactly as `git <verb> <args>`,
/// for reads and for every mutating verb the brief names.
#[test]
fn git_dash_c_is_judged_as_its_plain_form() {
    let reads = [
        "diff A..B",
        "log",
        "log --oneline -5",
        "show abc123",
        "rev-parse",
        "rev-parse --show-toplevel",
        "status --short",
    ];
    let writes = [
        "commit -m x",
        "push origin HEAD",
        "reset --hard",
        "checkout main",
        "stash",
        "rebase origin/main",
        "merge topic",
        "diff --output=/tmp/d.diff",
    ];
    for (tails, allowed) in [(&reads[..], true), (&writes[..], false)] {
        for tail in tails {
            let plain = judge(&format!("git {tail}")).is_ok();
            assert_eq!(plain, allowed, "git {tail}");
            for dir in ["/Users/masa/wt/agent-a1", "crates", "'/a b'"] {
                let with_c = format!("git -C {dir} {tail}");
                assert_eq!(judge(&with_c).is_ok(), plain, "{with_c}");
            }
        }
    }
}

/// #8578: the `cd` prefix never admits a write, an unresolved directory, or
/// any other `&&`/`;`/`|` chain.
#[test]
fn a_leading_cd_never_admits_a_write() {
    check(
        false,
        &[
            "cd /repo && rm -rf target",
            "cd /repo && git diff > /tmp/d.diff",
            "cd /repo && git diff | tee /tmp/d.diff",
            "cd /repo && git commit -m x",
            "cd /repo && git -C /other push",
            "cd /repo && sed -i s/a/b/ f.txt",
            "cd /repo && for f in a; do rm \"$f\"; done",
            "cd /repo && git diff && rm f",
            "cd /repo && cd /other && git diff",
            "git diff && cd /repo",
            "if cd /repo && git diff; then echo x; fi",
            "cd /repo; git diff",
            "cd /repo || git diff",
            "cd /repo & git diff",
            "cd /repo &&& git diff",
            "cd /repo&&git diff",
            "cd /repo &&\ngit diff",
            "cd /repo &&",
            "cd && git diff",
            "cd - && git diff",
            "cd -P /repo && git diff",
            "cd /repo /x && git diff",
            // Unresolved directories (path_tokens::unresolved_target).
            "cd $X && git diff",
            "cd \"$X\" && git diff",
            "cd '$X' && git diff",
            "cd ~/wt && git diff",
            "cd '~/wt' && git diff",
            "cd '$(cat /tmp/main)' && git diff",
            "cd '`pwd`' && git diff",
            "git -C $X diff",
            "git -C \"$X\" diff",
            "git -C ~/wt diff",
        ],
    );
}

/// 🔴 REGRESSION (#8567): the GitHub reads a research or critic brief opens
/// with. Refused on origin/main, where `gh` is off the allowlist.
#[test]
fn gh_read_verbs_are_allowed() {
    for agent in READ_ONLY_DISPATCH_AGENTS {
        let got = run(Some(agent), "gh issue view 8567 --comments");
        assert_eq!(got, None, "{agent}");
    }
    check(
        true,
        &[
            "gh issue view 8567 --comments",
            "gh issue view 8567 --repo bobmatnyc/trusty-tools --json title,body",
            "gh issue list --state open --label bug --limit 20",
            "gh pr view 8604 --json state,mergeable,statusCheckRollup",
            "gh pr list --search 'read-only allowlist' --state all",
            "gh pr diff 8604 --name-only",
            "gh pr checks 8604",
            "gh run view 123456 --log-failed | tail -50",
            "gh run list --branch main --limit 5",
            "gh issue view 8567 --json comments --jq '.comments[].body' | head -40",
            "cd /repo && gh pr view 8604",
            "for n in 8567 8586; do gh issue view \"$n\"; done",
        ],
    );
}

/// #8567: every mutating verb, every verb not named, and every read verb in a
/// form that opens a browser, blocks on CI or writes stays refused.
#[test]
fn gh_mutating_and_unknown_verbs_are_refused() {
    check(
        false,
        &[
            "gh pr create --title x --body y",
            "gh pr merge 1 --squash",
            "gh pr review 1 --comment -b ok",
            "gh pr review 1 --approve",
            "gh pr comment 1 --body x",
            "gh pr edit 1 --add-label x",
            "gh pr checkout 1",
            "gh pr close 1",
            "gh issue create --title x",
            "gh issue comment 1 --body x",
            "gh issue edit 1 --add-label x",
            "gh issue close 1",
            "gh issue delete 1",
            "gh run rerun 1",
            "gh run cancel 1",
            "gh run download 1",
            "gh run watch 1",
            "gh repo delete o/r --yes",
            "gh release create v1",
            "gh auth token",
            "gh secret list",
            "gh alias set x 'pr merge'",
            "gh extension install o/r",
            "gh issue",
            "gh",
            "gh -R o/r pr view 1",
            "gh pr ls",
            "gh pr view 1 --web",
            "gh issue list -w",
            "gh pr checks 1 --watch",
            "gh pr view 1 > /tmp/pr.txt",
            "gh pr view 1 | tee /tmp/pr.txt",
            "GH_CONFIG_DIR=/tmp/gh gh pr view 1",
            "gh pr view 1 && gh pr merge 1",
            "ls | gh issue create --body-file -",
            "for v in create; do gh pr \"$v\"; done",
        ],
    );
}

/// 🔴 REGRESSION (#8567): `gh api` reads that send GET. Refused on origin/main.
#[test]
fn gh_api_get_forms_are_allowed() {
    check(
        true,
        &[
            "gh api repos/bobmatnyc/trusty-tools/issues/8567",
            "gh api /repos/o/r/pulls/1/comments --paginate --jq '.[].body'",
            "gh api -X GET search/issues",
            "gh api --method GET repos/o/r",
            "gh api --method=GET repos/o/r",
            "gh api -XGET repos/o/r",
            "gh api repos/o/r/commits/abc/check-runs -q '.check_runs[].conclusion'",
            "gh api -H 'Accept: application/vnd.github+json' \
             -H 'X-GitHub-Api-Version: 2022-11-28' repos/o/r",
            "gh api -i --silent repos/o/r",
            "gh api 'repos/{owner}/{repo}/pulls?state=open' --paginate --slurp",
            "gh api --hostname github.com user",
            "gh api repos/o/r/actions/runs -t '{{range .workflow_runs}}{{.id}}{{end}}'",
        ],
    );
}

/// #8567: a `gh api` call that could write — another method, a request body,
/// GraphQL, a method-override header, or an option not named — is refused.
#[test]
fn gh_api_writes_are_refused() {
    check(
        false,
        &[
            "gh api -X POST repos/o/r/issues -f title=x",
            "gh api -X PATCH repos/o/r/issues/1 -f state=closed",
            "gh api -X PUT repos/o/r/pulls/1/merge",
            "gh api -X DELETE repos/o/r/git/refs/heads/x",
            "gh api -XPOST repos/o/r/issues",
            "gh api --method POST repos/o/r/issues",
            "gh api --method=DELETE repos/o/r",
            "gh api repos/o/r/issues -f title=x",
            "gh api repos/o/r/issues -F title=x",
            "gh api repos/o/r/issues -ftitle=x",
            "gh api repos/o/r/issues --field title=x",
            "gh api repos/o/r/issues --raw-field title=x",
            "gh api repos/o/r/issues --input body.json",
            "gh api repos/o/r/issues --input=body.json",
            "gh api -X GET repos/o/r/issues -f title=x",
            "gh api graphql -f query='mutation { x }'",
            "gh api graphql",
            "gh api /graphql",
            "gh api -H 'X-HTTP-Method-Override: DELETE' repos/o/r",
            "gh api --cache 1h repos/o/r",
            "gh api -iX POST repos/o/r",
            "gh api -X",
            "gh api",
            "gh api repos/o/r repos/o/s",
            "for e in repos/o/r; do gh api \"$e\"; done",
        ],
    );
}

/// 🔴 REGRESSION (#8567): `date`, which BASE-AGENT asks every dispatch to run
/// first. Refused on origin/main.
#[test]
fn date_is_allowed() {
    for agent in READ_ONLY_DISPATCH_AGENTS {
        assert_eq!(run(Some(agent), "date"), None, "{agent}");
    }
    check(
        true,
        &[
            "date",
            "date +%s",
            "date -u +%Y-%m-%dT%H:%M:%SZ",
            "date '+%Y-%m-%d %H:%M:%S'",
            "date -r 0",
        ],
    );
}

/// #8567: `date` never writes through a redirect or a chained command.
#[test]
fn date_with_a_redirect_is_refused() {
    check(
        false,
        &[
            "date > /tmp/start",
            "date >> start.txt",
            "date | tee start.txt",
            "date 2>/tmp/err",
            "date; rm -f x",
            "date && touch x",
            "echo $(date)",
        ],
    );
}

/// 🔴 REGRESSION (#8586): a quoted rg/grep pattern keeps its `\` escapes, an
/// end-of-line `$` and glob characters as text. The double-quoted rows are
/// refused on origin/main; the single-quoted rows pin the issue's own forms.
#[test]
fn quoted_rg_grep_patterns_are_pattern_text() {
    check(
        true,
        &[
            r#"rg -n "fn \w+\(" crates"#,
            r#"grep -e "\(foo\|bar\)" f.txt"#,
            r#"grep -n "^\[workspace\.dependencies\]" Cargo.toml"#,
            r#"grep -n -A2 -B1 -E "^version = \"[0-9.]+\"$" Cargo.toml"#,
            r#"rg "foo$|bar$" src"#,
            r#"rg "(fn|struct) \w+$" src"#,
            r#"rg -n "\$HOME" docs"#,
            r#"rg "\`tm \w+\`" docs"#,
            r#"grep -rn "a\s+b" --include="*.rs" crates"#,
            r#"git log --oneline | grep "fix(\w+)""#,
            r#"rg -F "a\"b" src"#,
            r#"rg "a\" 'b" src"#,
            r#"cd /repo && rg -n "unwrap\(\)" crates"#,
            "rg -g '!*.test.ts' foo",
            r"grep -e '\(foo\)' f.txt",
            r"rg 'a\.b$' src",
        ],
    );
}

/// #8586: shell syntax stays refused — an expansion the shell performs inside
/// double quotes (`$(…)`, `${…}`, `$NAME`, backtick), any unquoted
/// metacharacter, an escape that would let a `"` close early, and pattern
/// text given to any program but rg/grep.
#[test]
fn shell_syntax_around_quoted_patterns_is_refused() {
    check(
        false,
        &[
            r#"rg "$(cat /tmp/x)" src"#,
            r#"grep "a$(id)b" f"#,
            r#"rg "a\\$(id)" f"#,
            r#"rg "a$|$(id)" f"#,
            r#"rg "${HOME}x" f"#,
            r#"rg "a$HOME" f"#,
            r#"rg "$[1+1]" f"#,
            r#"rg "a$'b'" f"#,
            r#"rg "`id`" f"#,
            r#"rg "a!b" f"#,
            r#"rg "a\!b" f"#,
            r#"rg "a$" $(id)"#,
            r#"rg "a\" 'b" f; rm -rf x #'"#,
            "rg \"a\\\nb\" f",
            r#"rg "a\"#,
            r"rg \( f",
            "rg a$ f",
            "rg foo; rm -rf x",
            "rg foo | tee out",
            r#"rg "foo\(" > out"#,
            r#"rg "foo\(" && rm f"#,
            r#"rg "foo\(" &"#,
            r#"ls | rg "\(" | tee out"#,
            "grep -r foo --include=*.rs .",
            r#"rg --pre "c\at" foo"#,
            r#"echo "a\(b""#,
            r#"cat "a\$b""#,
            r#"sed -n "/a$/p" f"#,
            r#"git log --grep="fix\(x\)""#,
            r#"git grep "a\(b""#,
            r#"for f in "a\$b"; do rg x "$f"; done"#,
            r#"cd "/tmp/a\$b" && rg x"#,
        ],
    );
}
