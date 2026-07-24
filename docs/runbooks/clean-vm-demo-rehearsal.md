# Clean-VM Demo Rehearsal Runbook — trusty-mpm 1.0.0 + trusty-installer 0.4.7

**Duration:** ~10 minutes  
**Target:** Fresh macOS Apple Silicon VM with NO Claude Code, NO tmux, NO ~/.trusty-tools  
**Objective:** Dry-run the full install → workflow pipeline before the live demo.

---

## Pre-Flight Checklist

Before starting, verify the VM has:

- [ ] macOS Apple Silicon (M1/M2/M3 family)
- [ ] No Claude Code installed (check: `which cc` → should be empty)
- [ ] No tmux installed (check: `which tmux` → should be empty)
- [ ] No ~/.trusty-tools directory (check: `ls ~/.trusty-tools` → should fail)
- [ ] Xcode Command Line Tools (check: `xcode-select --print-path` → should return a path)
- [ ] GitHub CLI installed and authenticated (check: `gh auth status` → should show logged-in user)
- [ ] Network connectivity to GitHub + crates.io
- [ ] At least 2 GB free disk space

---

## Step 1: Run the Single-URL Install

**Command:**
```bash
curl https://install.sh | sh -y
```

**Expected observations:**
- Shell echoes the install script as it runs.
- Terminal renders a **live, in-place per-component progress checklist** (indicatif UI).
- Progress checklist shows components (e.g., "Downloading trusty-mpm", "Installing Claude Code", "Installing tmux", "Bootstrapping daemon") with real-time status updates.
- Checklist clears/overwrites itself in-place as each component completes (not a scrolling log).
- **Critical:** Claude Code + tmux appear in the checklist as **auto-installed-on-missing** (this path is never hit on Bob's live machine, only here on fresh VMs).
- Exit code: 0 (success).

---

## Step 2: Verify Install Output

After the checklist completes:

**Command:**
```bash
which cc
```

**Expected:** Returns `/opt/homebrew/bin/cc` (or similar homebrew path).

**Command:**
```bash
which tmux
```

**Expected:** Returns `/opt/homebrew/bin/tmux` (or similar homebrew path).

**Command:**
```bash
which tm
```

**Expected:** Returns a path (e.g., `/opt/homebrew/bin/tm` or similar).

**Command:**
```bash
tm version
```

**Expected:** Outputs `tm 1.0.0`.

---

## Step 3: Start the Daemon Explicitly

**CRITICAL NOTE:** `tm sessions new` does NOT auto-start the daemon. You MUST call `tm start` first.

**Command:**
```bash
tm start
```

**Expected:**
- Daemon bootstrap message(s).
- Exit code: 0.

**Verify daemon is running:**
```bash
tm doctor
```

**Expected:** All checks green (daemon alive, connectivity OK, config validated).

---

## Step 4: Set Auth Token (Avoid #2246 Keychain Loop)

**Flag:** The known hazard #2246 causes an interactive login (via `claude login`) to loop indefinitely in the keychain. Use `setup-token` instead.

**Command:**
```bash
claude setup-token
```

**Follow prompts:**
- Paste your Claude API token (from https://claude.ai/account/settings/api-keys).
- Token is stored in the keychain (safe, persists across shell restarts).

**Alternative (NOT recommended, hits #2246):**
```bash
claude login
```
(Skip this unless you want to observe the keychain loop behavior; if it hangs, ^C and fall back to setup-token.)

---

## Step 5: Verify GitHub + tm Health

**Command:**
```bash
gh auth status
```

**Expected:** Shows logged-in GitHub user and authentication method.

**Command:**
```bash
tm doctor
```

**Expected:**
- Daemon: ✓ Alive
- Config: ✓ Valid
- GitHub: ✓ Authenticated
- Claude: ✓ Authenticated (or "not required for CLI")
- All other checks: ✓ Green

---

## Step 6: Create a New Session with Demo Task (Clone + Configure + Tmux in One Command)

**Choose a throwaway test repo** (create or use an existing non-critical repo; e.g., a fork of this project or a dummy repo).

**Command:**
```bash
tm sessions new https://github.com/<your-github-user>/<throwaway-repo>.git --task "Add a demo README section explaining the new feature"
```

**Expected:**
- `tm` clones the repo into `.trusty-mpm-projects/<your-github-user>/<throwaway-repo>/`.
- `.trusty-tools` skeleton and `.trusty-mpm/` directories are created inside the cloned repo.
- A new tmux session is created (check: `tmux list-sessions` → should show a session named after your repo).
- Session is NOT attached yet; your current shell remains in the original prompt.
- Git worktree for the session is checked out and ready.

---

## Step 7: Attach to the Session and Verify Tmux

**Command:**
```bash
tm sessions attach
```

**Expected:**
- Terminal switches to the tmux session (you see a new tmux status bar at the bottom).
- Session shows panes for the repo and workspace.

**Command (inside tmux, optional):**
```bash
pwd
```

**Expected:** Shows the worktree path (e.g., `.trusty-mpm-projects/<user>/<repo>/.base/.worktrees/<uuid>`).

---

## Step 8: Pause and Resume the Session (Workflow Beat)

**Command (inside tmux):**
```bash
tm sessions stop
```

**Expected:**
- tmux session is detached.
- Terminal returns to your base shell.
- Session is paused (snapshot written to `.trusty-mpm/sessions/`).

**Command:**
```bash
tm sessions list
```

**Expected:** Shows your paused session (status: `paused` or `inactive`).

**Command:**
```bash
tm sessions resume
```

**Expected:**
- Loads the session snapshot.
- Prints resume context (branch, last commit, uncommitted changes count, if any).
- Terminal remains at the base shell (session is not auto-attached).

**Command:**
```bash
tm sessions attach
```

**Expected:** Re-attaches to the tmux session. Session state (working directory, git branch, any uncommitted changes) is preserved from before the pause.

---

## Step 9: Make a Demo Commit and Review Workflow

Inside the tmux session (from Step 8):

**Command:**
```bash
git checkout -b demo-feature-readme
```

**Command (edit a file and stage it):**
```bash
echo "## Demo Feature" >> README.md
git add README.md
```

**Command (commit with trusty-mpm footer):**
```bash
git commit -m "docs: add demo feature section to README

🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools"
```

**Expected:**
- Commit is created on the demo-feature-readme branch.
- Commit message includes the trusty-mpm footer (NOT Claude Code footer).

**Command (open a PR — the command depends on your repo and CI setup; for a GitHub-backed repo):**
```bash
gh pr create --title "Demo: Add feature section to README" --body "This is a demo PR from the clean-VM rehearsal."
```

**Expected:**
- PR is created on GitHub.
- `trusty-review` is auto-wired (check the PR page for any auto-checks or hooks).
- Exit code: 0.

---

## Step 10: Verify trusty-review Integration

**Command:**
```bash
gh pr view <PR-NUMBER> --json checks,reviews
```

(Replace `<PR-NUMBER>` with the number from Step 9.)

**Expected:**
- Checks include trusty-review (if the repo is wired to trusty-review service).
- Checks are running or pending (or green if instant).

**Note:** Full CI/review cycles depend on the repo's workflow. The goal here is to confirm the PR infrastructure is in place, not to wait for all checks to pass.

---

## Known Hazards & Watch-Fors

### #2246 — Keychain Loop on Interactive Login
- **Trigger:** Running `claude login` interactively on a fresh machine.
- **Symptom:** Prompt hangs indefinitely in a keychain password dialog loop.
- **Workaround:** Use `claude setup-token` instead (Step 4). It stores the API token directly without spawning keychain dialogs.
- **Why:** The harness's credential-chain initialization interacts poorly with launchd on first-run systems. Root cause is under investigation.

### Daemon NOT Auto-Started by `sessions new`
- **Trigger:** Running `tm sessions new` before `tm start`.
- **Symptom:** Command waits or errors with "daemon not alive" or similar.
- **Workaround:** Always run `tm start` first (Step 3). Explicit daemon bootstrap is required after a fresh install.
- **Why:** Sessions new reads configuration from the daemon; it does not spawn it. This design keeps session creation lightweight and ensures daemon state is consistent.

### Worktree Auto-Cleanup on Pause
- **Note:** When you pause a session (`tm sessions stop`), the worktree is automatically cleaned up after a grace period if the session is not resumed. On a fresh install, this is unlikely to matter, but it's good to know if you pause and walk away for hours.

### GitHub CLI Drift on Multi-Account Setups
- **Note:** If you use multiple GitHub accounts (e.g., bob-duetto + personal), `gh` may drift to an unexpected account on subsequent commands. Verify `gh auth status` after opening a PR if you're unsure which account was used.

---

## Success Criteria Checklist

Map each success criterion back to the four unsafe-on-live-machine objectives:

### 1. Real Piped Install + Launchd Bootstrap
- [ ] `curl … | sh -y` completes with exit 0.
- [ ] Live per-component progress checklist appears and updates in-place (not scrolling).
- [ ] `tm version` returns 1.0.0 after install.
- [ ] `tm doctor` shows daemon alive (launchd bootstrap successful).

### 2. CC + Tmux Auto-Install-on-Missing
- [ ] `which cc` returns a path (Claude Code was installed by the script).
- [ ] `which tmux` returns a path (tmux was installed by the script).
- [ ] Both tools were listed in the progress checklist as "Installing" and completed.

### 3. Full tm Workflow: Sessions + Tmux + Pause + Resume + PR
- [ ] `tm sessions new <repo>` creates a tmux session and clones the repo.
- [ ] `tm sessions stop` pauses the session and writes a snapshot.
- [ ] `tm sessions resume` restores the session state (branch, uncommitted changes visible).
- [ ] `tm sessions attach` re-enters the tmux session.
- [ ] `git commit` with trusty-mpm footer and `gh pr create` both succeed.

### 4. trusty-review Auto-Wired
- [ ] PR is created on GitHub (exit 0 from `gh pr create`).
- [ ] `gh pr view <PR>` shows trusty-review in the checks (or repo config indicates auto-wiring is enabled).

---

## Post-Demo Cleanup

When done:

**Command (to leave the session):**
```bash
exit  # inside tmux
```

**Command (optional, to destroy the session):**
```bash
tm sessions stop --force
```

**Command (to return to the base system state):**
```bash
rm -rf ~/.trusty-tools  # if desired, for a truly clean slate
```

---

## Troubleshooting Quick Reference

| Symptom | Likely Cause | Fix |
|---------|--------------|-----|
| `curl … \| sh` hangs after initial download | Homebrew is slow on first install | Wait 2-3 min; if it persists, ^C and retry |
| Progress checklist doesn't appear | Old version of trusty-installer served | Check URL resolves to 0.4.7 (run `curl -I https://install.sh`) |
| `tm start` fails with "port in use" | Another daemon is already running | Run `tm stop` first, or use a different port (config option) |
| `claude setup-token` fails | Network issue or expired token | Verify https://claude.ai is reachable; generate a new token |
| `gh pr create` fails with "not a git repo" | Wrong working directory | Confirm you're inside the cloned repo's worktree |
| `tm sessions attach` shows empty pane | Tmux server crashed | Run `tmux kill-server` and `tm sessions resume` again |

---

## Notes for Observers

- This runbook is designed to be run **live and sequentially** on a single fresh VM. All 10 steps should complete in ~10 minutes if there are no network delays.
- The progress checklist in Step 1 is the most visually distinctive feature; watch for the in-place redrawing effect to confirm trusty-installer 0.4.7 is live.
- The pause/resume beat in Step 8 is new in this release; it's the key workflow innovation being showcased.
- If CI/review integration is new to the demo repo, `gh pr view` in Step 10 may show "no checks yet" initially; that's normal (checks are async).

---

**Ready to rehearse? Start with Step 1.**
