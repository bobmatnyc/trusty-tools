# vmtest-harness/lib/source.sh — host->guest transport and install steps
# (DOC-1 §3.4, §6.1; DOC-2 §12.2).
#
# AT PLAN PHASE 3 THIS FILE CONTAINS `source_deliver_local` AND THE
# DIRTY-WORKTREE ASSERTIONS THAT PROVE IT. `source_deliver_branch` (P6-T1),
# `source_deliver_released`, `install_from_path` (P5-T1) and
# `install_from_registry` (P7-T1) land later.
#
# NAMING TENSION, RECORDED (DOC-2 §12.2). DOC-1 §3.4 calls this module "source
# delivery" while DOC-1 §12.1 wants reusable INSTALL-STEP functions, so
# `install_from_path` / `install_from_registry` will also live here. Read
# `source.sh` as "source acquisition and installation". A later split into
# `lib/install.sh` is permitted and would change no scenario, because scenarios
# call the functions, not the file.
#
# THE HOST REPO IS NEVER MOUNTED INTO THE GUEST, IN EITHER DIRECTION (DOC-1
# §6.4, §11). Source reaches the guest ONLY as a tar stream over the exec
# channel's stdin. Every host-side read below is read-only; the sole exception
# is the opt-in dirty-check fixture, which is held to the same discipline as the
# VM — created only after asserting the paths are clean, and restored on EVERY
# exit path by the driver's cleanup trap.
#
# This file never calls the virtualisation CLI directly (DOC-1 §3.2) — it goes
# through `lib/vm.sh`. `die`, `log`, `conf_get` are driver infrastructure
# (plan §F-5) and are shell-global by the time this file is sourced.
#
# CONVENTIONS (DOC-2 §12.1): positional string arguments; the return channel is
# the exit status; the value channel is stdout and carries AT MOST ONE VALUE —
# here, the streamed byte count, which DOC-1 §6.1 explicitly asks be logged;
# diagnostics ALWAYS to stderr, because §1's oracle parses stdout;
# THIS FILE DEFINES FUNCTIONS AND NOTHING ELSE.

# --- pattern (c): the local worktree (DOC-1 §6.1) --------------------------

# source_deliver_local <vm_name> <host_repo> <guest_dir>
# EMITS the streamed byte count on stdout. 0, or dies 50.
#
# `git ls-files -co --exclude-standard` is the right file set for two reasons,
# and both are asserted rather than assumed (see source_assert_dirty_delivery):
#   - it INCLUDES uncommitted work, which is the entire reason pattern (c)
#     exists rather than the already-measured pattern (b), which can only ever
#     deliver what has been pushed;
#   - it EXCLUDES gitignored paths BY CONSTRUCTION, so `target/` never enters
#     the payload — not via a hand-maintained exclude list that can rot.
source_deliver_local() {
    local vm="$1" host_repo="$2" guest_dir="$3"
    local files_host files_guest files_guest_type_f bytes t0 elapsed

    [ -e "$host_repo/.git" ] \
        || die 50 "host repo '$host_repo' is not a git worktree — pattern (c)'s file set is \`git ls-files -co --exclude-standard\` (DOC-1 §6.1)"

    log "host repo (READ-ONLY; NEVER mounted into the guest — DOC-1 §11): $host_repo"

    files_host=$(cd "$host_repo" && git ls-files -co --exclude-standard | wc -l | tr -d ' ') \
        || die 50 "could not enumerate the host file set in '$host_repo'"
    log "host file set (git ls-files -co --exclude-standard | wc -l): $files_host"

    vm_exec_raw "$vm" "rm -rf $guest_dir && mkdir -p $guest_dir" \
        || die 50 "could not prepare the guest source directory $guest_dir"

    t0=$(date '+%s')

    # `pipefail` is set by the driver. Without it a `tar` that fails mid-stream
    # is INVISIBLE whenever the exec stage exits 0 — a silently truncated tree
    # that then fails to build for an unrelated-looking reason (DOC-2 §Shell
    # discipline).
    #
    # `dd` is in the pipeline purely to count the bytes crossing it. Being an
    # ELEMENT of the pipeline rather than a `tee` into a process substitution,
    # its byte total is written before the pipeline returns, with no race.
    (
        cd "$host_repo" \
            && git ls-files -co --exclude-standard -z | tar -cf - --null -T -
    ) \
        | dd bs=1048576 2>"$VMTEST_TMPDIR/dd.err" \
        | vm_exec_stdin "$vm" "cd $guest_dir && tar -xf -" >/dev/null \
        || die 50 'the delivery pipeline failed (pipefail is set, so the status is the first non-zero stage)'

    elapsed=$(( $(date '+%s') - t0 ))

    bytes=$(awk '/bytes transferred/ { print $1; exit }' "$VMTEST_TMPDIR/dd.err")
    [ -n "$bytes" ] \
        || die 50 "could not read the streamed byte count: $(cat "$VMTEST_TMPDIR/dd.err" 2>/dev/null)"
    log "streamed ${bytes} bytes in ${elapsed}s"

    # `! -type d`, NOT `-type f`. This repo carries FOUR TRACKED SYMLINKS, which
    # `-type f` does not count, so the literal `-type f` check reports
    # G = H - 4 on a perfectly correct transfer. `! -type d` counts regular
    # files AND symlinks and is therefore the set comparable to
    # `git ls-files`'s. Corrected at plan P1-T6 on 2026-08-01 and inherited here
    # by P3-T4's first correction. Both are computed and logged; only the
    # comparable one is asserted.
    files_guest=$(vm_exec_raw "$vm" "find $guest_dir ! -type d | wc -l" | tr -d ' ')
    files_guest_type_f=$(vm_exec_raw "$vm" "find $guest_dir -type f | wc -l" | tr -d ' ')
    log "guest file set (find ! -type d):     $files_guest"
    log "guest file set (find -type f):       $files_guest_type_f  (regular files only; excludes tracked symlinks)"
    [ "$files_guest" -eq "$files_host" ] \
        || die 50 "delivered file count mismatch: host ${files_host} != guest ${files_guest}"
    log "file counts match: guest == host == ${files_host}"

    # `target/` absent BY CONSTRUCTION. Weaker than the dirty-check's sentinel 3
    # — it passes vacuously on a host that has never built — which is why the
    # sentinel exists as well.
    if vm_exec_raw "$vm" "[ -d $guest_dir/target ]" >/dev/null 2>&1; then
        die 50 "$guest_dir/target exists in the guest — --exclude-standard did not exclude the gitignored target/, and the payload would balloon from ~92 MB to tens of GB"
    fi
    log 'target/ absent in the guest, by construction'

    printf '%s\n' "$bytes"
}

# ---------------------------------------------------------------------------
# PATTERN (c)'S DEFINING PROPERTY — the three dirty-worktree assertions.
#
# PORTED FROM THE PHASE 1 SPIKE BY P3-T4, deliberately and under an explicit
# obligation: MANIFEST Phase 1 recorded that "deleting the spike without porting
# them would return this item to `open`". They are assertions about
# `source_deliver_local`, so they belong with it — and they now test the REAL
# function rather than a copy of its pipeline.
#
# DOC-1 §6.1 justifies `git ls-files -co --exclude-standard` on two claims, and
# each sentinel fails differently:
#
#   POSITIVE — it includes UNCOMMITTED work. This is the entire reason pattern
#   (c) exists rather than the slower, already-measured pattern (b).
#     sentinel 1 — a TRACKED file whose WORKING-TREE content differs from HEAD's.
#                  `-c` lists the path; `tar` must read the WORKTREE, not the
#                  index and not HEAD. AN IMPLEMENTATION BUILT ON
#                  `git archive HEAD` PASSES EVERY COUNT CHECK AND FAILS THIS
#                  ONE — which is why the assertion is on the whole file's
#                  `cksum`, not on the sentinel line's mere presence.
#     sentinel 2 — an UNTRACKED, non-ignored file. This is the `-o` half, which
#                  contributed exactly ZERO files to the 2026-07-31 clean run.
#
#   NEGATIVE — it excludes gitignored paths BY CONSTRUCTION. `--exclude-standard`
#   is what makes `-o` safe: without it, `-o` would enumerate `target/`.
#     sentinel 3 — a GITIGNORED file that must NOT arrive. Strictly stronger than
#                  `test -d target`, which passes vacuously on a host that has
#                  never built; this file is created by the fixture, so it
#                  cannot pass vacuously.
# ---------------------------------------------------------------------------

# source_dirty_fixture_create <tag>
# Dirties the HOST WORKTREE with three sentinel files. This is the one thing in
# the harness that mutates state outside the ephemeral VM, so it is held to the
# same discipline: it asserts the paths are clean first, asserts the host's own
# git classification of the two synthetic paths (so neither half of the check
# can be vacuous), and sets SRC_FIXTURES_CREATED BEFORE the first write, so a
# failure between the flag and the write still restores.
source_dirty_fixture_create() {
    local tag="$1" repo dirt
    repo="$VMTEST_HOST_REPO"

    SRC_FIX_TRACKED='vmtest-harness/tests/dirty-check-fixture.txt'
    SRC_FIX_UNTRACKED='vmtest-harness/tests/dirty-check-untracked.txt'
    SRC_FIX_IGNORED='vmtest-harness/tests/target/dirty-check-ignored.txt'
    SRC_SENT_TRACKED="VMTEST_DIRTY_SENTINEL_TRACKED_${tag}"
    SRC_SENT_UNTRACKED="VMTEST_DIRTY_SENTINEL_UNTRACKED_${tag}"
    SRC_SENT_IGNORED="VMTEST_DIRTY_SENTINEL_IGNORED_${tag}"

    [ -f "$repo/$SRC_FIX_TRACKED" ] \
        || die 50 "tracked fixture missing: $SRC_FIX_TRACKED — it must be COMMITTED for \`git ls-files -c\` to list it"

    # A `git checkout --` restore is only safe if the path had nothing to lose.
    dirt=$(cd "$repo" && git status --porcelain --ignored -- \
        "$SRC_FIX_TRACKED" "$SRC_FIX_UNTRACKED" "$SRC_FIX_IGNORED")
    [ -z "$dirt" ] || die 50 "the dirty-check fixture paths are not clean before the run; refusing to touch them:
$dirt"

    if (cd "$repo" && git check-ignore -q "$SRC_FIX_UNTRACKED"); then
        die 50 "$SRC_FIX_UNTRACKED is gitignored — the '-o' half of the check would be vacuous"
    fi
    if ! (cd "$repo" && git check-ignore -q "$SRC_FIX_IGNORED"); then
        die 50 "$SRC_FIX_IGNORED is NOT gitignored — the '--exclude-standard' half of the check would be vacuous"
    fi

    SRC_FIXTURES_CREATED=1
    printf '%s\n' "$SRC_SENT_TRACKED"   >> "$repo/$SRC_FIX_TRACKED"
    printf '%s\n' "$SRC_SENT_UNTRACKED" >  "$repo/$SRC_FIX_UNTRACKED"
    mkdir -p "$(dirname "$repo/$SRC_FIX_IGNORED")"
    printf '%s\n' "$SRC_SENT_IGNORED"   >  "$repo/$SRC_FIX_IGNORED"

    log "dirty-check fixture 1 (tracked, MODIFIED):   $SRC_FIX_TRACKED"
    log "dirty-check fixture 2 (untracked, expected): $SRC_FIX_UNTRACKED"
    log "dirty-check fixture 3 (gitignored, EXCLUDED): $SRC_FIX_IGNORED"
    log 'host git classification of the three fixtures (git status --porcelain --ignored):'
    (cd "$repo" && git status --porcelain --ignored -- \
        "$SRC_FIX_TRACKED" "$SRC_FIX_UNTRACKED" "$SRC_FIX_IGNORED") | sed 's/^/    | /' >&2
}

# source_dirty_fixture_restore — idempotent, and a no-op when no fixture was
# created. Called from the driver's cleanup trap, so it runs on EVERY exit path
# including failure and interrupt.
source_dirty_fixture_restore() {
    local repo dirt
    [ "${SRC_FIXTURES_CREATED:-0}" -eq 1 ] || return 0
    [ "${SRC_FIXTURES_RESTORED:-0}" -eq 0 ] || return 0
    SRC_FIXTURES_RESTORED=1
    repo="$VMTEST_HOST_REPO"

    rm -f "$repo/$SRC_FIX_UNTRACKED" "$repo/$SRC_FIX_IGNORED" || :
    rmdir "$(dirname "$repo/$SRC_FIX_IGNORED")" 2>/dev/null || :
    (cd "$repo" && git checkout -- "$SRC_FIX_TRACKED") \
        || { SRC_FIXTURE_RESTORE_FAILED=1; log "*** dirty-check fixture restore FAILED: git checkout -- $SRC_FIX_TRACKED ***"; }

    dirt=$(cd "$repo" && git status --porcelain) || dirt='<git status failed>'
    if [ -n "$dirt" ]; then
        SRC_FIXTURE_RESTORE_FAILED=1
        log '*** host worktree NOT clean after the dirty-check fixture restore — DO NOT COMMIT: ***'
        printf '%s\n' "$dirt" | sed 's/^/    | /' >&2
    else
        log 'dirty-check fixtures restored: git status --porcelain is empty'
    fi
}

# source_assert_dirty_delivery <vm_name> <guest_dir>
# The three assertions themselves. All are on CONTENT, not merely presence, so a
# truncated or HEAD-sourced transfer cannot satisfy them. Dies 50 on any failure.
source_assert_dirty_delivery() {
    local vm="$1" guest_dir="$2"
    local g_tracked g_untracked g_ignored out h_ck g_ck hits

    g_tracked="$guest_dir/$SRC_FIX_TRACKED"
    g_untracked="$guest_dir/$SRC_FIX_UNTRACKED"
    g_ignored="$guest_dir/$SRC_FIX_IGNORED"

    log '--- dirty-worktree assertions (pattern (c)'"'"'s defining property; ported from the Phase 1 spike by P3-T4) ---'

    # sentinel 1 — TRACKED + MODIFIED, PRESENT, with WORKTREE content.
    out=$(vm_exec_raw "$vm" "tail -1 $g_tracked") \
        || die 50 "sentinel 1 FAIL: the tracked fixture is ABSENT in the guest ($g_tracked)"
    [ "$out" = "$SRC_SENT_TRACKED" ] \
        || die 50 "sentinel 1 FAIL: the guest copy's last line is '$out', expected '$SRC_SENT_TRACKED' — the stream carried HEAD content, not WORKTREE content"
    log "sentinel 1 PRESENT (tracked, modified): $out"

    # Whole-file equality, not just the sentinel line — this is the assertion a
    # `git archive HEAD` implementation cannot satisfy. `cksum` is POSIX and both
    # ends are macOS, so the two outputs are directly comparable.
    h_ck=$(cksum < "$VMTEST_HOST_REPO/$SRC_FIX_TRACKED")
    g_ck=$(vm_exec_raw "$vm" "cksum < $g_tracked")
    [ "$h_ck" = "$g_ck" ] \
        || die 50 "sentinel 1 FAIL: whole-file cksum host '$h_ck' != guest '$g_ck'"
    log "sentinel 1 content matches the host EXACTLY (whole-file cksum $g_ck)"

    # sentinel 2 — UNTRACKED, non-ignored, PRESENT. The `-o` half.
    out=$(vm_exec_raw "$vm" "cat $g_untracked") \
        || die 50 "sentinel 2 FAIL: the untracked fixture is ABSENT in the guest ($g_untracked) — the '-o' half of the file set does not work"
    [ "$out" = "$SRC_SENT_UNTRACKED" ] \
        || die 50 "sentinel 2 FAIL: guest content is '$out', expected '$SRC_SENT_UNTRACKED'"
    log "sentinel 2 PRESENT (untracked, not ignored): $out"

    # sentinel 3 — GITIGNORED, ABSENT. Three independent checks.
    if vm_exec_raw "$vm" "[ -e $g_ignored ]" >/dev/null 2>&1; then
        die 50 "sentinel 3 FAIL: the GITIGNORED fixture ARRIVED at $g_ignored — --exclude-standard is not excluding, and target/ would follow"
    fi
    log "sentinel 3 ABSENT (the gitignored path is not present): $g_ignored"

    if vm_exec_raw "$vm" "[ -d $guest_dir/vmtest-harness/tests/target ]" >/dev/null 2>&1; then
        die 50 'sentinel 3 FAIL: the ignored directory vmtest-harness/tests/target/ arrived'
    fi
    log 'sentinel 3 ABSENT (its ignored parent directory is not present either)'

    hits=$(vm_exec_raw "$vm" "grep -rl '$SRC_SENT_IGNORED' $guest_dir 2>/dev/null | head -5" || true)
    [ -z "$hits" ] \
        || die 50 "sentinel 3 FAIL: the ignored sentinel leaked into the delivered tree at: $hits"
    log 'sentinel 3 ABSENT (grep -rl over the whole delivered tree found 0 occurrences)'

    log 'DIRTY_CHECK PASS — pattern (c) delivers uncommitted work and still excludes ignored paths'
}
