#!/usr/bin/env bash
#
# check_architect_subproject_selftest.sh — planted-violation cases for
# scripts/check_architect_subproject.sh (issue #8436).
#
# Why: a gate that has only been seen passing cannot be told apart from one
#   that returns 0 unconditionally. Each violation class gets one planted case
#   that must turn the gate red with that class named in its output.
#
# What: builds a minimal clean repo tree in a temp dir (root Cargo.toml with
#   `members = ["crates/*"]`, a sub-project file, and a skill whose shipped
#   copy matches), asserts the gate passes on it, then for each case copies the
#   clean tree, plants one violation, and asserts exit 1 plus the expected
#   "FAIL <CLASS>" text. The clean tree holds every pair the gate lists
#   (`--list-pairs`), and each shipped copy gets its own planted-drift case.
#   Near misses must pass: a root Cargo.toml that names the Rust crate
#   crates/trusty-architect is not a sub-project membership (#9822).
#   Token-shaped strings are assembled at run time so this file carries no
#   literal credential shape.
#
# Test: this IS the test. Run: scripts/check_architect_subproject_selftest.sh
#
# Portability: POSIX tools only; bash 3.2 (macOS) and bash 5 (Linux CI).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/check_architect_subproject.sh"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/architect-selftest.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

SKILL_SRC="python/trusty-architect/skills/tm-architect-setup.md"
SKILL_DST="content/skills/tm-architect-setup.md"
PAIRS="$("$GATE" --list-pairs)"
[ -n "$PAIRS" ] || { echo "FAIL: the gate lists no shipped-copy pairs" >&2; exit 1; }

clean="$WORK/clean"
mkdir -p "$clean/python/trusty-architect/scripts"
printf '[workspace]\nresolver = "2"\nmembers = ["crates/*"]\n' >"$clean/Cargo.toml"
printf 'import os\nROOT = os.path.dirname(__file__)\n' >"$clean/python/trusty-architect/scripts/ok.py"
for pair in $PAIRS; do
  src="$clean/python/trusty-architect/${pair%%|*}"
  dst="$clean/${pair#*|}"
  mkdir -p "$(dirname "$src")" "$(dirname "$dst")"
  printf 'shipped copy of %s\n' "${pair%%|*}" >"$src"
  cp "$src" "$dst"
done

repeat() { # $1 = char, $2 = count
  local s="" i=0
  while [ "$i" -lt "$2" ]; do s="$s$1"; i=$((i + 1)); done
  printf '%s' "$s"
}
TOKEN="ghp""_$(repeat a 36)"

case_dir() { # $1 = case name; a directory name without slashes or colons
  printf '%s' "$1" | tr '/:' '__'
}

# #9996: one leak line per class; the gate must reject each in every scanned
# tree (the content tree is the one the old scan skipped).
leak_text() { # $1 = kind
  case "$1" in
    owner_handle) printf 'maintained by bobmatnyc\n' ;;
    owner_first) printf 'ask masa before merging\n' ;;
    owner_short) printf 'Bob wants this reviewed\n' ;;
    org) printf 'clone https://github.com/realcorp/widgets\n' ;;
    host_local) printf 'ssh studio.local\n' ;;
    # #10161: the host boundary still catches a <name>.local mid-line.
    host_local_port) printf 'curl http://studio.local:8080/\n' ;;
    host_tailnet) printf 'ssh box.tail1234.ts.net\n' ;;
    slack) printf 'notify U01ABC23DEF now\n' ;;
    email) printf 'mail someone@corp.io\n' ;;
    org_repo) printf 'gh pr view 1 --repo realcorp/widgets\n' ;;
    org_cli) printf 'gh pr list -R realcorp/widgets\n' ;;
    org_pages) printf 'docs at realcorp.github.io\n' ;;
    org_gitlab) printf 'clone gitlab.com/realcorp/widgets\n' ;;
    # #9171: a gh account config dir and a tailnet (CGNAT 100.64/10) address.
    gh_config) printf 'cat ~/.config/gh-realacct/hosts.yml\n' ;;
    # #10161: an allowed name as a prefix must not pass.
    gh_config_long) printf 'cat ~/.config/gh-acmecorp-realname/hosts.yml\n' ;;
    tailnet_ip) printf 'ssh 100.101.102.103\n' ;;
    # Binary and non-UTF-8 files must still be scanned (#9996 fix round).
    owner_nul) printf 'x\000y\nask masa\n' ;;
    owner_badutf8) printf '\377\376 ask masa\n' ;;
  esac
}

plant_leak() { # $1 = leak-<kind>-<tree>, $2 = tree dir
  local rest="${1#leak-}" tree kind
  tree="${rest##*-}"
  kind="${rest%-*}"
  case "$tree" in
    content) leak_text "$kind" >"$2/content/instructions/architect/skills/leak.md" ;;
    python) leak_text "$kind" >"$2/python/trusty-architect/notes.md" ;;
    assets) leak_text "$kind" >"$2/crates/trusty-mpm/src/assets/architect/scripts/leak.txt" ;;
  esac
}

plant() { # $1 = case name; mutates $WORK/<case_dir>, a copy of the clean tree
  local t="$WORK/$(case_dir "$1")" sub="$WORK/$(case_dir "$1")/python/trusty-architect"
  case "$1" in
    cargo-toml) printf '[package]\nname = "x"\n' >"$sub/Cargo.toml" ;;
    workspace-member)
      printf '[workspace]\nmembers = ["crates/*", "python/trusty-architect"]\n' >"$t/Cargo.toml" ;;
    python-import) printf 'from trusty_example import config\n' >"$sub/scripts/bad.py" ;;
    crates-path) printf 'P = "../crates/trusty-mpm/src"\n' >"$sub/scripts/bad.py" ;;
    climb-out) printf 'sys.path.insert(0, "../../lib")\n' >"$sub/scripts/bad.py" ;;
    users-path) printf 'ROOT = "/Users/someone/work"\n' >"$sub/scripts/bad.py" ;;
    home-path) printf 'ROOT = "/home/someone/work"\n' >"$sub/scripts/bad.py" ;;
    claude-slug) printf 'D = "projects/-Users-someone-work"\n' >"$sub/scripts/bad.py" ;;
    private-checkout) printf 'see ~/trusty-mpm-projects/acme/supervisor\n' >"$sub/notes.md" ;;
    secret) printf 'TOKEN = "%s"\n' "$TOKEN" >"$sub/scripts/bad.py" ;;
    leak-*) plant_leak "$1" "$t" ;;
    drift) printf 'edited\n' >>"$t/$SKILL_DST" ;;
    drift:*) printf 'edited\n' >>"$t/${1#drift:}" ;;
    shipped-missing) rm "$t/$SKILL_DST" ;;
    # #9132: scripts/ still holds shipped copies, so the directory exists.
    unpaired) printf 'no source\n' >"$t/crates/trusty-mpm/src/assets/architect/scripts/extra.py" ;;
    # #9997: a content skill with no sub-project source is unguarded too.
    unpaired-skill) printf 'no source\n' >"$t/content/instructions/architect/skills/extra.md" ;;
    empty) rm -rf "$sub" && mkdir -p "$sub" ;;
    # #9822: the Rust crate crates/trusty-architect is not the sub-project.
    crate-dependency)
      printf '[workspace]\nmembers = ["crates/*"]\ndefault-members = ["crates/trusty-architect"]\n%s\n%s\n' \
        '[workspace.dependencies]' \
        'trusty-architect = { path = "crates/trusty-architect", version = "0.1" }' >"$t/Cargo.toml" ;;
  esac
}

# case<TAB>expected FAIL class
TAB="$(printf '\t')"
CASES="cargo-toml${TAB}CARGO_TOML
workspace-member${TAB}WORKSPACE_MEMBER
python-import${TAB}MAIN_TREE_IMPORT
crates-path${TAB}MAIN_TREE_IMPORT
climb-out${TAB}MAIN_TREE_IMPORT
users-path${TAB}OPERATOR_PATH
home-path${TAB}OPERATOR_PATH
claude-slug${TAB}OPERATOR_PATH
private-checkout${TAB}OPERATOR_PATH
secret${TAB}SECRET
drift${TAB}DRIFT
shipped-missing${TAB}DRIFT
unpaired${TAB}UNPAIRED
unpaired-skill${TAB}UNPAIRED
empty${TAB}EMPTY"
# #9996: every new class in the content tree and in the Python tree; the
# assets copy gets a representative pair.
for kind in owner_handle owner_first owner_short owner_nul owner_badutf8 org org_repo org_cli org_pages org_gitlab host_local host_local_port host_tailnet slack email gh_config gh_config_long tailnet_ip; do
  case "$kind" in
    owner_*) class=OWNER_NAME ;; org*) class=ORG ;; host_*) class=HOSTNAME ;;
    slack) class=SLACK_ID ;; email) class=EMAIL ;;
    gh_config*) class=GH_CONFIG_DIR ;; tailnet_ip) class=TAILNET_IP ;;
  esac
  for tree in content python; do
    CASES="$CASES
leak-${kind}-${tree}${TAB}${class}"
  done
done
CASES="$CASES
leak-owner_handle-assets${TAB}OWNER_NAME
leak-email-assets${TAB}EMAIL"
# One planted drift per shipped copy the gate lists.
for pair in $PAIRS; do
  CASES="$CASES
drift:${pair#*|}${TAB}DRIFT"
done

fail=0
pass=0

rc=0
out="$("$GATE" --root "$clean" 2>&1)" || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "FAIL: clean tree -> exit $rc (expected 0)" >&2
  printf '%s\n' "$out" | sed 's/^/       /' >&2
  fail=1
else
  pass=$((pass + 1))
fi

while IFS="$TAB" read -r name class; do
  [ -n "$name" ] || continue
  cp -R "$clean" "$WORK/$(case_dir "$name")"
  plant "$name"
  rc=0
  out="$("$GATE" --root "$WORK/$(case_dir "$name")" 2>&1)" || rc=$?
  if [ "$rc" -ne 1 ]; then
    echo "FAIL: $name -> exit $rc (expected 1)" >&2
    fail=1
  elif ! printf '%s\n' "$out" | grep -q "FAIL $class"; then
    echo "FAIL: $name -> output lacks 'FAIL $class'" >&2
    fail=1
  else
    pass=$((pass + 1))
    continue
  fi
  printf '%s\n' "$out" | sed 's/^/       /' >&2
done <<EOF
$CASES
EOF

# Planted trees that must stay clean (exit 0): a near miss the gate once
# reported (#9822: a root Cargo.toml naming the crate, not the sub-project).
for name in crate-dependency; do
  cp -R "$clean" "$WORK/$(case_dir "$name")"
  plant "$name"
  rc=0
  out="$("$GATE" --root "$WORK/$(case_dir "$name")" 2>&1)" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "FAIL: $name -> exit $rc (expected 0)" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
  else
    pass=$((pass + 1))
  fi
done

# #9996: near misses that must stay clean, in both trees. #10161: `.local` or
# `.lan` inside an identifier (localStorage, language-, LOCAL_URL) is no host.
for tree in content python; do
  name="near-miss-$tree"
  cp -R "$clean" "$WORK/$name"
  if [ "$tree" = content ]; then f="$WORK/$name/content/instructions/architect/near.md"
  else f="$WORK/$name/python/trusty-architect/near.md"; fi
  printf '%s\n' 'a bobcat and masala, Bobbin, robertson' \
    'mail user@example.com or ops@docs.example.org' \
    'see github.com/example/widgets and github.com/acme/tools' \
    'gh pr view 1 --repo bobmatnyc/trusty-tools; gh pr list -R acme/x --repo example/y' \
    'acme.github.io gitlab.com/example/x cp -R src/ dst' \
    'repo github.com/bobmatnyc/trusty-tools' \
    'localhost 127.0.0.1 .claude/settings.local.json' \
    'window.localStorage a.localeCompare(b) code.language-mermaid db.LOCAL_URL' \
    'threading.local() self.local.cache = 1' \
    'TEMPLATES_REFRESH CONTEXTUALIZE U+1F600' \
    '~/.config/gh-<account> gh-example ~/.config/gh-example/hosts.yml ~/.config/gh-acme' \
    '100.20.1.1 1.100.64.1 100.128.0.1 100.63.255.255' >"$f"
  rc=0
  out="$("$GATE" --root "$WORK/$name" 2>&1)" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "FAIL: $name -> exit $rc (expected 0)" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
  else
    pass=$((pass + 1))
  fi
done

# #9996 fail-closed arms: a gate that cannot scan must not pass.
closed() { # $1 name, $2 tree mutation, $3 env (VAR=val or -), $4 expected text, $5 expected exit
  local t="$WORK/closed-$1" rc=0 out pf="$WORK/closed-$1.txt" envs="$3"
  cp -R "$clean" "$t"
  case "$2" in
    rm-content) rm -rf "$t/content/instructions/architect" ;;
    rm-assets) rm -rf "$t/crates/trusty-mpm/src/assets/architect" ;;
    bad-regex) printf 'OWNER_NAME\ti\t(unclosed\n' >"$pf" ;;
    empty-regex) printf 'OWNER_NAME\ti\t\n' >"$pf" ;;
    garbage) printf 'only-one-field\n' >"$pf" ;;
    deny-hit)
      printf 'Zebediah\n' >"$pf"
      printf 'ask Zebediah\n' >"$t/content/instructions/architect/skills/d.md" ;;
    deny-clean) printf 'Zebediah\n' >"$pf" ;;
    none) ;;
  esac
  envs="${envs//@PF@/$pf}"
  if [ "$envs" = - ]; then
    out="$("$GATE" --root "$t" 2>&1)" || rc=$?
  else
    out="$(env "$envs" "$GATE" --root "$t" 2>&1)" || rc=$?
  fi
  if [ "$rc" -ne "$5" ] || ! printf '%s\n' "$out" | grep -q -- "$4"; then
    echo "FAIL: closed-$1 -> exit $rc (expected $5) with '$4'" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
  else
    pass=$((pass + 1))
  fi
}
closed patterns-missing none ARCHITECT_LEAK_PATTERNS=/nonexistent/patterns.txt 'FAIL PATTERNS' 1
closed pattern-garbage garbage ARCHITECT_LEAK_PATTERNS=@PF@ 'FAIL PATTERNS' 1
closed pattern-empty empty-regex ARCHITECT_LEAK_PATTERNS=@PF@ 'FAIL PATTERNS' 1
closed grep-error bad-regex ARCHITECT_LEAK_PATTERNS=@PF@ 'FAIL GREP_ERROR' 1
closed scan-root-content-missing rm-content - 'FAIL SCAN_ROOT' 1
closed scan-root-assets-missing rm-assets - 'FAIL SCAN_ROOT' 1
closed deny-file-missing none ARCHITECT_LEAK_DENY_FILE=/nonexistent/deny.txt 'FAIL PATTERNS' 1
closed deny-file-hit deny-hit ARCHITECT_LEAK_DENY_FILE=@PF@ 'FAIL OWNER_NAME' 1
closed deny-file-clean deny-clean ARCHITECT_LEAK_DENY_FILE=@PF@ 'check_architect_subproject: OK' 0
closed deny-unset-skipped none - 'check_architect_subproject: OK' 0

# #9996 fix round: a hit must never print a credential. Each line carries a
# token or password in URL userinfo; neither may appear in the output.
exposure() { # $1 name, $2 tree (content|python), $3 line, $4 secret text
  local t="$WORK/exposure-$1" rc=0 out
  cp -R "$clean" "$t"
  if [ "$2" = content ]; then f="$t/content/instructions/architect/skills/x.md"
  else f="$t/python/trusty-architect/x.md"; fi
  printf '%s\n' "$3" >"$f"
  out="$("$GATE" --root "$t" 2>&1)" || rc=$?
  if [ "$rc" -ne 1 ] || printf '%s\n' "$out" | grep -q -- "$4"; then
    echo "FAIL: exposure-$1 -> exit $rc (expected 1) or the output carries the credential" >&2
    printf '%s\n' "$out" | sed 's/^/       /' >&2
    fail=1
  else
    pass=$((pass + 1))
  fi
}
exposure token-content content "https://x-access-token:$TOKEN@github.com/acme/x" "$TOKEN"
exposure token-python python "https://x-access-token:$TOKEN@github.com/acme/x" "$TOKEN"
exposure password-content content 'https://deploy:Hunter2Secret@registry.corp.io/x' 'Hunter2Secret'
exposure password-python python 'https://deploy:Hunter2Secret@registry.corp.io/x' 'Hunter2Secret'

if [ "$fail" -ne 0 ]; then
  echo "check_architect_subproject selftest: FAILED ($pass cases passed)" >&2
  exit 1
fi
echo "check_architect_subproject selftest: OK ($pass cases passed)"
