# vmtest-harness/lib/source.sh — host->guest transport and install steps
# (DOC-1 §3.4, §6.1; DOC-2 §12.2).
#
# AT PLAN PHASE 7 THIS FILE IS COMPLETE: `source_deliver_local` and the
# DIRTY-WORKTREE ASSERTIONS THAT PROVE IT, `source_deliver_branch` (P6-T1),
# `source_deliver_released` and `install_from_registry` (P7-T1), and
# `install_from_path` (P5-T1). All three delivery functions and both install
# steps of DOC-2 §12.2 now exist.
#
# NAMING TENSION, RECORDED (DOC-2 §12.2). DOC-1 §3.4 calls this module "source
# delivery" while DOC-1 §12.1 wants reusable INSTALL-STEP functions, so
# `install_from_path` / `install_from_registry` will also live here. Read
# `source.sh` as "source acquisition and installation". A later split into
# `lib/install.sh` is permitted and would change no scenario, because scenarios
# call the functions, not the file.
#
# THE HOST REPO IS NEVER MOUNTED INTO THE GUEST, IN EITHER DIRECTION (DOC-1
# §6.4, §11). Under pattern (c) source reaches the guest ONLY as a tar stream
# over the exec channel's stdin; under pattern (b) it does not cross from the
# host AT ALL — the guest clones the public repository itself (DOC-1 §6.2), and
# `source_deliver_branch` has no host path argument to read. Every host-side read
# below is therefore pattern (c)'s alone; the sole exception
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

# --- pattern (b): a branch of the public repo (DOC-1 §6.2) -----------------

# source_deliver_branch <vm_name> <repo_url> <branch> <guest_dir>
# 0, or dies 50 — §12.2's declared signature, with NO stdout emit. The resolved
# commit SHA is LOGGED to stderr like every other diagnostic (§12.1); it was
# emitted on stdout until 2026-08-04, which put all ten `die 50` calls inside the
# caller's command substitution and classified none of them (#16).
#
# ============================================================================
# NO HOST->GUEST BYTE STREAM EXISTS ON THIS PATH, AND THAT IS THE POINT.
#
# DOC-1 §6.2: "The guest performs `git clone` directly (the repo is public, so no
# credential plumbing is needed), checks out the target branch, and runs
# `cargo install --path` per crate. No host->guest source transfer occurs; the
# host repository is not read." This function therefore takes NO host path
# argument — `source_deliver_local`'s `<host_repo>` has no counterpart here —
# and touches `$VMTEST_HOST_REPO` nowhere. That is a structural guarantee, not a
# discipline: there is no host path in scope to read.
#
# The phase checkpoint asserts the absence mechanically (`grep -c 'streamed'` in
# the run log is 0), which is why nothing on this path may borrow
# `source_deliver_local`'s vocabulary.
#
# NO NEW MECHANISM (plan §A). Pattern (b) reuses pattern (c)'s scaffolding
# entirely: the same `lib/vm.sh` exec boundary, the same `run_watchdog`, the same
# `install_from_path` install step, the same oracle. Only the acquisition of the
# tree differs, and it differs by being done BY THE GUEST.
#
# THE GUEST'S TREE IS NOT THE HOST'S WORKING TREE, AND SEVERAL THINGS DEPEND ON
# THAT BEING UNDERSTOOD. `branch` is `default_branch` (DOC-2 §8.2), so the tree
# built here is whatever that branch carries on the remote — it need not equal,
# and in general does not equal, the checkout the driver is running from. Two
# consequences, both already handled at source rather than here:
#   - DOC-2 §1.2's version cross-check reads `source_tree_version` GUEST-SIDE at
#     `guest_src_dir` precisely for this reason. Host-side reading is equivalent
#     under (c) by construction and WRONG under (b). `verify_versions` already
#     does the right thing; this is the pattern that first makes the difference
#     real rather than theoretical.
#   - the install loop iterates `tsv_scope_crate_dirs`, derived from the HOST's
#     `expected-binaries.tsv`. A crate directory the cloned branch does not carry
#     fails in `install_from_path`, where `cd` fails and `&&` (never `;`, §7.4)
#     stops cargo running in the wrong directory. That is a correct, classified
#     failure and it is deliberately NOT pre-checked here: a table/branch
#     disagreement is a finding about the branch, and the failure should name the
#     crate it happened on.
#
# `tctl install` IS BANNED ON THIS PATH exactly as it is under (c) (DOC-1 §6.5).
# The prohibition is about the INSTALL step, which is shared, so it is stated and
# enforced in `install_from_path`; it is repeated here only so that a reader
# writing a new pattern-(b) step does not conclude the ban was pattern-(c)'s.
# ============================================================================
source_deliver_branch() {
    local vm="$1" repo_url="$2" branch="$3" guest_dir="$4"
    local t0 elapsed rc sha head_branch files

    [ -n "$repo_url" ] || die 50 'source_deliver_branch: empty repo_url (DOC-2 §8.2 key repo_url)'
    [ -n "$branch" ]   || die 50 'source_deliver_branch: empty branch (DOC-2 §8.2 key default_branch)'

    log "guest-side clone (NO host->guest byte stream; the host repository is not read at all — DOC-1 §6.2, §11): ${repo_url} branch ${branch}"

    # The clone target must not pre-exist: `git clone` refuses a non-empty
    # directory. Removing it is the same preparation `source_deliver_local` does,
    # for the same reason — a run must not build on a previous run's tree.
    vm_exec_raw "$vm" "rm -rf $guest_dir" \
        || die 50 "could not prepare the guest source directory $guest_dir"

    # ------------------------------------------------------------------
    # 300 s, DOC-2 §10.2's `guest git clone (pattern b)` row, grounded in the
    # measured GIT_CLONE_MS=50131 (~6x). §10.2's `§8.2 key` column records this
    # budget as "none — built-in", so §10.3's requirement that a timeout message
    # name the key that changes it is satisfied by saying that NO key exists —
    # naming one that does not would be worse than naming none.
    #
    # CLONE THEN CHECKOUT, IN THAT ORDER AND AS TWO STEPS, not `clone --branch`.
    # DOC-1 §6.2 describes both actions and the checkpoint requires the run log
    # to show "the checked-out branch name"; an explicit checkout is what makes
    # the branch an OBSERVED state of the tree rather than an argument the harness
    # passed and never confirmed. It also leaves every remote branch fetched, so
    # `default_branch` can be overridden to a branch that is not the remote HEAD
    # with no change here.
    #
    # `&&` between every stage, never `;` (DOC-2 §7.4): a failed clone must not be
    # followed by a `cd` into a directory that does not exist and a checkout in
    # whatever directory the shell happened to be in.
    # ------------------------------------------------------------------
    t0=$(date '+%s')
    rc=0
    run_watchdog 300 "$VMTEST_TMPDIR/git-clone.log" \
        vm_exec "$vm" "git clone ${repo_url} ${guest_dir} && cd ${guest_dir} && git checkout ${branch}" || rc=$?
    elapsed=$(( $(date '+%s') - t0 ))

    if [ "$rc" -ne 0 ]; then
        tail -40 "$VMTEST_TMPDIR/git-clone.log" 2>/dev/null | sed 's/^/    | /' >&2 || :
        if [ "$rc" -eq 124 ]; then
            die 50 "the guest \`git clone\` of ${repo_url} exceeded its 300 s budget (DOC-2 §10.2 guest-git-clone row; NO vmtest.defaults key exists for this budget). No retry, ever (§10.3)."
        fi
        die 50 "the guest \`git clone ${repo_url}\` / \`git checkout ${branch}\` exited ${rc} (last 40 lines above). The repository is public (DOC-1 §6.2), so this is not a credentials failure."
    fi
    log "MEASURE git_clone_s ${elapsed} (measured baseline GIT_CLONE_MS=50131, i.e. 50.131 s)"

    # ------------------------------------------------------------------
    # Read the delivered tree back and ASSERT what it is. The clone's own exit
    # status says the command succeeded; it does not say which commit is checked
    # out, and DOC-1 §8.1's "an exit code is not a completion signal" is the same
    # discipline applied to a different tool.
    # ------------------------------------------------------------------
    vm_exec_raw "$vm" "[ -d ${guest_dir}/.git ]" >/dev/null 2>&1 \
        || die 50 "${guest_dir}/.git does not exist in the guest after a \`git clone\` that exited 0"

    head_branch=$(vm_exec "$vm" "cd ${guest_dir} && git rev-parse --abbrev-ref HEAD") \
        || die 50 "could not read the checked-out branch name at ${guest_dir}"
    sha=$(vm_exec "$vm" "cd ${guest_dir} && git rev-parse HEAD") \
        || die 50 "could not resolve HEAD at ${guest_dir}"
    [ -n "$sha" ] || die 50 "\`git rev-parse HEAD\` at ${guest_dir} produced nothing"

    # The requested branch is what must be checked out. A clone that silently
    # left the remote's default branch checked out would build the wrong tree and
    # every downstream assertion would pass against it.
    [ "$head_branch" = "$branch" ] \
        || die 50 "the guest checked out '${head_branch}', not the requested branch '${branch}' (DOC-2 §8.2 key default_branch)"

    files=$(vm_exec_raw "$vm" "find $guest_dir -path $guest_dir/.git -prune -o ! -type d -print | wc -l" | tr -d ' ')

    log "checked-out branch: ${head_branch}   [the branch under test; select it with VMTEST_DEFAULT_BRANCH — DOC-2 §8.2's mechanical override mapping, NOT a --branch flag]"
    log "resolved commit SHA: ${sha}"
    log "guest working tree at ${guest_dir}: ${files} files (excluding .git)"
    log 'THE HOST REPOSITORY WAS NOT READ: pattern (b) has no host path argument and no host->guest transfer (DOC-1 §6.2).'
    # #16: NO stdout emit. DOC-2 §12.2 declares this function "0 or dies 50"
    # with no emit; the `printf '%s\n' "$sha"` that used to close it was an
    # undeclared value channel whose only consumer rebuilt a log line this
    # function already logs one line above — so ten `die 50` calls ran inside
    # `_sha=$(source_deliver_branch …)`'s subshell and classified nothing.
}

# --- pattern (a): the registry (DOC-1 §6.3, D1) ----------------------------

# source_deliver_released
# A NO-OP RETURNING 0. Takes no arguments and touches no VM.
#
# ============================================================================
# IT EXISTS SO THE SCENARIOS STAY SYMMETRIC (DOC-2 §12.2, verbatim: "no-op
# returning 0; pattern (a) has no delivery step; exists so scenarios stay
# symmetric"). DO NOT DELETE IT AS DEAD CODE.
#
# The symmetry is not decoration. DOC-1 §12.1's upgrade-testing extension is
# "two install steps in one scenario file, and not a new mechanism" — that
# composability only holds while all three scenario files have the same SHAPE, so
# an upgrade scenario can be written by pairing any delivery with any install
# step. A pattern-(a) scenario whose step 1 was simply missing would make
# `install-released.sh` structurally different from its two siblings, and the
# first upgrade scenario would then have to invent the missing slot.
#
# PATTERN (a) IS crates.io AND NOTHING ELSE (DOC-1 D1). `install.sh` and prebuilt
# release tarballs are OUT OF SCOPE — settled, not pending. The crates.io path is
# the only one grounded in measurement (`cargo install tga --locked`, 131 s, 211
# deps, 4 vCPU), and it is the only one where "what the user gets" is a version
# this harness can name.
# ============================================================================
source_deliver_released() {
    log 'pattern (a): NO delivery step, by construction (DOC-2 §12.2). The source never crosses from the host and the guest never clones — `cargo install <package> --locked` fetches each package from crates.io (DOC-1 §6.3, D1). This no-op is called anyway, so the three scenario files stay symmetric.'
    return 0
}

# --- install steps (DOC-2 §12.2; DOC-1 §7.3, §7.4, §8.4, §8.6, §6.5) -------

# install_from_path <vm_name> <guest_dir> <crate_dir>
# Asserts `rustc --version` FIRST, then installs. 0, or dies 50.
#
# ============================================================================
# THE ONE RULE THIS FUNCTION EXISTS TO HOLD: IT INSTALLS A **PACKAGE**.
# NEVER `--bin`. NEVER `--bins` WITH A FILTER. (DOC-2 §12.2, amended
# 2026-07-31; mirrored in plan P5-T1; the hazard is named in plan §F-3.)
#
# `expected-binaries.tsv`'s `binary` column is the ORACLE's input. It is never
# the INSTALLER's. A "row-faithful" loop — `cargo install --path <dir> --bin
# <binary>`, once per TSV row — looks like tidying up and is a change in
# meaning: DOC-1 §7.4's Single-Install Convention gate asserts exactly one
# thing, that ONE package-granular install yields EVERY sidecar. Install each
# sidecar by name and `verify_binaries` reports 13/13 while
# `verify_single_install` passes four times over, and NOTHING has tested the
# convention. A crate that silently stopped shipping a sidecar would still show
# green. Unlike a missing table row, `--check-table` cannot catch it, because
# the table is not what is wrong. There is no accessor in this harness that
# emits `(crate_dir, binary)` pairs, precisely so this loop cannot be written
# by accident — see `tsv_scope_binaries` in the driver.
#
# `tctl install` IS BANNED HERE (DOC-1 §6.5). `install_one()` in
# `crates/trusty-installer/src/commands/install.rs` is prebuilt-tarball-first
# with a crates.io `cargo install --locked` fallback and has NO `--path` code
# path, so invoking it during a source-based scenario would overwrite the
# source-built binaries under test with RELEASED artefacts — a false pass, the
# worst failure mode a harness has.
#
# NEVER `cp` A BINARY INTO A PATH DIRECTORY (DOC-1 §7.3): copying a Mach-O
# binary is not installing it, and cdhash-dependent behaviour (TCC attribution,
# keychain ACLs, notarisation) does not survive an arbitrary copy.
# ============================================================================
install_from_path() {
    local vm="$1" guest_dir="$2" crate_dir="$3"
    local crate_path expected rustc_line t0 elapsed rc bins now

    # `crate_dir` IS RELATIVE TO `crates/`, NOT TO THE REPOSITORY ROOT.  §9.1
    # defines the column as "directory under `crates/`", `--check-table` derives
    # it by stripping exactly `<workspace_root>/crates/`, and §7.4's worked
    # invocation spells the guest path out in full:
    #     cd /Users/admin/vmtest-src/crates/trusty-git-analytics && rustc --version
    # Joining `guest_dir` to `crate_dir` directly produces
    # `/Users/admin/vmtest-src/trusty-search`, which does not exist — observed on
    # the first Phase 5 run, where `cd` failed and `&&` (never `;`, §7.4) stopped
    # the command from running in the wrong directory, exactly as designed.
    crate_path="${guest_dir}/crates/${crate_dir}"

    # The canonical log line the P5 checkpoint and P5-T8's tripwire both count.
    # It is ALSO appended to a run-scoped ledger, because the count has to be
    # answerable from data the harness owns rather than from whatever the
    # operator happened to redirect stderr into — see the ledger note in
    # `scenarios/install-local.sh`.
    log "install_from_path ${crate_dir}"
    printf '%s\n' "$crate_dir" >> "$VMTEST_RUNDIR/installs.log" \
        || die 50 "could not append to the install ledger $VMTEST_RUNDIR/installs.log"

    # §10.2's full-stack budget, enforced as a deadline (see `scenario_dispatch`
    # for why it is not a wrapper). §10.3 clause 3: name the condition, the
    # elapsed time, the budget and the key that changes it.
    now=$(date '+%s')
    if [ -n "${INSTALL_DEADLINE_EPOCH:-}" ] && [ "$now" -ge "$INSTALL_DEADLINE_EPOCH" ]; then
        die 50 "the full-stack scenario budget of $(conf_get install_timeout)s was exhausted before '${crate_dir}' could be installed (change it with vmtest.defaults key install_timeout; DOC-2 §10.2 full-stack row). No retry, ever (§10.3)."
    fi

    # ------------------------------------------------------------------
    # DOC-1 §8.4's per-build-step assertion, adjacent to the build.
    #
    # WHAT `expected` IS, AND WHY IT IS NOT ALWAYS THE WORKSPACE PIN.
    # DOC-2 §12.2 gives `verify_rustc <vm> <dir> <expected>` an expected
    # argument "rather than assuming 1.91.1 everywhere", and measurement K5 is
    # why: `crates/trusty-git-analytics/rust-toolchain.toml` pins
    # `channel = "stable"`, which resolved to rustc 1.97.1 INSIDE that crate
    # against the workspace's 1.91.1 at the root. rustup resolves by current
    # directory, so the two are both correct and both real.
    #
    # The expectation is therefore derived from the same thing rustup resolves
    # against — the presence of a crate-local `rust-toolchain.toml`:
    #   - no crate-local file -> the crate inherits the workspace toolchain, and
    #     `expected` is `toolchain.tsv`'s measured `rustc_version`. ASSERTED for
    #     equality; a mismatch dies 50.
    #   - a crate-local file  -> the crate overrides the workspace pin with a
    #     CHANNEL (`stable`), not a version, so no literal can be predicted
    #     host-side without resolving rustup's channel ourselves. `expected` is
    #     passed EMPTY, which `verify_rustc` treats as "assert that rustc
    #     resolves and reports a version, do not assert WHICH" — and the K5
    #     comparison against the workspace pin is logged loudly right here.
    # Inventing a literal for the override case would be pinning a number this
    # harness cannot derive; asserting the workspace pin for it would fail the
    # run on a toolchain difference that is the crate's declared intent.
    # ------------------------------------------------------------------
    local workspace_rustc
    workspace_rustc=$(tsv_get "$VMTEST_RUNDIR/toolchain.tsv" rustc_version) \
        || die 50 "no rustc_version in $VMTEST_RUNDIR/toolchain.tsv (DOC-2 §7.1) — cannot form DOC-1 §8.4's expectation for '${crate_dir}'"

    if vm_exec "$vm" "[ -f ${crate_path}/rust-toolchain.toml ]" >/dev/null 2>&1; then
        log "rustc(${crate_dir}): crate declares its OWN rust-toolchain.toml — it overrides the workspace pin ${workspace_rustc} (DOC-1 §8.4, measurement K5); asserting resolution, not a literal"
        expected=''
    else
        expected="$workspace_rustc"
    fi

    # #16: a PLAIN call. As `rustc_line=$(verify_rustc …)` all three of the
    # function's `die 50` calls ran inside the substitution's subshell: the run
    # aborted (the assignment takes the substitution's status) but VMTEST_EXIT
    # was written in a child and lost, so the MEASURE line reported `exit 0` and
    # a later teardown `die 70` could claim the slot §2 reserves for the first
    # classified failure. The resolved line comes back in RUSTC_LAST_LINE.
    verify_rustc "$vm" "$crate_path" "$expected"
    rustc_line="${RUSTC_LAST_LINE:-}"

    if [ -z "$expected" ]; then
        case "$rustc_line" in
            *"$workspace_rustc"*)
                log "*** FINDING: ${crate_dir} declares its own rust-toolchain.toml but resolved to the WORKSPACE version ${workspace_rustc} — measurement K5's toolchain drift did NOT reproduce. Recorded, not smoothed over (plan P5-T1 acceptance). ***" ;;
            *)
                log "rustc(${crate_dir}): K5 REPRODUCED — '${rustc_line}' differs from the workspace pin ${workspace_rustc}" ;;
        esac
    fi

    # ------------------------------------------------------------------
    # The build. PACKAGE GRANULARITY — see the banner above.
    #
    #   - `cd` INTO the crate directory and `&&`, not `;` (DOC-2 §7.4): rustup
    #     resolves by current directory, and a failed `cd` must not run cargo in
    #     the wrong one. `--path .` therefore names the directory we just proved
    #     we are in, rather than re-deriving it.
    #   - `--locked` is the same reproducibility discipline DOC-1 §6.3 applies to
    #     pattern (a): build the dependency set the committed `Cargo.lock`
    #     names, not whatever resolves today. Under pattern (c) the lockfile
    #     arrived in the same stream as the source, so it is the tree's own.
    #   - PATH, CARGO_TARGET_DIR (DOC-1 §8.6's single shared target directory)
    #     and SKIP_UI_BUILD ride in $VMTEST_GUEST_ENV, composed in `vm_exec` and
    #     nowhere else (DOC-2 §7.3).
    #   - 900 s per crate, DOC-2 §10.2's single-crate row. NO vmtest.defaults key
    #     exists for it and §10.3's amendment requires the message to say so
    #     rather than name one that does not exist.
    # ------------------------------------------------------------------
    log "cargo install --path ${crate_path} (PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)"
    t0=$(date '+%s')
    rc=0
    run_watchdog 900 "$VMTEST_TMPDIR/install-${crate_dir}.log" \
        vm_exec "$vm" "cd ${crate_path} && cargo install --path . --locked" || rc=$?
    elapsed=$(( $(date '+%s') - t0 ))

    if [ "$rc" -ne 0 ]; then
        tail -40 "$VMTEST_TMPDIR/install-${crate_dir}.log" 2>/dev/null | sed 's/^/    | /' >&2 || :
        if [ "$rc" -eq 124 ]; then
            die 50 "\`cargo install --path\` for '${crate_dir}' exceeded its 900 s budget (DOC-2 §10.2 single-crate row; NO vmtest.defaults key exists for this budget). No retry, ever (§10.3)."
        fi
        die 50 "\`cargo install --path .\` in '${crate_path}' exited ${rc} (last 40 lines above)"
    fi

    # Cargo names every binary it placed on `Installed`/`Replacing` lines. They
    # are the direct evidence for DOC-1 §7.4 — that ONE package-granular install
    # produced the whole sidecar set — so they are logged where the reader can
    # see them beside the command that produced them, rather than left in a
    # temporary file cleanup deletes.
    bins=$(awk '/^ +(Installed|Replacing) /' "$VMTEST_TMPDIR/install-${crate_dir}.log" 2>/dev/null | sed 's/^ *//' | tr '\n' '; ')
    log "MEASURE install_s ${crate_dir} ${elapsed}"
    log "installed ${crate_dir} in ${elapsed}s: ${bins:-<no Installed/Replacing lines>}"
}

# install_from_registry <vm_name> <package> [version]
# Asserts `rustc --version` FIRST, then installs from crates.io. 0, or dies 50.
#
# ============================================================================
# THE KEY IS THE **PACKAGE NAME**, NOT THE DIRECTORY. (DOC-2 §9.2: `[package]
# name` is "what `cargo install <name> --locked` takes, which is pattern (a)'s
# entire interface".) That is why the caller drives this from
# `tsv_scope_packages` and NEVER from `tsv_scope_crate_dirs`:
#
#     crates/trusty-git-analytics/  publishes as  **tga**
#
# `cargo install trusty-git-analytics --locked` does not exist on crates.io. This
# discontinuity between directory name and package name is exactly what DOC-1 D3
# warns about and the whole reason `expected-binaries.tsv` carries BOTH columns.
# A pattern-(a) loop written over crate directories fails on one crate out of
# eight, and it fails at the LAST possible moment — after seven successful
# multi-minute installs.
#
# `--locked` IS MANDATORY, AND IT IS NOT A STYLE CHOICE. Default `cargo install`
# RE-RESOLVES the dependency graph and IGNORES the lockfile the package was
# published with; it will happily pair a published crate's source with a NEWER
# version of a sibling library that has since been released. That is not
# theoretical in this workspace: `cargo install trusty-analyze` has failed with
# **E0063** (missing struct fields) from precisely that pairing — old
# `trusty-analyze` source against a newer `trusty-common`. `--locked` builds the
# graph the publisher tested. If an install still fails this way WITH `--locked`,
# that is a genuine product finding about a published lockfile and it is RECORDED
# rather than worked around.
#
# THE SAME PACKAGE-GRANULARITY RULE AS `install_from_path`, FOR THE SAME REASON:
# NEVER `--bin`, NEVER `--bins` WITH A FILTER (DOC-2 §12.2). The TSV's `binary`
# column is the ORACLE's input and never the INSTALLER's. `cargo install
# trusty-mpm --locked` must yield BOTH `tm` and `trusty-mpm`, and DOC-1 §7.4's
# Single-Install Convention gate is the assertion that it did — an assertion
# worth nothing the moment the installer is handed the binary names.
#
# NO `tctl install` HERE EITHER, THOUGH IT IS NOT BANNED ON THIS PATH. DOC-1 §6.5
# bans it from patterns (b)/(c); under (a) it would do roughly what this function
# does. The harness still calls `cargo install` directly (plan P7-T2) SO THAT ALL
# THREE PATTERNS SHARE ONE INSTALL MECHANISM AND DIFFER ONLY IN SOURCE. A
# pattern-(a) run that went through `tctl install` would be testing the
# installer's tarball-first path, not the registry, and the three patterns would
# no longer be comparable.
# ============================================================================
install_from_registry() {
    local vm="$1" pkg="$2" version="${3:-}"
    local spec t0 elapsed rc bins now workspace_rustc guest_home

    [ -n "$pkg" ] || die 50 'install_from_registry: empty package name (DOC-2 §12.2)'

    # The canonical log line the P7 checkpoint counts, and the ledger
    # `install_assert_install_count` counts — the same ledger `install_from_path`
    # writes, so one tripwire covers all three patterns.
    log "install_from_registry ${pkg}"
    printf '%s\n' "$pkg" >> "$VMTEST_RUNDIR/installs.log" \
        || die 50 "could not append to the install ledger $VMTEST_RUNDIR/installs.log"

    # §10.2's full-stack budget, enforced as a deadline (see `scenario_dispatch`).
    # §10.3 clause 3: name the condition, the elapsed time, the budget and the key.
    now=$(date '+%s')
    if [ -n "${INSTALL_DEADLINE_EPOCH:-}" ] && [ "$now" -ge "$INSTALL_DEADLINE_EPOCH" ]; then
        die 50 "the full-stack scenario budget of $(conf_get install_timeout)s was exhausted before '${pkg}' could be installed (change it with vmtest.defaults key install_timeout; DOC-2 §10.2 full-stack row). No retry, ever (§10.3)."
    fi

    # ------------------------------------------------------------------
    # DOC-1 §8.4's per-build-step assertion. A registry install IS a build step,
    # so it gets one.
    #
    # THE DIRECTORY IS `guest_home`, AND THAT IS NOT A WEAKER CHOICE — IT IS THE
    # ONLY CORRECT ONE. rustup resolves by CURRENT DIRECTORY, which is why
    # `install_from_path` cd's into the crate. Pattern (a) has NO crate directory
    # in the guest: cargo unpacks the published crate into its own temporary
    # scratch and builds there. The directory that governs the toolchain is
    # therefore the one cargo is invoked FROM, which is the guest home — the same
    # directory `provision.sh` measured `rustc_version` in (`cd ${guest_home} &&
    # rustc --version`, provision.sh:203). Asserting equality against
    # `toolchain.tsv`'s value is therefore an assertion about the same resolution
    # site, not an approximation of one.
    #
    # NO CRATE-LOCAL OVERRIDE BRANCH EXISTS HERE, and measurement K5 is why that
    # is correct rather than an omission: `trusty-git-analytics`'s
    # `rust-toolchain.toml` governs builds run INSIDE its directory. A published
    # `tga` built from a temporary unpack directory under the guest home is not
    # such a build, so K5's 1.97.1 is not expected on this path and its absence is
    # not drift. It is logged either way by `verify_rustc`.
    # ------------------------------------------------------------------
    guest_home=$(conf_get guest_home)
    workspace_rustc=$(tsv_get "$VMTEST_RUNDIR/toolchain.tsv" rustc_version) \
        || die 50 "no rustc_version in $VMTEST_RUNDIR/toolchain.tsv (DOC-2 §7.1) — cannot form DOC-1 §8.4's expectation for '${pkg}'"
    # #16: no `>/dev/null` — this function no longer writes to stdout, and the
    # redirection was suppressing nothing.
    verify_rustc "$vm" "$guest_home" "$workspace_rustc"

    # §12.2's optional third argument. Unused by today's scenario — every package
    # installs at its published maximum — but it is the signature the contract
    # gives, and DOC-1 §12.1's upgrade extension is the caller that will use it.
    spec="$pkg"
    [ -z "$version" ] || spec="${pkg} --version ${version}"

    # ------------------------------------------------------------------
    # The install. PACKAGE GRANULARITY — see the banner above.
    #
    #   - PATH, CARGO_TARGET_DIR (DOC-1 §8.6's single shared target directory) and
    #     SKIP_UI_BUILD ride in $VMTEST_GUEST_ENV, composed in `vm_exec` and
    #     nowhere else (DOC-2 §7.3).
    #   - 900 s per package, DOC-2 §10.2's single-crate row. NO vmtest.defaults key
    #     exists for it and §10.3's amendment requires the message to say so
    #     rather than name one that does not exist.
    #   - There is no `cd` and no `&&` chain: unlike `install_from_path` there is
    #     no directory whose existence has to be proven before cargo runs.
    # ------------------------------------------------------------------
    log "cargo install ${spec} --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)"
    t0=$(date '+%s')
    rc=0
    run_watchdog 900 "$VMTEST_TMPDIR/install-registry-${pkg}.log" \
        vm_exec "$vm" "cargo install ${spec} --locked" || rc=$?
    elapsed=$(( $(date '+%s') - t0 ))

    if [ "$rc" -ne 0 ]; then
        tail -40 "$VMTEST_TMPDIR/install-registry-${pkg}.log" 2>/dev/null | sed 's/^/    | /' >&2 || :
        if [ "$rc" -eq 124 ]; then
            die 50 "\`cargo install ${spec} --locked\` exceeded its 900 s budget (DOC-2 §10.2 single-crate row; NO vmtest.defaults key exists for this budget). No retry, ever (§10.3)."
        fi
        die 50 "\`cargo install ${spec} --locked\` exited ${rc} (last 40 lines above). If the failure is 'could not find \`${pkg}\` in registry', the package is NOT PUBLISHED and that is a DESIGN-LEVEL FINDING about DOC-1 D2/D3's scope, not a harness bug to work around (plan P7-T4). If it is a COMPILE error, record it verbatim: \`--locked\` is present precisely to build the graph the publisher tested, so a compile failure under it is a finding about the PUBLISHED lockfile."
    fi

    # Cargo names every binary it placed on `Installed`/`Replacing` lines. Direct
    # evidence for DOC-1 §7.4 — that ONE package-granular install produced the
    # whole sidecar set — logged beside the command that produced it.
    bins=$(awk '/^ +(Installed|Replacing) /' "$VMTEST_TMPDIR/install-registry-${pkg}.log" 2>/dev/null | sed 's/^ *//' | tr '\n' '; ')
    log "MEASURE install_s ${pkg} ${elapsed}"
    log "installed ${pkg} in ${elapsed}s: ${bins:-<no Installed/Replacing lines>}"
}

# install_assert_install_count [<accessor>]
# P5-T8's run-level tripwire: the install loop must have run EXACTLY ONCE per
# value the accessor WRITES TO ITS OUT_PATH. 0, or dies 60.
#
# THE ACCESSOR ARGUMENT IS PHASE 7'S ONE ADDITION, and it is what keeps ONE
# tripwire covering all three patterns. Patterns (b)/(c) install by DIRECTORY
# (`tsv_scope_crate_dirs`, the default); pattern (a) installs by PACKAGE NAME
# (`tsv_scope_packages`), because that is what `cargo install` takes (§9.2). Both
# sets have eight members today, so a COUNT-ONLY check would pass pattern (a)
# even if the loop had been driven off the wrong accessor and installed
# `trusty-git-analytics` instead of `tga` — which is precisely the discontinuity
# DOC-1 D3 warns about. The assertion is therefore on the SET, not the count:
# P7-T1's acceptance requires the installed package names to be EXACTLY
# `tsv_scope_packages` — "no more, no fewer, none repeated" — and asserted
# against the helper's output rather than a literal list, because the scope has
# already changed twice (§A.1, §A.1b) and a hardcoded list is the thing that
# silently fails to change with it.
#
# TWO DEPARTURES FROM P5-T8'S SNIPPET, BOTH NARROW, BOTH RECORDED.
#
#   1. IT IS A LIB FUNCTION, NOT INLINE SCENARIO CODE. P5-T8 writes the tripwire
#      as two lines inside the scenario calling `die 60` directly — but DOC-2
#      §12.4 states that "scenarios do NOT call `die` with a code of their own…
#      so a scenario stays a description of steps and expectations and never
#      encodes the exit-code table". The two are in direct conflict. Putting the
#      identical logic behind a lib function satisfies both: the scenario calls a
#      function, the function dies with its phase code, and the assertion is
#      unchanged.
#
#   2. IT COUNTS A LEDGER, NOT `$VMTEST_RUNDIR/run.log`. P5-T8 greps
#      `"$VMTEST_RUNDIR/run.log"` for `^vmtest: install_from_path `. THAT FILE
#      DOES NOT EXIST: the harness as merged through Phase 4 writes every
#      diagnostic to STDERR (DOC-2 §12.1) and keeps no run log, so the snippet as
#      written would grep a missing path. Rather than invent a whole run-log
#      facility to support one `grep -c`, `install_from_path` appends the crate
#      directory it is installing to `$VMTEST_RUNDIR/installs.log`, and this
#      counts that.
#
#      The ledger is STRICTLY STRONGER than the log grep it replaces, which is
#      the reason to prefer it rather than merely a way around a missing file:
#      it is written by `install_from_path` ITSELF, so it records an install
#      issued from ANYWHERE — a second install block, a retry, a future
#      `install-upgrade.sh` — whereas a scenario counting its own log lines can
#      only ever see the loop it wrote. The canonical `vmtest: install_from_path
#      <dir>` line is still emitted on stderr for the human and for the
#      checkpoint's clause (i).
#      THE ACCESSOR IS INVOKED THROUGH A STRING PARAMETER, and that is the one
#      place issue #16's static guard (`tests/check-no-swallowed-die.sh`) cannot
#      see: no grep for a function NAME finds `"$accessor"`. The three defects
#      it hid — a `$( … | wc -l)` and two `<( … | sort)` operands, all three
#      running `tsv_scope_*`'s `die 60` in a fork — were found by reading. They
#      are fixed here by passing the accessor an out_path, which is also what
#      makes the indirection safe by construction rather than by vigilance: the
#      call is now a plain command whose failure `set -e` catches.
install_assert_install_count() {
    local accessor="${1:-tsv_scope_crate_dirs}"
    local ledger unit expected actual dups missing extra scope

    ledger="$VMTEST_RUNDIR/installs.log"
    case "$accessor" in
        tsv_scope_packages)   unit='package name' ;;
        *)                    unit='crate directory' ;;
    esac

    [ -f "$ledger" ] \
        || die 60 "the install ledger ${ledger} does not exist — no install step ever ran, so the scenario installed nothing at all"

    # #16: out_path, so the accessor's `die 60` runs HERE and not in a fork.
    scope="$VMTEST_TMPDIR/install-scope-${accessor}.txt"
    # swallowed-die-check: indirect — `$accessor` is one of the two
    # `tsv_scope_*` out-path accessors (the `case` above enumerates them), and
    # this is a PLAIN command: no substitution, so a `die` inside it unwinds
    # this shell and `set -e` catches a non-zero return.
    "$accessor" "$scope"

    expected=$(wc -l < "$scope" | tr -d ' ')
    actual=$(grep -c . "$ledger" | tr -d ' ')
    [ "$actual" = "$expected" ] \
        || die 60 "install ran ${actual} times, expected ${expected} (one per ${unit}, from \`${accessor}\`). Thirteen in-scope ROWS resolve to ${expected} distinct values (§F-3); a count equal to the row count means the loop is iterating rows or binaries instead."

    dups=$(sort "$ledger" | uniq -d)
    if [ -n "$dups" ]; then
        die 60 "a ${unit} was installed twice: $(printf '%s' "$dups" | tr '\n' ' ')"
    fi

    # SET equality, not just count. Under pattern (a) both accessors emit eight
    # values, so a count-only check cannot tell `tga` from
    # `trusty-git-analytics` — the one discontinuity DOC-1 D3 names.
    # #16: both operands are now plain files. `sort` is not die-capable, so
    # these two process substitutions carry no classification to lose.
    missing=$(comm -23 <(sort "$scope") <(sort "$ledger") | tr '\n' ' ')
    extra=$(comm -13 <(sort "$scope") <(sort "$ledger") | tr '\n' ' ')
    if [ -n "${missing# }" ] || [ -n "${extra# }" ]; then
        die 60 "the installed set is not \`${accessor}\`'s set. NOT INSTALLED: ${missing:-<none>} / INSTALLED BUT NOT IN SCOPE: ${extra:-<none>}. Under pattern (a) the key is the PACKAGE name \`cargo install\` takes, not the directory: crates/trusty-git-analytics publishes as \`tga\` (DOC-2 §9.2, DOC-1 D3)."
    fi

    log "install count OK: ${actual} package-granular installs, one per ${unit} from \`${accessor}\` (${expected}), none installed twice, set matches exactly ($(sort "$ledger" | tr '\n' ' '))"
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
