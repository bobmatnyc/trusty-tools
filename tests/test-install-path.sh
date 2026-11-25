#!/usr/bin/env bash
# tests/test-install-path.sh — unit tests for install.sh's PATH handling
# (#3874: write PATH idempotently to the file(s) each shell always sources;
# #3897 code-critic follow-up round 1 [WARN]: no PATH tripling in a normal
# login+interactive session, and idempotency detects pre-existing
# differently-quoted entries; round 2 [BLOCK]: the round-1 idempotency
# broadening was an unanchored substring match that silently matched
# commented-out lines and sibling *PATH variables, causing the REAL export
# line to be skipped while the script still exited reporting success).
#
# Why: install.sh previously wrote the PATH export only to `.zshrc`, which
# neither non-interactive (`ssh host cmd`) nor login-but-not-interactive zsh
# invocations source — only `.zshenv` is sourced unconditionally by every zsh
# invocation type. This left `~/.local/bin` off PATH for those shells,
# producing false "not found" preflight warnings and bogus verify results.
# A first fix wrote all three of .zshenv/.zprofile/.zshrc, but a normal
# login+interactive session (a fresh Terminal.app window) sources all three
# in ONE shell, tripling the install dir in PATH on a single fresh install
# (round 1 MEDIUM finding) — fixed by writing only `.zshenv` (sufficient on
# its own; sourced unconditionally). A round-1 second finding showed the
# idempotency check only recognised this script's own exact quoting style,
# so a hand-written differently-quoted PATH entry got a redundant second
# entry appended — fixed by broadening the match to `PATH=.*<dir>`. THAT
# broadening was itself unanchored (round 2 CRITICAL/BLOCK): it silently
# matched a commented-out line naming the dir, and matched `PATH=` as a
# substring of `MANPATH=`/`FPATH=`/etc. and the dir as a substring of a
# longer sibling directory — in both cases the REAL export was never
# written, and the script still exited 0. Fixed with a comment-stripping
# pre-pass plus a properly anchored (word-boundary on `PATH=`, delimiter
# boundary on both sides of the directory) match.
#
# What: sources install.sh with TRUSTY_INSTALL_SOURCE_ONLY=1 (skips the
# network `main` flow) inside a synthetic $HOME, then drives
# `maybe_update_path` directly under each detected-shell branch, asserting:
# - zsh: only .zshenv gets the export line (not .zprofile/.zshrc)
# - bash: both .bashrc/.bash_profile get the export line
# - repeat runs do not duplicate the export line in any file (idempotency)
# - a real login+interactive zsh sourcing every file install.sh could have
#   touched sees the install dir on PATH EXACTLY ONCE (the round-1 tripling
#   regression)
# - a pre-existing, differently-quoted PATH entry is recognised (no second
#   entry appended)
# - a pre-existing COMMENTED-OUT entry naming the dir does NOT block the
#   real export line from being written (round-2 trigger a)
# - a pre-existing sibling `MANPATH=` entry naming the dir does NOT block
#   the real `PATH=` line from being written (round-2 trigger b, part 1)
# - a pre-existing entry naming a longer directory that merely STARTS WITH
#   the install dir (`.../.local/binaries`) does NOT block the real
#   `.../.local/bin` export line from being written (round-2 trigger b,
#   part 2)
#
# #11280 additions: under `curl | sh` stdin is a pipe, and zsh users never
# got PATH set up; "already on PATH" was judged from the installer's
# inherited $PATH, which `zsh -c` does not share. These cases run with stdin
# from /dev/null and no ASSUME_YES, the `curl | sh` shape:
# - missing dir is added to .zshenv once, with no prompt, and resolves in
#   `zsh -il`, `zsh -i`, `zsh -l -c` and `zsh -c`
# - re-runs add nothing; a dir already in .zshenv (or via rustup's
#   ~/.cargo/env) gets no edit; a dir present only via .zshrc still gets the
#   .zshenv line; the probe never runs .zshrc
# - the line goes to $HOME/.zshenv even with ZDOTDIR exported (the .zshenv a
#   fresh zsh reads); a line a fresh zsh still misses gets a warning, not a
#   success line
# - TRUSTY_NO_MODIFY_PATH=1 makes no edit and starts no zsh
# - an unwritable .zshenv, read-only HOME or unwritable .bashrc is an error
#   with no "Added" line
# - a hanging .zshenv or a missing zsh falls back to adding the line
# - bash keeps its prompt-or-skip behaviour under a non-tty stdin
#
# #11430 additions: `tctl install` refused (exit 3) under `curl | sh`
# because its stdin was the script pipe. These cases run the whole `main`
# piped into `sh`, with a fake curl and a stub trusty-installer:
# - a controlling terminal (a pty from script(1)): the bootstrap runs
#   without -y and with the terminal on stdin; confirm() asks there too
# - no controlling terminal (a new session via perl setsid) and no -y:
#   exit 1 before any download, one error naming `| sh -s -- -y` and
#   TRUSTY_YES=1, bootstrap never run
# - CI=1 with a terminal and no -y: the same early stop (owner decision)
# - -y / TRUSTY_YES=1: `install -y` as before
# A skipped case is counted; any skip fails the suite unless ALLOW_SKIP=1.
#
# Every case runs against a temp HOME with ZDOTDIR and the TRUSTY_* knobs
# removed from the environment; the real $HOME is never touched.
#
# Test: This file *is* the test. Run with: tests/test-install-path.sh

# The fixtures write literal `$PATH` text into startup files on purpose.
# shellcheck disable=SC2016

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_SH="$REPO_ROOT/install.sh"

PASS=0
FAIL=0
SKIP=0

# skip <reason> — records a skipped test. #11430: skips are counted and turn
# the suite red unless ALLOW_SKIP=1, so a host that cannot run a case never
# reports green.
skip() {
    echo "  SKIP: $1"
    SKIP=$((SKIP + 1))
}

assert_eq() {
    local actual="$1" expected="$2" desc="$3"
    if [[ "$actual" == "$expected" ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc"
        echo "    expected: $expected"
        echo "    actual:   $actual"
        FAIL=$((FAIL + 1))
    fi
}

# count_matches <file> <needle> — occurrences of an exact-substring line.
count_matches() {
    local file="$1" needle="$2"
    [[ -f "$file" ]] || { echo 0; return; }
    # #11280: `grep -c` prints 0 AND exits 1 on no match; `|| echo 0` used to
    # print a second 0, so a zero count never compared equal to "0".
    local n
    n=$(grep -cF "$needle" "$file" 2>/dev/null) || true
    echo "${n:-0}"
}

# Environment knobs that must not leak from the developer's shell into a
# test: a real ZDOTDIR would point writes at the developer's own .zshenv.
# #11430: a CI runner's CI=true would make install.sh stop early.
HERMETIC_ENV=(env -u ZDOTDIR -u TRUSTY_NO_MODIFY_PATH -u TRUSTY_YES -u ASSUME_YES -u TRUSTY_INSTALL_DIR -u CI)

# Runs maybe_update_path("$FAKE_HOME/.local/bin") inside a fresh subshell
# with $HOME/$SHELL overridden, sourcing install.sh in source-only mode.
run_maybe_update_path() {
    local home="$1" shell_path="$2"
    "${HERMETIC_ENV[@]}" HOME="$home" SHELL="$shell_path" ASSUME_YES=1 TRUSTY_INSTALL_SOURCE_ONLY=1 \
        bash -c '. "$1"; maybe_update_path "$HOME/.local/bin"' _ "$INSTALL_SH"
}

# #11280: runs maybe_update_path "$dir" the way `curl | sh` does — stdin
# from /dev/null, no ASSUME_YES — under the shell named by RUNNER_SH
# (default sh). Extra `NAME=value` arguments go into the environment.
# PROBE_TIMEOUT, when set, lowers the zsh probe timeout after sourcing.
# Prints stdout+stderr; the exit status is maybe_update_path's.
run_curl_pipe() {
    local home="$1" shell_path="$2" dir="$3"
    shift 3
    "${HERMETIC_ENV[@]}" HOME="$home" SHELL="$shell_path" TRUSTY_INSTALL_SOURCE_ONLY=1 "$@" \
        "${RUNNER_SH:-sh}" -c '. "$1"; if [ -n "${PROBE_TIMEOUT:-}" ]; then PATH_PROBE_TIMEOUT_SECS="$PROBE_TIMEOUT"; fi; maybe_update_path "$2"' \
        _ "$INSTALL_SH" "$dir" </dev/null 2>&1
}

# new_home — a fresh temp HOME.
new_home() {
    mktemp -d -t install-path-test-XXXXXX
}

# file_sum <file> — content checksum, or "absent".
file_sum() {
    if [[ -f "$1" ]]; then cksum <"$1"; else echo absent; fi
}

# has_text <haystack> <needle> — "yes" when needle is a substring.
has_text() {
    if [[ "$1" == *"$2"* ]]; then echo yes; else echo no; fi
}

# zsh_mode_path <home> <flags...> — PATH a fresh zsh started with <flags>
# reports, from a clean environment like Terminal or sshd gives it.
zsh_mode_path() {
    local home="$1"
    shift
    env -i HOME="$home" PATH=/usr/bin:/bin:/usr/sbin:/sbin TERM=dumb \
        zsh "$@" -c 'print -r -- "$PATH"' </dev/null 2>/dev/null
}

test_zsh_writes_only_zshenv() {
    echo "--- test_zsh_writes_only_zshenv ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local needle="PATH='${home}/.local/bin'"
    assert_eq "$(count_matches "$home/.zshenv" "$needle")" "1" "zshenv gets the export line"
    assert_eq "$([[ -f "$home/.zprofile" ]] && echo yes || echo no)" "no" "zprofile is NOT written (would double-source with .zshenv)"
    assert_eq "$([[ -f "$home/.zshrc" ]] && echo yes || echo no)" "no" "zshrc is NOT written (would double-source with .zshenv)"

    rm -rf "$home"
}

test_bash_writes_both_files() {
    echo "--- test_bash_writes_both_files ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/bash" >/dev/null

    local needle="PATH='${home}/.local/bin'"
    assert_eq "$(count_matches "$home/.bashrc" "$needle")" "1" "bashrc gets the export line"
    assert_eq "$(count_matches "$home/.bash_profile" "$needle")" "1" "bash_profile gets the export line"

    rm -rf "$home"
}

test_repeat_install_is_idempotent() {
    echo "--- test_repeat_install_is_idempotent ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local needle="PATH='${home}/.local/bin'"
    assert_eq "$(count_matches "$home/.zshenv" "$needle")" "1" "zshenv has exactly one export line after 3 runs"

    rm -rf "$home"
}

# bash still trusts the installer's inherited $PATH (#11280 changes zsh only;
# for zsh see test_zshrc_only_dir_still_gets_zshenv_line).
test_bash_already_on_path_is_noop() {
    echo "--- test_bash_already_on_path_is_noop ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home/.local/bin"
    "${HERMETIC_ENV[@]}" HOME="$home" SHELL="/bin/bash" ASSUME_YES=1 TRUSTY_INSTALL_SOURCE_ONLY=1 \
        PATH="$home/.local/bin:$PATH" \
        bash -c '. "$1"; maybe_update_path "$HOME/.local/bin"' _ "$INSTALL_SH" >/dev/null

    assert_eq "$([[ -f "$home/.bashrc" ]] && echo yes || echo no)" "no" "no .bashrc created when dir already on PATH"
    assert_eq "$([[ -f "$home/.bash_profile" ]] && echo yes || echo no)" "no" "no .bash_profile created when dir already on PATH"

    rm -rf "$home"
}

# #3897 code-critic MEDIUM finding 1 regression test: a normal Terminal.app
# window is a login+interactive zsh shell, which sources .zshenv
# unconditionally, THEN .zprofile (it's a login shell), THEN .zshrc (it's
# interactive). If install.sh ever again writes the same export line to more
# than one of those files, this test catches the resulting PATH tripling
# directly (rather than asserting per-file line counts, which is exactly the
# coverage gap that let the original regression through).
test_login_interactive_zsh_sources_dir_exactly_once() {
    echo "--- test_login_interactive_zsh_sources_dir_exactly_once ---"
    if ! command -v zsh >/dev/null 2>&1; then
        skip "zsh not available on this host"
        return
    fi
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    # A real login+interactive zsh sources EVERY startup file it finds for
    # that invocation type (.zshenv always; .zprofile/.zshrc/.zlogin only if
    # present — none of the latter exist here after the fix, so zsh silently
    # skips them). ZDOTDIR pinned to the synthetic HOME so this is hermetic
    # regardless of the outer environment's zsh config.
    local result_path
    result_path=$(HOME="$home" ZDOTDIR="$home" zsh -l -i -c 'print -r -- $PATH' 2>/dev/null)
    local count
    count=$(printf '%s' "$result_path" | tr ':' '\n' | grep -cF "${home}/.local/bin")
    assert_eq "$count" "1" "install dir appears exactly once in PATH after a real login+interactive zsh sources every written file"

    rm -rf "$home"
}

# #3897 code-critic MEDIUM finding 2 regression test: a pre-existing
# hand-written PATH export in a different quoting style than this script's
# own must be recognised, not duplicated.
test_differently_quoted_existing_entry_is_recognised() {
    echo "--- test_differently_quoted_existing_entry_is_recognised ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home"
    # Simulate a user's own hand-written entry, double-quoted with a $HOME
    # reference expanded to the literal path (as a real shell would leave it
    # once written) — NOT this script's own single-quoted format.
    printf 'export PATH="%s/.local/bin:$PATH"\n' "$home" >"$home/.zshenv"

    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local occurrences
    occurrences=$(grep -cF "${home}/.local/bin" "$home/.zshenv")
    assert_eq "$occurrences" "1" "pre-existing differently-quoted entry is not duplicated"

    rm -rf "$home"
}

# #3897 code-critic BLOCK finding, trigger (a): a broadened idempotency
# match on a bare "PATH=.*<dir>" substring silently matched a COMMENTED-OUT
# line naming the dir, so `_append_path_export` thought a real export was
# already present and skipped writing one — leaving `.zshenv` with ZERO
# live `export PATH=` lines while the script exited reporting success. The
# real fix must strip full-line comments before matching, so a disabled
# entry never blocks the real write.
test_commented_out_entry_does_not_block_real_write() {
    echo "--- test_commented_out_entry_does_not_block_real_write ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home"
    printf '# export PATH="%s/.local/bin:$PATH"  (disabled)\n' "$home" >"$home/.zshenv"

    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local real_lines
    real_lines=$(grep -Ec "^[[:space:]]*export PATH=" "$home/.zshenv")
    assert_eq "$real_lines" "1" "a commented-out entry does not block the real export line from being written"

    rm -rf "$home"
}

# #3897 code-critic BLOCK finding, trigger (b) part 1: "PATH=" is a
# substring of MANPATH=/FPATH=/PYTHONPATH=/CLASSPATH=, so an unanchored
# match on a sibling *PATH variable naming the install dir also silently
# blocked the real PATH export from ever being written.
test_sibling_path_var_does_not_block_real_write() {
    echo "--- test_sibling_path_var_does_not_block_real_write ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home"
    printf 'export MANPATH="%s/.local/bin:$MANPATH"\n' "$home" >"$home/.zshenv"

    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local real_lines
    real_lines=$(grep -Ec "^[[:space:]]*export PATH=" "$home/.zshenv")
    assert_eq "$real_lines" "1" "a sibling MANPATH= entry naming the dir does not block the real PATH= line from being written"

    rm -rf "$home"
}

# #3897 code-critic BLOCK finding, trigger (b) part 2: with no right-hand
# word-boundary check, ".../.local/bin" matched inside ".../.local/binaries"
# — a different, longer directory that merely starts with the install dir's
# path — also silently blocking the real write.
test_dir_as_prefix_of_longer_path_does_not_block_real_write() {
    echo "--- test_dir_as_prefix_of_longer_path_does_not_block_real_write ---"
    local home
    home=$(mktemp -d -t install-path-test-XXXXXX)
    mkdir -p "$home"
    printf 'export PATH="%s/.local/binaries:$PATH"\n' "$home" >"$home/.zshenv"

    run_maybe_update_path "$home" "/bin/zsh" >/dev/null

    local real_lines
    real_lines=$(grep -Ec "^[[:space:]]*export PATH=" "$home/.zshenv")
    assert_eq "$real_lines" "2" "an unrelated .../.local/binaries entry does not block the real .../.local/bin export line from being written (2 total export lines: the pre-existing unrelated one + the new real one)"

    local dir_lines
    dir_lines=$(grep -cF "PATH='${home}/.local/bin'" "$home/.zshenv")
    assert_eq "$dir_lines" "1" "exactly one export line names the exact install dir (not the .../.local/binaries lookalike)"

    rm -rf "$home"
}

# ---------------------------------------------------------------------------
# #11280: `curl | sh` (stdin not a tty, no -y) for zsh users
# ---------------------------------------------------------------------------

# skip_without_zsh — prints a SKIP line and returns 1 when zsh is absent.
skip_without_zsh() {
    if ! command -v zsh >/dev/null 2>&1; then
        skip "zsh not available on this host"
        return 1
    fi
    return 0
}

test_zsh_missing_dir_is_added_without_prompt() {
    echo "--- test_zsh_missing_dir_is_added_without_prompt (RUNNER_SH=${RUNNER_SH:-sh}) ---"
    skip_without_zsh || return
    local home dir out st
    home=$(new_home)
    dir="$home/.cargo/bin"
    out=$(run_curl_pipe "$home" /bin/zsh "$dir")
    st=$?

    assert_eq "$st" "0" "maybe_update_path exits 0"
    assert_eq "$(count_matches "$home/.zshenv" "PATH='${dir}'")" "1" ".zshenv gets exactly one export line with no prompt"
    assert_eq "$(has_text "$out" "Added ${dir} to PATH in ${home}/.zshenv.")" "yes" "success line names the file"
    assert_eq "$(has_text "$out" "source ${home}/.zshenv")" "yes" "tells the user how to load PATH in this terminal"
    assert_eq "$(has_text "$out" "[y/N]")" "no" "no prompt is printed"
    assert_eq "$([[ -f "$home/.zshrc" || -f "$home/.zprofile" ]] && echo yes || echo no)" "no" ".zshrc/.zprofile are not written"

    rm -rf "$home"
}

# Criterion 1: after the write, all four zsh modes resolve the dir. On macOS
# the login modes also run /etc/zprofile's path_helper, which must keep it.
test_zsh_added_dir_resolves_in_every_mode() {
    echo "--- test_zsh_added_dir_resolves_in_every_mode ---"
    skip_without_zsh || return
    local home dir mode p
    home=$(new_home)
    dir="$home/.cargo/bin"

    for mode in "-il" "-i" "-l" ""; do
        p=$(zsh_mode_path "$home" ${mode:+"$mode"})
        assert_eq "$(has_text ":$p:" ":$dir:")" "no" "before: zsh ${mode:-(plain)} -c does not see the dir"
    done

    run_curl_pipe "$home" /bin/zsh "$dir" >/dev/null

    for mode in "-il" "-i" "-l" ""; do
        p=$(zsh_mode_path "$home" ${mode:+"$mode"})
        assert_eq "$(has_text ":$p:" ":$dir:")" "yes" "after: zsh ${mode:-(plain)} -c sees the dir"
    done

    rm -rf "$home"
}

test_curl_pipe_rerun_adds_no_duplicate() {
    echo "--- test_curl_pipe_rerun_adds_no_duplicate ---"
    skip_without_zsh || return
    local home dir sum1 out
    home=$(new_home)
    dir="$home/.cargo/bin"
    run_curl_pipe "$home" /bin/zsh "$dir" >/dev/null
    sum1=$(file_sum "$home/.zshenv")
    out=$(run_curl_pipe "$home" /bin/zsh "$dir")
    run_curl_pipe "$home" /bin/zsh "$dir" >/dev/null

    assert_eq "$(count_matches "$home/.zshenv" "PATH='${dir}'")" "1" "one export line after three runs"
    assert_eq "$(file_sum "$home/.zshenv")" "$sum1" ".zshenv is byte-identical after re-runs"
    assert_eq "$(has_text "$out" "already on PATH for new zsh shells")" "yes" "re-run reports the dir as already present"

    rm -rf "$home"
}

# Criterion 2: the installer inherited the dir from the interactive shell
# (.zshrc), but `zsh -c` does not read .zshrc — so .zshenv still gets it.
test_zshrc_only_dir_still_gets_zshenv_line() {
    echo "--- test_zshrc_only_dir_still_gets_zshenv_line ---"
    skip_without_zsh || return
    local home dir zshrc_sum
    home=$(new_home)
    dir="$home/.cargo/bin"
    printf 'export PATH="%s:$PATH"\n' "$dir" >"$home/.zshrc"
    zshrc_sum=$(file_sum "$home/.zshrc")

    run_curl_pipe "$home" /bin/zsh "$dir" "PATH=$dir:$PATH" >/dev/null

    assert_eq "$(count_matches "$home/.zshenv" "PATH='${dir}'")" "1" ".zshenv gets the line although the installer's PATH had the dir"
    assert_eq "$(file_sum "$home/.zshrc")" "$zshrc_sum" ".zshrc is left untouched"

    rm -rf "$home"
}

test_dir_already_in_zshenv_is_noop() {
    echo "--- test_dir_already_in_zshenv_is_noop ---"
    skip_without_zsh || return
    local home dir sum out
    home=$(new_home)
    dir="$home/.cargo/bin"
    printf 'export PATH="%s:$PATH"\n' "$dir" >"$home/.zshenv"
    sum=$(file_sum "$home/.zshenv")

    out=$(run_curl_pipe "$home" /bin/zsh "$dir")

    assert_eq "$(file_sum "$home/.zshenv")" "$sum" ".zshenv is not edited"
    assert_eq "$(has_text "$out" "Added")" "no" "no success line for a no-op"

    rm -rf "$home"
}

# rustup writes `. "$HOME/.cargo/env"` into .zshenv; a text match on .zshenv
# cannot see that, the probe can.
test_rustup_cargo_env_counts_as_present() {
    echo "--- test_rustup_cargo_env_counts_as_present ---"
    skip_without_zsh || return
    local home dir sum
    home=$(new_home)
    dir="$home/.cargo/bin"
    mkdir -p "$home/.cargo"
    printf 'case ":${PATH}:" in\n    *:"$HOME/.cargo/bin":*) ;;\n    *) export PATH="$HOME/.cargo/bin:$PATH" ;;\nesac\n' >"$home/.cargo/env"
    printf '. "$HOME/.cargo/env"\n' >"$home/.zshenv"
    sum=$(file_sum "$home/.zshenv")

    run_curl_pipe "$home" /bin/zsh "$dir" >/dev/null

    assert_eq "$(file_sum "$home/.zshenv")" "$sum" "a .zshenv sourcing ~/.cargo/env gets no extra line"

    rm -rf "$home"
}

# The probe must start a non-interactive, non-login zsh: .zshrc, .zprofile
# and .zlogin never run.
test_probe_does_not_run_zshrc() {
    echo "--- test_probe_does_not_run_zshrc ---"
    skip_without_zsh || return
    local home dir f
    home=$(new_home)
    dir="$home/.cargo/bin"
    for f in .zshrc .zprofile .zlogin; do
        printf ': >"$HOME/ran%s"\n' "$f" >"$home/$f"
    done

    run_curl_pipe "$home" /bin/zsh "$dir" >/dev/null

    for f in .zshrc .zprofile .zlogin; do
        assert_eq "$([[ -e "$home/ran$f" ]] && echo yes || echo no)" "no" "probe did not run $f"
    done

    rm -rf "$home"
}

# PM ruling on #11280 AC 3: the target is the .zshenv a fresh zsh reads.
# The common XDG layout exports ZDOTDIR from ~/.zshenv, so the installer
# inherits ZDOTDIR, yet zsh never reads $ZDOTDIR/.zshenv. The line must land
# in $HOME/.zshenv, and $ZDOTDIR/.zshenv must not be created.
test_exported_zdotdir_still_writes_home_zshenv() {
    echo "--- test_exported_zdotdir_still_writes_home_zshenv ---"
    skip_without_zsh || return
    local home zdot dir out st
    home=$(new_home)
    zdot="$home/zdot"
    mkdir -p "$zdot"
    dir="$home/.cargo/bin"
    printf 'export ZDOTDIR="%s"\n' "$zdot" >"$home/.zshenv"

    out=$(run_curl_pipe "$home" /bin/zsh "$dir" "ZDOTDIR=$zdot")
    st=$?

    assert_eq "$st" "0" "maybe_update_path exits 0"
    assert_eq "$(count_matches "$home/.zshenv" "PATH='${dir}'")" "1" "HOME/.zshenv gets one export line although ZDOTDIR is exported"
    assert_eq "$([[ -e "$zdot/.zshenv" ]] && echo yes || echo no)" "no" "\$ZDOTDIR/.zshenv is not created"
    assert_eq "$(has_text "$out" "Added ${dir} to PATH in ${home}/.zshenv.")" "yes" "success line names HOME/.zshenv"

    rm -rf "$home"
}

# The write succeeds but a fresh zsh still misses the dir (here a .zshenv
# that returns before the appended line; launchd-set ZDOTDIR is the other
# real case). The user gets a warning naming the causes, never "Added".
test_zshenv_line_not_taking_effect_warns() {
    echo "--- test_zshenv_line_not_taking_effect_warns ---"
    skip_without_zsh || return
    local home dir out st
    home=$(new_home)
    dir="$home/.cargo/bin"
    printf 'return 0\n' >"$home/.zshenv"

    out=$(run_curl_pipe "$home" /bin/zsh "$dir")
    st=$?

    assert_eq "$st" "1" "maybe_update_path exits 1"
    assert_eq "$(count_matches "$home/.zshenv" "PATH='${dir}'")" "1" "the line is written to HOME/.zshenv"
    assert_eq "$(has_text "$out" "Added")" "no" "no success line"
    assert_eq "$(has_text "$out" "a new zsh still does not have it on PATH")" "yes" "warning explains the miss"
    assert_eq "$(has_text "$out" "launchd")" "yes" "warning names the launchd ZDOTDIR case"

    rm -rf "$home"
}

test_no_modify_path_opt_out_is_noop() {
    echo "--- test_no_modify_path_opt_out_is_noop ---"
    local home dir out
    home=$(new_home)
    dir="$home/.cargo/bin"
    printf ': >"$HOME/probe-ran"\n' >"$home/.zshenv"

    out=$(run_curl_pipe "$home" /bin/zsh "$dir" TRUSTY_NO_MODIFY_PATH=1)

    assert_eq "$(count_matches "$home/.zshenv" "PATH=")" "0" "no export line written"
    assert_eq "$([[ -e "$home/probe-ran" ]] && echo yes || echo no)" "no" "no zsh was started (the .zshenv marker is absent)"
    assert_eq "$out" "" "prints nothing"

    rm -rf "$home"
}

test_unsafe_install_dir_is_noop() {
    echo "--- test_unsafe_install_dir_is_noop ---"
    local home dir out
    home=$(new_home)
    dir="$home/bad dir"

    out=$(run_curl_pipe "$home" /bin/zsh "$dir")

    assert_eq "$([[ -e "$home/.zshenv" ]] && echo yes || echo no)" "no" "no .zshenv written"
    assert_eq "$(has_text "$out" "unsafe characters")" "yes" "warns about the unsafe dir"

    rm -rf "$home"
}

test_unwritable_zshenv_reports_failure() {
    echo "--- test_unwritable_zshenv_reports_failure ---"
    skip_without_zsh || return
    if [[ "$(id -u)" == "0" ]]; then
        skip "root ignores file permissions"
        return
    fi
    local home dir out st
    home=$(new_home)
    dir="$home/.cargo/bin"
    : >"$home/.zshenv"
    chmod 444 "$home/.zshenv"

    out=$(run_curl_pipe "$home" /bin/zsh "$dir")
    st=$?

    assert_eq "$st" "1" "maybe_update_path exits 1"
    assert_eq "$(has_text "$out" "error: could not write ${home}/.zshenv")" "yes" "an error names the file"
    assert_eq "$(has_text "$out" "Added")" "no" "no success line"

    chmod 644 "$home/.zshenv"
    rm -rf "$home"
}

# A read-only HOME with no .zshenv: the file cannot be created.
test_readonly_home_reports_failure() {
    echo "--- test_readonly_home_reports_failure ---"
    skip_without_zsh || return
    if [[ "$(id -u)" == "0" ]]; then
        skip "root ignores file permissions"
        return
    fi
    local home dir out st
    home=$(new_home)
    dir="$home/.cargo/bin"
    chmod 555 "$home"

    out=$(run_curl_pipe "$home" /bin/zsh "$dir")
    st=$?

    assert_eq "$st" "1" "maybe_update_path exits 1"
    assert_eq "$(has_text "$out" "error: could not write ${home}/.zshenv")" "yes" "an error names the file"
    assert_eq "$(has_text "$out" "Added")" "no" "no success line"

    chmod 755 "$home"
    rm -rf "$home"
}

# A .zshenv that hangs must not hang the installer: the probe times out, is
# treated as failed, and the line is added (the safe direction).
test_hanging_zshenv_probe_times_out() {
    echo "--- test_hanging_zshenv_probe_times_out ---"
    skip_without_zsh || return
    local home dir out start elapsed
    home=$(new_home)
    dir="$home/.cargo/bin"
    printf 'sleep 30\n' >"$home/.zshenv"

    start=$(date +%s)
    out=$(run_curl_pipe "$home" /bin/zsh "$dir" PROBE_TIMEOUT=1)
    elapsed=$(( $(date +%s) - start ))

    assert_eq "$([[ "$elapsed" -lt 15 ]] && echo yes || echo no)" "yes" "returns well before the hanging .zshenv finishes (took ${elapsed}s)"
    assert_eq "$(count_matches "$home/.zshenv" "PATH='${dir}'")" "1" "falls back to adding the line"
    assert_eq "$(has_text "$out" "Could not start zsh to check PATH")" "yes" "says the probe failed"
    assert_eq "$(has_text "$out" "could not be started to confirm it")" "yes" "does not claim a verified success"

    rm -rf "$home"
}

# No zsh anywhere (SHELL names a missing zsh, PATH has none): the probe fails
# and the line is still added.
test_no_zsh_falls_back_to_adding() {
    echo "--- test_no_zsh_falls_back_to_adding ---"
    local home dir bin tool out
    home=$(new_home)
    dir="$home/.cargo/bin"
    bin="$home/fakebin"
    mkdir -p "$bin"
    # `sh` too: env looks the runner shell up on the new PATH.
    for tool in sh basename mktemp sed tail grep rm env sleep dirname; do
        ln -s "$(command -v "$tool")" "$bin/$tool"
    done

    out=$(run_curl_pipe "$home" /nonexistent/zsh "$dir" "PATH=$bin")

    assert_eq "$(count_matches "$home/.zshenv" "PATH='${dir}'")" "1" "the line is added"
    assert_eq "$(has_text "$out" "Could not start zsh to check PATH")" "yes" "says the probe failed"

    rm -rf "$home"
}

# bash keeps the pre-#11280 behaviour: with no terminal at all and no -y it
# skips. #11430: run with no controlling terminal, since `confirm` now asks
# on /dev/tty when one exists.
test_bash_no_tty_still_skips() {
    echo "--- test_bash_no_tty_still_skips ---"
    skip_without_no_ctty || return
    local home dir out
    home=$(new_home)
    dir="$home/.cargo/bin"

    out=$(no_ctty "${HERMETIC_ENV[@]}" HOME="$home" SHELL=/bin/bash TRUSTY_INSTALL_SOURCE_ONLY=1 \
        sh -c '. "$1"; maybe_update_path "$2"' _ "$INSTALL_SH" "$dir" </dev/null 2>&1)

    assert_eq "$([[ -e "$home/.bashrc" || -e "$home/.bash_profile" ]] && echo yes || echo no)" "no" "no bash rc file written"
    assert_eq "$(has_text "$out" "Skipped PATH modification")" "yes" "prints the manual instructions"

    rm -rf "$home"
}

test_unwritable_bashrc_reports_failure() {
    echo "--- test_unwritable_bashrc_reports_failure ---"
    if [[ "$(id -u)" == "0" ]]; then
        skip "root ignores file permissions"
        return
    fi
    local home dir out st
    home=$(new_home)
    dir="$home/.cargo/bin"
    : >"$home/.bashrc"
    chmod 444 "$home/.bashrc"

    out=$(run_curl_pipe "$home" /bin/bash "$dir" ASSUME_YES=1)
    st=$?

    assert_eq "$st" "1" "maybe_update_path exits 1"
    assert_eq "$(has_text "$out" "error: could not write ${home}/.bashrc")" "yes" "an error names the file"
    assert_eq "$(has_text "$out" "Added")" "no" "no success line"

    chmod 644 "$home/.bashrc"
    rm -rf "$home"
}

# run_sourced <home> <shell_path> <snippet> [args...] — sources install.sh in
# source-only mode, then runs <snippet> with install.sh as $1 and the extra
# args as $2...; stdin from /dev/null. Prints stdout+stderr; the exit status
# is the snippet's.
run_sourced() {
    local home="$1" shell_path="$2" snippet="$3"
    shift 3
    "${HERMETIC_ENV[@]}" HOME="$home" SHELL="$shell_path" TRUSTY_INSTALL_SOURCE_ONLY=1 \
        sh -c ". \"\$1\"; ${snippet}" _ "$INSTALL_SH" "$@" </dev/null 2>&1
}

# #11280 review: an unwritable .zshenv used to end in "Installation complete
# and verified" with exit 0. Mirrors main(): record the PATH failure, then
# compose the summary and status with finish_install.
test_finish_install_reports_path_failure() {
    echo "--- test_finish_install_reports_path_failure ---"
    skip_without_zsh || return
    if [[ "$(id -u)" == "0" ]]; then
        skip "root ignores file permissions"
        return
    fi
    local home dir out st
    home=$(new_home)
    dir="$home/.cargo/bin"
    : >"$home/.zshenv"
    chmod 444 "$home/.zshenv"

    out=$(run_sourced "$home" /bin/zsh \
        'pf=0; maybe_update_path "$2" || pf=1; finish_install 0 "$2/trusty-installer" "$pf" "$2"' "$dir")
    st=$?

    assert_eq "$st" "4" "exit status is 4 (installed, PATH not updated)"
    assert_eq "$(has_text "$out" "Installation complete and verified")" "no" "no unqualified success summary"
    assert_eq "$(has_text "$out" "PATH was NOT updated")" "yes" "summary says PATH was not updated"
    assert_eq "$(has_text "$out" "export PATH=\"${dir}:\$PATH\"")" "yes" "summary prints the exact export line"

    chmod 644 "$home/.zshenv"
    rm -rf "$home"
}

test_finish_install_success_and_bootstrap_failure() {
    echo "--- test_finish_install_success_and_bootstrap_failure ---"
    local home dir out st
    home=$(new_home)
    dir="$home/.cargo/bin"

    out=$(run_sourced "$home" /bin/zsh 'finish_install 0 "$2/trusty-installer" 0 "$2"' "$dir")
    st=$?
    assert_eq "$st" "0" "both ok: exit 0"
    assert_eq "$(has_text "$out" "Installation complete and verified")" "yes" "both ok: success summary"
    assert_eq "$(has_text "$out" "PATH was NOT updated")" "no" "both ok: no PATH note"

    out=$(run_sourced "$home" /bin/zsh 'finish_install 2 "$2/trusty-installer" 1 "$2"' "$dir")
    st=$?
    assert_eq "$st" "2" "bootstrap failed: its status passes through"
    assert_eq "$(has_text "$out" "exited with status 2")" "yes" "bootstrap failed: failure summary"
    assert_eq "$(has_text "$out" "PATH was NOT updated")" "yes" "bootstrap and PATH failed: PATH note too"

    rm -rf "$home"
}

# #11280 review: stopping the probe watchdog used to orphan its `sleep` for
# the whole timeout. 7919 is an unusual timeout so the `ps` match is ours.
test_probe_leaves_no_stray_sleep() {
    echo "--- test_probe_leaves_no_stray_sleep ---"
    skip_without_zsh || return
    local home n i
    home=$(new_home)

    run_sourced "$home" /bin/zsh 'PATH_PROBE_TIMEOUT_SECS=7919; _zsh_sees_dir /nonexistent-11280 || true' >/dev/null

    n=1
    for i in 1 2 3 4 5 6 7 8 9 10; do
        n=$(pgrep -f '^sleep 7919$' | wc -l | tr -d ' ')
        [[ "$n" == "0" ]] && break
        sleep 0.2
    done
    assert_eq "$n" "0" "no watchdog sleep is left running after a successful probe (checked ${i}x)"
    if [[ "$n" != "0" ]]; then
        pkill -f '^sleep 7919$'
    fi

    rm -rf "$home"
}

# ---------------------------------------------------------------------------
# #11430: `curl | sh` consent. stdin is the script pipe, so `tctl install`
# (which asks only when its stdin is a terminal, #2112) refused with exit 3.
# These cases run install.sh's whole `main` with the script piped into `sh`,
# a fake `curl` serving a local release fixture (no network) and a stub
# `trusty-installer` that records its arguments and whether its stdin is a
# terminal. The install dir is under a temp HOME.
# ---------------------------------------------------------------------------

FIXTURE_VERSION="0.0.1"

# no_ctty <cmd...> — runs <cmd> in a new session, so it has no controlling
# terminal even when this suite runs from one. Exit status is <cmd>'s.
no_ctty() {
    perl -MPOSIX -e '
        my $pid = fork;
        die "fork: $!" unless defined $pid;
        if ($pid) { waitpid($pid, 0); exit($? & 127 ? 128 + ($? & 127) : $? >> 8) }
        POSIX::setsid() or die "setsid: $!";
        exec @ARGV or die "exec: $!";
    ' "$@"
}

# with_pty <cmd...> — runs <cmd> on a new pseudo-terminal, which becomes its
# controlling terminal. util-linux and BSD `script` differ in arguments.
with_pty() {
    if script --version >/dev/null 2>&1; then
        SHELL=/bin/bash script -qec "$(printf '%q ' "$@")" /dev/null
    else
        script -q /dev/null "$@"
    fi
}

# skip_without_no_ctty / skip_without_pty — print SKIP and return 1 when the
# host cannot give a command no terminal / a terminal.
skip_without_no_ctty() {
    if ! command -v perl >/dev/null 2>&1; then
        skip "perl not installed (needed to drop the controlling terminal)"
        return 1
    fi
    if [[ "$(no_ctty sh -c '( : </dev/tty ) 2>/dev/null && echo tty || echo none' </dev/null)" != "none" ]]; then
        skip "could not drop the controlling terminal"
        return 1
    fi
}
skip_without_pty() {
    if ! command -v script >/dev/null 2>&1; then
        skip "script(1) not installed (needed for a pseudo-terminal)"
        return 1
    fi
    if ! with_pty sh -c '( : </dev/tty ) 2>/dev/null && echo tty' </dev/null 2>&1 | grep -q tty; then
        skip "script(1) did not provide a controlling terminal"
        return 1
    fi
}

# make_release_fixture <dir> — builds, under <dir>:
#   dl/   the release tarball (stub trusty-installer + tctl) and its .sha256
#   bin/  a fake `curl` that copies dl/<basename of URL> to its -o file and
#         logs each URL to <dir>/curl.log; anything else exits 22
#   stub.log is written by the stub: "args=<args> stdin_tty=<yes|no>".
# The stub mimics `tctl install`'s #2112 gate: no -y and no terminal on
# stdin prints the refusal and exits 3; otherwise exits 0.
make_release_fixture() {
    local fx="$1" target name
    target=$(run_sourced "$fx" /bin/sh 'detect_target')
    name="trusty-installer-${FIXTURE_VERSION}-${target}"
    mkdir -p "$fx/dl/$name" "$fx/bin"
    cat >"$fx/dl/$name/trusty-installer" <<EOF
#!/bin/sh
if [ -t 0 ]; then t=yes; else t=no; fi
printf 'args=%s stdin_tty=%s\n' "\$*" "\$t" >>"$fx/stub.log"
case " \$* " in *" -y "*) exit 0 ;; esac
if [ "\$t" = no ]; then
    echo "tctl install: refusing to install without confirmation in a non-interactive context" >&2
    exit 3
fi
exit 0
EOF
    cp "$fx/dl/$name/trusty-installer" "$fx/dl/$name/tctl"
    chmod 755 "$fx/dl/$name/trusty-installer" "$fx/dl/$name/tctl"
    tar -czf "$fx/dl/$name.tar.gz" -C "$fx/dl" "$name"
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$fx/dl" && sha256sum "$name.tar.gz" >"$name.tar.gz.sha256")
    else
        (cd "$fx/dl" && shasum -a 256 "$name.tar.gz" >"$name.tar.gz.sha256")
    fi
    cat >"$fx/bin/curl" <<EOF
#!/bin/sh
out=""; url=""
while [ "\$#" -gt 0 ]; do
    case "\$1" in
        -o | -H | -w) [ "\$1" = -o ] && out="\$2"; shift ;;
        -*) ;;
        *) url="\$1" ;;
    esac
    shift
done
printf '%s\n' "\$url" >>"$fx/curl.log"
src="$fx/dl/\${url##*/}"
[ -f "\$src" ] && [ -n "\$out" ] || exit 22
cp "\$src" "\$out"
EOF
    chmod 755 "$fx/bin/curl"
}

# curl_pipe_main_cmd <home> <fixture> [install.sh args...] — sets CMD to the
# argv that runs `cat install.sh | sh -s -- <args>` with a hermetic env: the
# fixture's fake curl first on a system-only PATH, a pinned version, the
# install dir under <home>, and no PATH edit. Callers prefix it with
# no_ctty / with_pty, and may add NAME=value pairs in the EXTRA_ENV array.
curl_pipe_main_cmd() {
    local home="$1" fx="$2"
    shift 2
    CMD=("${HERMETIC_ENV[@]}" -u CARGO_HOME HOME="$home" SHELL=/bin/sh
        PATH="$fx/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        TRUSTY_VERSION="$FIXTURE_VERSION" TRUSTY_INSTALL_DIR="$home/.cargo/bin"
        TRUSTY_NO_MODIFY_PATH=1 ${EXTRA_ENV[@]+"${EXTRA_ENV[@]}"}
        bash -c 'f="$1"; shift; cat "$f" | sh -s -- "$@"' _ "$INSTALL_SH" "$@")
}

# (a) piped stdin + a terminal: `trusty-installer install` gets the terminal
# on stdin (no -y), so the user sees and answers tctl's prompt.
test_curl_pipe_with_terminal_bootstraps_from_tty() {
    echo "--- test_curl_pipe_with_terminal_bootstraps_from_tty ---"
    skip_without_pty || return
    local home fx out st
    home=$(new_home)
    fx=$(new_home)
    make_release_fixture "$fx"
    curl_pipe_main_cmd "$home" "$fx"

    with_pty "${CMD[@]}" </dev/null >"$fx/out.raw" 2>&1
    st=$?
    out=$(tr -d '\r' <"$fx/out.raw")

    assert_eq "$(cat "$fx/stub.log" 2>/dev/null)" "args=install stdin_tty=yes" \
        "bootstrap runs once, without -y, with the terminal on stdin"
    assert_eq "$st" "0" "install.sh exits 0"
    assert_eq "$(has_text "$out" "failed to bootstrap")" "no" "no bootstrap-failure banner"

    rm -rf "$home" "$fx"
}

# (b) piped stdin + no terminal + no -y: stop before any download, with one
# message naming both ways to proceed non-interactively.
test_curl_pipe_without_terminal_stops_before_bootstrap() {
    echo "--- test_curl_pipe_without_terminal_stops_before_bootstrap ---"
    skip_without_no_ctty || return
    local home fx out st
    home=$(new_home)
    fx=$(new_home)
    make_release_fixture "$fx"
    curl_pipe_main_cmd "$home" "$fx"

    out=$(no_ctty "${CMD[@]}" </dev/null 2>&1)
    st=$?

    assert_eq "$st" "1" "install.sh exits 1"
    assert_eq "$(has_text "$out" "| sh -s -- -y")" "yes" "message names | sh -s -- -y"
    assert_eq "$(has_text "$out" "TRUSTY_YES=1")" "yes" "message names TRUSTY_YES=1"
    assert_eq "$(grep -c '^error:' <<<"$out")" "1" "exactly one error line"
    assert_eq "$(has_text "$out" "failed to bootstrap")" "no" "no bootstrap-failure banner"
    assert_eq "$([[ -e "$fx/stub.log" ]] && echo invoked || echo never)" "never" "bootstrap never invoked"
    assert_eq "$([[ -e "$fx/curl.log" ]] && echo yes || echo no)" "no" "nothing downloaded"
    assert_eq "$([[ -e "$home/.cargo/bin/trusty-installer" ]] && echo yes || echo no)" "no" "nothing installed"

    rm -rf "$home" "$fx"
}

# CI set + a terminal + piped stdin + no -y: the owner decision treats CI as
# no terminal (a pty nobody types at), so the same early stop applies.
test_ci_with_terminal_stops_before_bootstrap() {
    echo "--- test_ci_with_terminal_stops_before_bootstrap ---"
    skip_without_pty || return
    local home fx out st
    home=$(new_home)
    fx=$(new_home)
    make_release_fixture "$fx"
    EXTRA_ENV=(CI=1)
    curl_pipe_main_cmd "$home" "$fx"
    unset EXTRA_ENV

    with_pty "${CMD[@]}" </dev/null >"$fx/out.raw" 2>&1
    st=$?
    out=$(tr -d '\r' <"$fx/out.raw")

    assert_eq "$st" "1" "install.sh exits 1"
    assert_eq "$(has_text "$out" "| sh -s -- -y")" "yes" "message names | sh -s -- -y"
    assert_eq "$(has_text "$out" "TRUSTY_YES=1")" "yes" "message names TRUSTY_YES=1"
    assert_eq "$([[ -e "$fx/stub.log" ]] && echo invoked || echo never)" "never" "bootstrap never invoked"
    assert_eq "$([[ -e "$fx/curl.log" ]] && echo yes || echo no)" "no" "nothing downloaded"

    rm -rf "$home" "$fx"
}

# (c) -y and TRUSTY_YES=1 keep `install -y`, with or without a terminal.
test_yes_bootstraps_with_yes_flag() {
    echo "--- test_yes_bootstraps_with_yes_flag ---"
    skip_without_no_ctty || return
    local home fx st mode
    for mode in flag env; do
        home=$(new_home)
        fx=$(new_home)
        make_release_fixture "$fx"
        if [[ "$mode" == flag ]]; then
            curl_pipe_main_cmd "$home" "$fx" -y
        else
            EXTRA_ENV=(TRUSTY_YES=1)
            curl_pipe_main_cmd "$home" "$fx"
            unset EXTRA_ENV
        fi

        no_ctty "${CMD[@]}" </dev/null >/dev/null 2>&1
        st=$?

        assert_eq "$(cat "$fx/stub.log" 2>/dev/null)" "args=install -y stdin_tty=no" "${mode}: bootstrap runs install -y"
        assert_eq "$st" "0" "${mode}: install.sh exits 0"
        rm -rf "$home" "$fx"
    done
}

# confirm() under `curl | sh` asks on the terminal: bash's PATH prompt is
# answered "y" there, so the export line is written.
test_curl_pipe_confirm_reads_tty() {
    echo "--- test_curl_pipe_confirm_reads_tty ---"
    skip_without_pty || return
    local home dir out
    home=$(new_home)
    dir="$home/.cargo/bin"

    # The answer is typed into the pty; the sleep keeps script(1) from
    # sending EOF before the prompt reads it.
    out=$( { printf 'y\n'; sleep 2; } | with_pty "${HERMETIC_ENV[@]}" HOME="$home" SHELL=/bin/bash \
        TRUSTY_INSTALL_SOURCE_ONLY=1 \
        bash -c 'printf "" | sh -c ". \"\$1\"; maybe_update_path \"\$2\"" _ "$1" "$2"' _ "$INSTALL_SH" "$dir" 2>&1 | tr -d '\r')

    assert_eq "$(has_text "$out" "[y/N]")" "yes" "the prompt is shown on the terminal"
    assert_eq "$(count_matches "$home/.bashrc" "PATH='${dir}'")" "1" "the terminal's \"y\" wrote .bashrc"

    rm -rf "$home"
}

echo "==========================================================="
echo "install.sh PATH and consent test suite (#3874, #3897, #11280, #11430)"
echo "==========================================================="
test_zsh_writes_only_zshenv
test_bash_writes_both_files
test_repeat_install_is_idempotent
test_bash_already_on_path_is_noop
test_login_interactive_zsh_sources_dir_exactly_once
test_differently_quoted_existing_entry_is_recognised
test_commented_out_entry_does_not_block_real_write
test_sibling_path_var_does_not_block_real_write
test_dir_as_prefix_of_longer_path_does_not_block_real_write
test_zsh_missing_dir_is_added_without_prompt
if command -v dash >/dev/null 2>&1; then
    RUNNER_SH=dash test_zsh_missing_dir_is_added_without_prompt
fi
test_zsh_added_dir_resolves_in_every_mode
test_curl_pipe_rerun_adds_no_duplicate
test_zshrc_only_dir_still_gets_zshenv_line
test_dir_already_in_zshenv_is_noop
test_rustup_cargo_env_counts_as_present
test_probe_does_not_run_zshrc
test_exported_zdotdir_still_writes_home_zshenv
test_zshenv_line_not_taking_effect_warns
test_no_modify_path_opt_out_is_noop
test_unsafe_install_dir_is_noop
test_unwritable_zshenv_reports_failure
test_readonly_home_reports_failure
test_hanging_zshenv_probe_times_out
test_no_zsh_falls_back_to_adding
test_bash_no_tty_still_skips
test_unwritable_bashrc_reports_failure
test_finish_install_reports_path_failure
test_finish_install_success_and_bootstrap_failure
test_probe_leaves_no_stray_sleep
test_curl_pipe_with_terminal_bootstraps_from_tty
test_curl_pipe_without_terminal_stops_before_bootstrap
test_ci_with_terminal_stops_before_bootstrap
test_yes_bootstraps_with_yes_flag
test_curl_pipe_confirm_reads_tty

echo "==========================================================="
echo "Result: $PASS passed, $FAIL failed, $SKIP skipped"
echo "==========================================================="

[[ "$FAIL" -eq 0 ]] || exit 1
if [[ "$SKIP" -ne 0 && "${ALLOW_SKIP:-0}" != "1" ]]; then
    echo "error: $SKIP test(s) skipped; set ALLOW_SKIP=1 to accept a partial run" >&2
    exit 1
fi
exit 0
