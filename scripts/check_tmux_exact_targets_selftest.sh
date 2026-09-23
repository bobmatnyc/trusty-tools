#!/usr/bin/env bash
#
# check_tmux_exact_targets_selftest.sh — mutation self-test for
# scripts/check_tmux_exact_targets.sh (issue #8443).
#
# Why: a gate whose matcher silently stopped matching reports OK exactly like a
#   clean tree (#4618, #5440). Its ability to fail is tested here before CI
#   trusts its pass.
# What: one throwaway git repo per case, with the gate and an allowlist copied
#   into its `scripts/` so the gate's repo root resolves inside the fixture.
#   Each case asserts the exit status and, for failures, the message. The
#   cases pin the defect (a bare `-t name`, in Rust argv, a Rust format string
#   and a shell line), the accepted forms (`=name`, a helper call, a `let`
#   bound from a helper, an immutable id), the false-positive guards (doc
#   comments, a `== "-t"` comparison), and the fail-closed paths (empty scan,
#   stale allowlist row).
#
# Usage: bash scripts/check_tmux_exact_targets_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_tmux_exact_targets.sh"
PASSED=0
FAILED=0
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

# new_fixture <name>: a git repo with the gate and an empty allowlist.
new_fixture() {
  local dir="$TMP_ROOT/$1"
  mkdir -p "$dir/scripts" "$dir/src"
  cp "$GATE" "$dir/scripts/check_tmux_exact_targets.sh"
  printf '# allowlist\n' > "$dir/scripts/tmux-exact-targets-allowlist.tsv"
  git -C "$dir" init -q
  printf '%s\n' "$dir"
}

# run_case <name> <expected-exit> <expected-message-or-empty> <dir>
run_case() {
  local name="$1" want="$2" needle="$3" dir="$4" out status
  git -C "$dir" add -A
  set +e
  out="$(bash "$dir/scripts/check_tmux_exact_targets.sh" 2>&1)"
  status=$?
  set -e
  if [ "$status" -ne "$want" ]; then
    echo "FAIL $name: exit $status, want $want"
    printf '%s\n' "$out" | sed 's/^/    /'
    FAILED=$((FAILED + 1))
    return
  fi
  if [ -n "$needle" ] && ! printf '%s' "$out" | grep -qF -- "$needle"; then
    echo "FAIL $name: output lacks '$needle'"
    printf '%s\n' "$out" | sed 's/^/    /'
    FAILED=$((FAILED + 1))
    return
  fi
  echo "ok   $name (exit $status)"
  PASSED=$((PASSED + 1))
}

d="$(new_fixture rust_bare_argv)"
cat > "$d/src/a.rs" <<'RS'
fn kill(name: &str) {
    let _ = std::process::Command::new("tmux").args(["kill-session", "-t", name]).output();
}
RS
run_case rust_bare_argv 1 'non-exact target `name`' "$d"

d="$(new_fixture rust_bare_format)"
cat > "$d/src/a.rs" <<'RS'
fn hint(name: &str) -> String {
    format!("tmux attach-session -t {name}")
}
RS
run_case rust_bare_format 1 'bare tmux target {name}' "$d"

d="$(new_fixture shell_bare)"
cat > "$d/src/a.sh" <<'SH'
tmux has-session -t "$SESSION" && echo up
SH
run_case shell_bare 1 'bare tmux target "$SESSION"' "$d"

d="$(new_fixture shell_positional)"
cat > "$d/src/a.sh" <<'SH'
tmux kill-session -t "$1"
SH
run_case shell_positional 1 'bare tmux target "$1"' "$d"

d="$(new_fixture shell_continuation)"
cat > "$d/src/a.sh" <<'SH'
tmux kill-session \
  -t "$SESSION"
SH
run_case shell_continuation 1 'bare tmux target "$SESSION"' "$d"

d="$(new_fixture shell_attached)"
cat > "$d/src/a.sh" <<'SH'
tmux send-keys -t"$S" Enter
SH
run_case shell_attached 1 'bare tmux target "$S"' "$d"

d="$(new_fixture shell_wrapper_array)"
cat > "$d/src/a.sh" <<'SH'
TM=(tmux -L private)
"${TM[@]}" has-session -t "$S"
SH
run_case shell_wrapper_array 1 'bare tmux target "$S"' "$d"

d="$(new_fixture rust_conditional_let)"
cat > "$d/src/a.rs" <<'RS'
fn kill(n: &str, c: bool) {
    let t = if c { exact_session_target(n) } else { n.to_string() };
    let _ = std::process::Command::new("tmux").args(["kill-session", "-t", &t]);
}
RS
run_case rust_conditional_let 1 'non-exact target `&t`' "$d"

d="$(new_fixture rust_attached_format)"
cat > "$d/src/a.rs" <<'RS'
fn arg(n: &str) -> String {
    let _ = "tmux";
    format!("-t{n}")
}
RS
run_case rust_attached_format 1 'bare tmux target {n}' "$d"

d="$(new_fixture rust_no_tmux_word)"
mkdir -p "$d/other"
printf 'tmux is used elsewhere\n' > "$d/other/readme.sh"
cat > "$d/src/a.rs" <<'RS'
fn kill(bin: &str, name: &str) {
    let _ = std::process::Command::new(bin).args(["kill-session", "-t", name]);
}
RS
run_case rust_no_tmux_word 1 'non-exact target `name`' "$d"

d="$(new_fixture markdown_asset)"
mkdir -p "$d/crates/x/src/assets/skills"
printf 'Run `tmux select-window -t %s` to realign.\n' "'main:2'" \
  > "$d/crates/x/src/assets/skills/s.md"
run_case markdown_asset 1 "bare tmux target 'main:2'" "$d"

d="$(new_fixture rust_exact_forms)"
cat > "$d/src/a.rs" <<'RS'
// A comment naming tmux has-session -t bare is prose, not an invocation.
/// Doc: `tmux kill-session -t name` is how NOT to do it.
fn exact(name: &str, pane: &str) {
    let _ = std::process::Command::new("tmux").args(["kill-session", "-t", "=work"]).output();
    let _ = ["send-keys", "-t", "%3", "Enter"];
    let _ = ["has-session", "-t", &exact_session_target(name)];
    let target = exact_window_target(name);
    let _ = ["capture-pane", "-t", &target, "-p"];
    let _ = ["display-message", "-t", &TmuxTarget::session(pane).as_target()];
    let _ = std::env::args().any(|a| a == "-t");
}
RS
cat > "$d/src/b.sh" <<'SH'
# tmux kill-session -t bare-in-a-comment
tmux has-session -t "=$SESSION" && tmux send-keys -t "=$SESSION:" Enter
SH
run_case rust_exact_forms 0 'every -t target exact' "$d"

d="$(new_fixture allowlisted)"
cat > "$d/src/a.rs" <<'RS'
fn pane(pane_id: &str) {
    let _ = std::process::Command::new("tmux").args(["display-message", "-t", pane_id]);
}
RS
printf 'src/a.rs\tdisplay-message", "-t", pane_id\ta runtime %%N pane id\n' \
  >> "$d/scripts/tmux-exact-targets-allowlist.tsv"
run_case allowlisted 0 'every -t target exact' "$d"

d="$(new_fixture stale_allowlist)"
cat > "$d/src/a.rs" <<'RS'
fn ok() { let _ = ["has-session", "-t", "=tmux-work"]; }
RS
printf 'src/a.rs\tno-such-line\tstale on purpose\n' >> "$d/scripts/tmux-exact-targets-allowlist.tsv"
run_case stale_allowlist 1 'STALE allowlist row' "$d"

d="$(new_fixture empty_scan)"
printf 'fn main() {}\n' > "$d/src/a.rs"
run_case empty_scan 1 'SCAN FLOOR' "$d"

echo "check_tmux_exact_targets_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
