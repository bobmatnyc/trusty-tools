#!/usr/bin/env bash
#
# check_no_tcp_listeners_selftest.sh — mutation self-test for
# scripts/check_no_tcp_listeners.sh (issue #8926).
#
# Why: a gate whose matcher silently stopped matching reports OK exactly like a
#   clean tree (#4618, #5440). Its ability to fail is tested before CI trusts
#   its pass.
# What: one throwaway git repo per case, holding fixture crates and its own
#   allowlist (passed through TCP_LISTENER_ALLOWLIST). Each case asserts the
#   exit status and, for a failure, the message. The cases pin the defect (an
#   injected non-allowlisted bind, a TcpSocket, a bind_with_auto_port call, a
#   test binding a fixed port, a bind under `#[cfg(not(test))]` or after
#   `#[cfg(test)] mod tests;`), the
#   accepted forms (an allowlisted bind, an ephemeral test bind, a bind a test
#   asserts fails, a mention in a comment or string), and the fail-closed paths
#   (stale row, malformed row, empty scan, a tree below the default floor).
#
# Usage: bash scripts/check_no_tcp_listeners_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_no_tcp_listeners.sh"
PASSED=0
FAILED=0
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMP_ROOT"' EXIT

ROW_OK="$(printf 'permanent\tconsole\tconsole\tcrates/console/src/\tADR-0032\tthe one TCP surface')"

# new_fixture <name> [allowlist-row...]: a git repo whose allowlist holds the
# rows given, plus an allowlisted console crate so the default rows are used.
new_fixture() {
  local dir="$TMP_ROOT/$1" row
  shift
  mkdir -p "$dir/crates/console/src" "$dir/crates/app/src" "$dir/crates/app/tests"
  printf '# allowlist\n%s\n' "$ROW_OK" > "$dir/allow.tsv"
  for row in "$@"; do printf '%s\n' "$row" >> "$dir/allow.tsv"; done
  printf 'pub async fn bind(a: std::net::SocketAddr) {\n    let _l = tokio::net::TcpListener::bind(a).await;\n}\n' \
    > "$dir/crates/console/src/bind.rs"
  printf 'pub fn nothing() {}\n' > "$dir/crates/app/src/lib.rs"
  git -C "$dir" init -q
  printf '%s\n' "$dir"
}

# run_case <name> <expected-exit> <expected-message-or-empty> <dir>
# The scan floor is 1 unless CASE_MIN_FILES says otherwise; an empty
# CASE_MIN_FILES leaves the gate on its default floor.
run_case() {
  local name="$1" want="$2" needle="$3" dir="$4" out status
  git -C "$dir" add -A
  set +e
  out="$(cd "$dir" && TCP_LISTENER_ALLOWLIST=allow.tsv TCP_LISTENER_MIN_FILES="${CASE_MIN_FILES-1}" bash "$GATE" 2>&1)"
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
  echo "ok   $name"
  PASSED=$((PASSED + 1))
}

# 1. Clean tree: allowlisted bind, ephemeral test binds, a bind asserted to
#    fail, and mentions that are only prose.
d="$(new_fixture clean)"
cat > "$d/crates/app/src/lib.rs" <<'RS'
//! Talks to the console; never binds `TcpListener::bind("0.0.0.0:80")`.
pub fn describe() -> &'static str {
    "we used to call TcpListener::bind here"
}
/* TcpSocket::new_v4() was retired */

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    #[test]
    fn binds_ephemeral() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let m = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        assert!(TcpListener::bind(("127.0.0.1", 1)).is_err());
        let again = TcpListener::bind(("127.0.0.1", 7788));
        assert!(again.is_err());
        drop((l, m));
    }
}
RS
cat > "$d/crates/app/tests/it.rs" <<'RS'
#[tokio::test]
async fn it_binds() {
    let _l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
}
RS
run_case "clean tree passes" 0 "OK: 3 file(s) scanned, 1 TCP site(s)" "$d"

# 2. The defect: a non-allowlisted production bind.
d="$(new_fixture injected)"
printf 'pub async fn serve() {\n    let _l = tokio::net::TcpListener::bind("127.0.0.1:7999").await;\n}\n' \
  > "$d/crates/app/src/lib.rs"
run_case "injected non-allowlisted bind fails" 1 "crates/app/src/lib.rs:2: TCP listener site" "$d"

# 3. The ephemeral literal does not excuse PRODUCTION code.
d="$(new_fixture prod-ephemeral)"
printf 'pub fn serve() {\n    let _l = std::net::TcpListener::bind("127.0.0.1:0");\n}\n' \
  > "$d/crates/app/src/lib.rs"
run_case "production ephemeral bind fails" 1 "crates/app/src/lib.rs:2: TCP listener site" "$d"

# 4. A TcpSocket builder is a listen site too.
d="$(new_fixture tcp-socket)"
printf 'pub fn build() {\n    let _s = tokio::net::TcpSocket::new_v4();\n}\n' > "$d/crates/app/src/lib.rs"
run_case "TcpSocket builder fails" 1 "crates/app/src/lib.rs:2: TCP listener site" "$d"

# 5. A test binding a fixed port is a finding.
d="$(new_fixture test-fixed-port)"
printf '#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {\n        let _l = std::net::TcpListener::bind("127.0.0.1:7880").unwrap();\n    }\n}\n' \
  > "$d/crates/app/src/lib.rs"
run_case "test binding a fixed port fails" 1 "crates/app/src/lib.rs:5: test binds a non-ephemeral address" "$d"

# 6. `#[cfg(test)] mod tests;` opens no region: the bind after it is production.
d="$(new_fixture cfg-test-decl)"
printf '#[cfg(test)]\nmod tests;\n\npub fn serve() {\n    let _l = std::net::TcpListener::bind("127.0.0.1:0");\n}\n' \
  > "$d/crates/app/src/lib.rs"
run_case "bind after a cfg(test) mod declaration fails" 1 "crates/app/src/lib.rs:5: TCP listener site" "$d"

# 7. `#[cfg(not(test))]` is production.
d="$(new_fixture cfg-not-test)"
printf '#[cfg(not(test))]\npub fn serve() {\n    let _l = std::net::TcpListener::bind("127.0.0.1:0");\n}\n' \
  > "$d/crates/app/src/lib.rs"
run_case "bind under cfg(not(test)) fails" 1 "crates/app/src/lib.rs:3: TCP listener site" "$d"

# 8. A test region closes: a bind after the test module is production.
d="$(new_fixture region-closes)"
printf '#[cfg(test)]\nmod tests {\n    fn f() { let s = "}"; }\n}\npub fn serve() {\n    let _l = std::net::TcpListener::bind("127.0.0.1:0");\n}\n' \
  > "$d/crates/app/src/lib.rs"
run_case "bind after a closed test module fails" 1 "crates/app/src/lib.rs:6: TCP listener site" "$d"

# 9. A row whose path excuses nothing is stale.
d="$(new_fixture stale "$(printf 'temporary\tapp\tapp\tcrates/app/src/gone.rs\t#1\tremoved bind')")"
run_case "stale allowlist path fails" 1 "STALE allowlist path (excuses no site): crates/app/src/gone.rs" "$d"

# 10. A malformed row fails: a temporary row must cite an issue, not an ADR.
d="$(new_fixture malformed "$(printf 'temporary\tapp\tapp\tcrates/console/src/bind.rs\tADR-0032\tno issue')")"
run_case "malformed allowlist row fails" 1 "temporary row must cite an issue" "$d"

# 11. An empty scan is a failure, never a pass.
d="$TMP_ROOT/empty"
mkdir -p "$d"
printf '# allowlist\n' > "$d/allow.tsv"
printf 'readme\n' > "$d/README.md"
git -C "$d" init -q
run_case "empty scan fails" 1 "SCAN FLOOR" "$d"

# 12. The default floor refuses a tree this small: a sensible minimum, not zero.
d="$(new_fixture below-default-floor)"
CASE_MIN_FILES="" run_case "tree below the default scan floor fails" 1 "SCAN FLOOR: 2 crates" "$d"

# 13. bind_with_auto_port (trusty-common) returns a bound TcpListener: a call
#     outside the allowlist is a site, in production and in a test alike.
d="$(new_fixture auto-port)"
printf 'pub async fn serve(a: std::net::SocketAddr) {\n    let _l = trusty_common::bind_with_auto_port(a, 8).await;\n}\n' \
  > "$d/crates/app/src/lib.rs"
printf '#[tokio::test]\nasync fn t() {\n    let _l = bind_with_auto_port(7878, 1).await;\n}\n' \
  > "$d/crates/app/tests/it.rs"
run_case "bind_with_auto_port call fails" 1 "crates/app/src/lib.rs:2: TCP listener site" "$d"
run_case "bind_with_auto_port in a test fails" 1 "crates/app/tests/it.rs:3: test binds a non-ephemeral address" "$d"

echo "check_no_tcp_listeners_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
