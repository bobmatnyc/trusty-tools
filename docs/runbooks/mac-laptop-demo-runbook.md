# Mac Laptop Demo Runbook — Presenter Script for Brand-New MacBook

**Duration:** T-minus 10 min prep (night before) + ~8 min live demo  
**Target Audience:** Live demo watchers  
**Setup:** Brand-new Apple Silicon MacBook (Homebrew + Claude Code already present; tmux to be pre-seeded)  
**Objective:** Deliver a flawless single-URL install → remote repo provisioning → live Claude Code PM session demo on the new machine, showcasing trusty-mpm 1.0.1 + trusty-installer 0.4.8.

---

## Critical Pre-Demo Facts

- **New MacBook:** Already has Homebrew and Claude Code installed; needs tmux pre-seeded (issue #3821); no tm, no ~/.trusty-tools.
- **Installer version:** Requires **trusty-installer 0.4.8+** (0.4.7 has known issues: progress-line spam, trusty-memory plist not bootstrapped, verify-races-startup false "down"/exit 2). Install script resolves latest automatically; if 0.4.7 is served, use the memory-bootstrap fallback (see Known Failure Modes).
- **Pre-seeding requirement:** tmux must be installed via Homebrew before `curl | sh`. If missing, `curl | sh` exits 2; the #3821 fix will auto-install tmux in a future release. Claude Code + Homebrew are already on the MacBook, so no pre-seed needed for them.
- **Authentication:** Claude Code PM authentication happens INSIDE the session after `tm sessions attach` — Bob auths interactively in Claude Code on stage (~30 sec, natural moment). NO CLI auth beats.
- **Real install command:** `curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y` (serves installer 0.4.8 → installs tm 1.0.1).
- **Real TTY demo:** The live install runs on the new MacBook with a real terminal and graphics, so the per-component progress checklist has its best chance to render in-place (not scrolling).

---

## T-Minus Prep (Night Before or Morning Of — ON THE NEW MACBOOK)

### Step 1: Verify Clean trusty-tools State

The new MacBook already has Homebrew + Claude Code. Confirm trusty-tools is not yet present:

```bash
which tm        # Should be EMPTY
which tctl      # Should be EMPTY
ls ~/.trusty-tools 2>&1  # Should fail: "No such file or directory"
```

**Expected:** All three commands confirm no prior tm installation.

**Note:** Homebrew and Claude Code are already installed; you don't need to touch them.

---

### Step 2: Pre-Seed tmux (The #3821 Workaround)

Install tmux via Homebrew so the installer doesn't fail when it can't find it:

```bash
brew install tmux
```

**Expected:** tmux installs via Homebrew (takes ~30 sec).

**Verify tmux is installed:**

```bash
which tmux && tmux -V
```

**Expected:** Returns a path (e.g., `/opt/homebrew/bin/tmux`) and version (e.g., "tmux 3.x").

**Bootstrap the tmux server** (required once, even though we won't use tmux until later):

```bash
tmux new-session -d
```

**Expected:** A tmux server is created in the background (no visible output). This prevents socket errors later when `tm sessions new` tries to create a session.

---

### Step 3: Prepare the Demo Repo URL

Have a throwaway GitHub repo URL ready (NOT on screen during demo):

1. **Throwaway demo repo URL:** Create or use a non-critical GitHub repo for the live demo.
   - Example: `https://github.com/bobmatnyc/trusty-demo-test.git`
   - **Check the default branch:** If it's NOT `main` (e.g., `master`), you'll add `--git-ref master` to the `tm sessions new` command in Beat 3.
   - Ensure you have push access to this repo.

---

### Step 4: Terminal Setup & Cosmetics

On the new MacBook, open a full-screen terminal for the demo:

```bash
# Open Terminal or iTerm2
# Set font size to 16-18pt (visible from ~6ft away)
# Clear any existing prompt history
clear
```

**Test the font size:** Open this script in a browser and check readability from arm's length. Adjust if needed.

**Note:** This is your only terminal on the new MacBook. All demo beats run in this single window.

---

### Step 5: Sanity Checks (Optional But Recommended)

Before the live demo, run a quick sanity check to verify everything is ready:

```bash
# Network connectivity
ping -c1 github.com && echo "GitHub: OK"
ping -c1 crates.io && echo "Crates.io: OK"

# Homebrew ready
brew --version

# Xcode CLT ready
git --version

# tmux ready
tmux -V
```

**Expected:** All commands succeed. Network reachable. All tools report versions.

---

## Live Demo Beats

**All beats run directly on the new MacBook. No VM, no screen switches.**

### Core Beats (4 beats, ~8 minutes)

These four beats showcase the core story: single-URL install → daemon health → provision remote repo with one command → Claude Code PM opens inside the provisioned environment.

---

### Beat 1: The Single-URL Install (WOW Moment)

**Talk track:** "We're going to install trusty-mpm from a single URL on a completely fresh Mac. Watch for the per-component progress checklist."

**Command (in the MacBook terminal):**

```bash
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y
```

**Expected (the moment the audience will watch):**
- Script echoes as it runs.
- **Live per-component progress checklist appears** — a vertical list of components (e.g., "Downloading trusty-mpm", "Claude Code: already satisfied" or "already installed", "Installing tmux" or "tmux: already satisfied", "Installing tm", "Bootstrapping daemon", "Configuring shell") with live status updates.
- Each component shows a spinner → checkmark as it completes. Components already present (Claude Code, tmux, Homebrew) show checkmarks immediately.
- Checklist **clears/overwrites itself in-place** (indicatif UI effect, since we have a real TTY).
- **TCC/approval prompts may appear** (macOS security for launchd services and codesign verification). If they do, click "Allow" or "Install" as prompted — this is a natural, expected moment. The script continues after you approve.
- Exit code: 0 (success). Prompt returns.

**Talk track:** "Each component installs in order. You'll see the checklist update live. Claude Code and tmux are already on this machine, so they'll show as satisfied. The installer is smart enough to skip what's already there and focus on trusty-mpm, tm, and the daemon. TCC prompts (if any) are macOS asking for permission to install services — that's normal."

**Fallback (if install exits non-zero):**
- Run the same `curl | sh -y` command again (the installer is idempotent).
- If it fails twice, check the error output and report it back. Common causes: network glitch, missing Homebrew (should not happen), or disk full (unlikely).

**Fallback (if checklist degrades to scrolling log instead of in-place redraw):**
- Still fine. The install is working; the rendering is just a TTY detection edge case.
- **Talk track:** "The checklist is rendering as a log here; in a real terminal, it would redraw in place. Either way, each component is progressing."

---

### Beat 2: Verify Versions & Binaries

**Command (in the MacBook terminal):**

```bash
which claude && echo "Claude Code: OK"
which tm && echo "tm: OK"
tm version
```

**Expected:**
- Paths printed (e.g., `/opt/homebrew/bin/claude`, `/opt/homebrew/bin/tm`).
- `tm version` outputs `tm 1.0.1`.

**Talk track:** "Claude Code and tm are both installed. Let's verify the daemon is alive."

---

### Beat 3: Start Daemon & Health Check

**Command (in the MacBook terminal):**

```bash
tm start
```

**Expected:**
- Daemon bootstrap message (may be silent or show "Starting daemon").
- Prompt returns with exit code 0.

**Command (in the MacBook terminal):**

```bash
tm doctor
```

**Expected:**
- All checks green:
  - Daemon: ✓ Alive (v1.0.1 matches binary)
  - Config: ✓ Valid
  - GitHub: ✓ Initialized (or similar)
  - Claude: ✓ Initialized (or similar)

**Talk track:** "Daemon is running. System is green. Now for the centerpiece."

---

### Beat 4: Provision Remote Repo & Launch Claude Code (CENTERPIECE)

**THE CORE STORY:** One command provisions everything — clones the repo, creates a tmux session, scaffolds .claude and .trusty-mpm config directories, and launches Claude Code inside the newly provisioned environment.

**Talk track:** "This is where trusty-mpm does its magic. One command does what normally takes multiple steps: clone the repo, bootstrap the environment, set up tmux, and launch the PM session."

**Command (in the MacBook terminal):**

```bash
# Replace <throwaway-repo> with your demo repo URL
tm sessions new https://github.com/bobmatnyc/trusty-demo-test.git --task "Explore the provisioned environment and show how PM works"
```

**Note:** If your throwaway repo's default branch is NOT `main` (e.g., `master`), add `--git-ref master`:

```bash
tm sessions new https://github.com/bobmatnyc/trusty-demo-test.git --git-ref master --task "Explore the provisioned environment and show how PM works"
```

**Expected:**
- Repo is cloned into `~/.trusty-mpm-projects/bobmatnyc/trusty-demo-test/`.
- `.trusty-mpm/` and `.claude/` directories are created inside the cloned repo with PM config.
- A new tmux session is created and named after the repo (e.g., `tm-trusty-demo-test-01`).
- Output shows the session name and attaching instructions.

**Talk track:** "Cloning… scaffolding… session created. Now let's attach and watch Claude Code open inside this provisioned environment."

**Fallback (if sessions list is empty despite the command succeeding):**
- Use the session NAME printed by the command (e.g., `tm-trusty-demo-test-01`) instead of listing.
- This is a known UI glitch; names always work.

**Then (same terminal):**

```bash
tm sessions attach tm-trusty-demo-test-01
```

**Expected:**
- Terminal switches to tmux (you see a tmux status bar at the bottom).
- Panes show the worktree directory.
- Claude Code loads in the PM context (may take 2-3 sec).

**Inside tmux (once Claude Code is visible on screen):**

**Talk track:** "Claude Code is now open inside the provisioned repo. Watch as Bob authenticates inside Claude Code for the first time — this is a one-time step."

**Claude Code authentication (on screen, ~30 sec):**
- Claude Code prompts for authentication (API key, or browser login).
- Bob authenticates interactively using Claude Code's built-in flow.
- Once authenticated, Claude Code is ready to work.

**This is a natural, expected moment.** The audience watches Bob auth live inside Claude Code in the new environment. No CLI token-pasting; it's all UI-driven and self-evident.

**Talk track after auth completes:** "Claude Code is now authenticated and ready to work inside the provisioned repo. The PM session is fully loaded. This is the trusty-mpm workflow: provision from a URL, launch the PM, everything is in context."

---

## Optional Closer Beats (If Time Allows)

If the live demo is running ahead of schedule, you can showcase additional features. These beats demonstrate durability and the full workflow end-to-end. Each adds ~2-3 minutes.

---

### Optional Beat 1: Pause & Resume the Session (Durability)

**Inside tmux (in the MacBook terminal):**

```bash
tm sessions stop
```

**Expected:**
- tmux is detached.
- You're back at the MacBook shell prompt (outside tmux).
- Message shows session paused/stopped.

**Command (back at the MacBook shell):**

```bash
tm sessions list
```

**Expected:** Session status shows `paused` or `inactive`.

**Command (in the MacBook terminal):**

```bash
tm sessions resume
```

**Expected:**
- Session snapshot is loaded.
- Context summary is printed (branch, last commit, any changes).
- MacBook shell prompt returns (session is not auto-attached yet).

**Command (in the MacBook terminal):**

```bash
tm sessions attach tm-trusty-demo-test-01
```

**Expected:** Tmux session is re-attached. All context preserved (branch, working directory, uncommitted changes if any).

**Talk track:** "Pause and resume are durable. Session state survives across detach. This is critical for CI handoffs and team workflows where one person pauses work for another to pick up."

---

### Optional Beat 2: Make a Demo Commit & Show PR Workflow

**Inside tmux (in the MacBook terminal):**

```bash
# Create a feature branch
git checkout -b demo-feature-section

# Edit the README
echo "" >> README.md
echo "## Demo Feature" >> README.md
echo "This section demonstrates the new feature added via trusty-mpm." >> README.md
git add README.md

# Commit with the trusty-mpm footer
git commit -m "docs: add demo feature section to README

🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools"
```

**Expected:**
- Commit is created on the `demo-feature-section` branch.
- Commit message includes the **🤖🤖🤖 trusty-mpm footer** (NOT Claude Code or Co-Authored-By).

**Talk track:** "Commit is complete. Notice the footer — it's the trusty-mpm attribution, auto-applied by the framework. This footer ties the work to trusty-mpm in the git history."

**Then (still inside tmux):**

```bash
# Push the branch
git push -u origin demo-feature-section

# Create a PR
gh pr create --title "Demo: Add feature section to README" --body "Demo PR from the live runbook. Shows trusty-review integration."
```

**Expected:**
- Branch is pushed.
- PR is created on GitHub.
- Output shows the PR URL (e.g., `https://github.com/bobmatnyc/trusty-demo-test/pull/1`).
- Exit code: 0.

**Talk track:** "PR is live. In a real workflow, trusty-review would automatically check this PR for code quality, security, and readability."

**Optional (if you want to show the PR on screen):**

```bash
# Inside tmux:
gh pr view 1 --web
```

**Expected:** PR page opens in the default browser (if available), showing the commit with the 🤖🤖🤖 footer.

---

## Known Failure Modes & Fallbacks

### Installer 0.4.8 Issues (This Runbook Requires 0.4.8+)

The `curl | sh` command resolves the latest installer release automatically. If for some reason installer 0.4.7 is served (previous release), use the fallback workaround below. **0.4.7 known issues:**

- Progress-line spam in the checklist (harmless, but verbose)
- trusty-memory plist not bootstrapped (daemon starts but trusty-memory service may not; fallback to manual bootstrap if needed)
- Verify-races-startup false "down"/exit 2 (rarely triggered; re-run the installer if it happens)

**Fallback (if running 0.4.7 and trusty-memory doesn't start):**

```bash
# Inside the session, after install completes:
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.memory.plist && launchctl kickstart -k gui/$(id -u)/com.trusty.memory
```

**Expected:** trusty-memory service starts. No errors.

### Other Common Failures

| Issue | Symptom | Fallback |
|-------|---------|----------|
| **Install exits nonzero** | `curl \| sh -y` fails with error | Idempotent retry: run the same `curl \| sh -y` command again. Installer is idempotent. |
| **TCC prompt blocks** | GUI dialog asks to approve service install | Normal on first install. Click "Allow" or "Install" in the dialog and wait. Install continues automatically. |
| **Checklist degrades to scrolling log** | Progress doesn't redraw in-place | Normal in some TTY environments. Rendering is fine; install is working. Talk track: "The checklist is scrolling here; in other terminals, it redraws in place." |
| **Sessions list shows empty** | `tm sessions list` returns nothing, but session was created | Use the session NAME from the `tm sessions new` output instead of listing (e.g., `tm sessions attach tm-trusty-demo-test-01`). This is a known UI glitch. |
| **Claude Code auth hangs** | Claude Code authentication prompt doesn't complete | This is rare but possible on first launch. Click "Cancel" and try again, or restart Claude Code inside the session (`tm sessions detach`, re-`tm sessions attach`). |
| **Tmux socket error in Beat 4** | `tm sessions new` fails with socket error | Should NOT happen because we bootstrapped tmux in prep. But if it does: exit and run `tmux new-session -d`, then retry `tm sessions new`. |
| **`tm sessions attach` shows empty pane** | Tmux is black/empty inside | Tmux server may have crashed. Recover: `tm sessions detach`, run `tmux kill-server`, then `tm sessions resume` and `tm sessions attach` again. |

### CLI-Only Hazard (#2246)

The #2246 keychain loop issue applies ONLY to CLI auth flows (e.g., `claude login` at the shell). **This runbook does NOT use CLI auth.** Bob authenticates inside Claude Code on stage (UI-driven), which is safe. If you ever need CLI login outside a session, use `claude setup-token` (one-time API token paste) instead of `claude login` (interactive).

---

## Reset Procedure (Between Dry-Runs or After the Demo)

To get the new MacBook back to pre-demo state (clean, ready for another run):

```bash
# Delete the demo repo and session artifacts
rm -rf ~/.trusty-mpm-projects/bobmatnyc/trusty-demo-test
rm -rf ~/.trusty-mpm/sessions/tm-trusty-demo-test-01*

# Stop the trusty-mpm daemon (and related services)
launchctl unload ~/Library/LaunchAgents/com.trusty.mpm.plist 2>/dev/null || true
launchctl unload ~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist 2>/dev/null || true

# Uninstall trusty-mpm binaries (check ~/.local/bin and ~/.cargo/bin depending on install path)
rm -f ~/.local/bin/tm ~/.local/bin/tctl
rm -f ~/.cargo/bin/tm ~/.cargo/bin/tctl

# Remove the daemon data directory
rm -rf ~/.trusty-tools

# Kill any remaining tmux servers (optional, if you want a fully clean slate)
tmux kill-server 2>/dev/null || true
```

**Note:** This removes the demo artifacts and daemon, but KEEPS:
- Xcode Command Line Tools
- Homebrew (and all installed packages like tmux)
- GitHub CLI authentication
- Claude API token (stored in keychain)

These are safe to keep for the next run; they're part of the MacBook setup.

**To truly reset everything (nuclear option, rarely needed):**

```bash
# Everything above, plus:
rm -rf ~/.local/bin/claude  # Remove Claude Code binary
rm -rf ~/.cargo/bin/claude
rm -f ~/.trusty-tools/*  # Remove all trusty-tools config
```

**Verify the reset worked:**

```bash
which tm       # Should be EMPTY
which claude   # Should be EMPTY
ls ~/.trusty-tools 2>&1  # Should fail: "No such file"
```

---

## Dry-Run Before the Live Demo (Highly Recommended)

Before the live demo, run through all steps once on the new MacBook:

1. Complete T-Minus Prep (Xcode CLT, Homebrew, tmux, credentials).
2. Run through all 10 live demo beats (Beat 1 through Beat 10).
3. Time the entire run (should take ~12 minutes if network is good).
4. Verify the terminal layout and font size look good on screen.
5. Note any timing surprises (e.g., "Homebrew installs slowly, add 30 sec to the talk track").
6. Run the Reset Procedure to get back to pre-demo state.
7. You're ready for the live demo.

---

## Cheat Sheet (One-Page Glance Reference)

### T-Minus Prep (On the New MacBook)

```bash
# Verify clean slate
which claude; which tm; which tmux; ls ~/.trusty-tools

# Install Xcode CLT (click the GUI dialog)
git --version

# Install Homebrew
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
eval "$(/opt/homebrew/bin/brew shellenv)"
brew --version

# Install tmux
brew install tmux
which tmux && tmux -V

# Bootstrap tmux server
tmux new-session -d

# Open terminal, set font 16-18pt, clear
```

### Core Live Demo Beats (4 beats, ~8 min)

```bash
# Beat 1: Install
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Beat 2: Verify install
which claude; which tm
tm version

# Beat 3: Start daemon
tm start
tm doctor

# Beat 4: Provision repo & launch Claude Code (THE CENTERPIECE)
tm sessions new https://github.com/bobmatnyc/trusty-demo-test.git --task "Explore the provisioned environment"
# OR with custom git-ref:
tm sessions new https://github.com/bobmatnyc/trusty-demo-test.git --git-ref master --task "Explore the provisioned environment"

tm sessions attach tm-trusty-demo-test-01
# Watch Claude Code open; Bob authenticates inside Claude Code on stage (~30 sec)
```

### Optional Closer Beats (If Time Allows)

```bash
# Optional Beat 1: Pause & resume (durability)
tm sessions stop
tm sessions list
tm sessions resume
tm sessions attach tm-trusty-demo-test-01

# Optional Beat 2: Commit + PR (workflow end-to-end)
git checkout -b demo-feature-section
echo "## Demo Feature" >> README.md
git add README.md
git commit -m "docs: add demo feature section

🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools"

git push -u origin demo-feature-section
gh pr create --title "Demo: Add feature section" --body "Demo PR from live runbook"
gh pr view 1 --web  # optional
```

### Reset

```bash
rm -rf ~/.trusty-mpm-projects/bobmatnyc/trusty-demo-test
rm -rf ~/.trusty-mpm/sessions/tm-trusty-demo-test-01*
launchctl unload ~/Library/LaunchAgents/com.trusty.mpm.plist 2>/dev/null || true
launchctl unload ~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist 2>/dev/null || true
rm -f ~/.local/bin/tm ~/.local/bin/tctl ~/.cargo/bin/tm ~/.cargo/bin/tctl
rm -rf ~/.trusty-tools
```

---

## Notes for Bob (Stage Craft)

- **The centerpiece is Beat 4.** The story is: "One command provisions everything—clone, worktree, config, session, Claude Code." Emphasize this moment. The audience should walk away remembering that single `tm sessions new <url>` command as the magic move.

- **Speak through the install (Beat 1).** Narrate what's happening as the checklist progresses: "Downloading trusty-mpm… Claude Code is already here so that's satisfied… tmux is already here too… now installing tm… bootstrapping the daemon…" The audience won't hear the mechanics; you're providing the story. This also primes them for the "already satisfied" components.

- **Claude Code auth on stage is a feature, not a hazard.** When Claude Code opens in Beat 4 and prompts for auth, lean into it: "Here's Claude Code opening inside our provisioned repo. Bob is authenticating inside Claude Code for the first time—this is a natural one-time step." It's UI-driven and self-evident, no CLI token-pasting required.

- **TCC prompts are normal.** If a security dialog appears during the install, stay calm and click through it. Tell the audience: "macOS is asking for permission to install services. This is normal." Then continue. Don't treat it as a failure.

- **Keep energy up if things degrade.** If the checklist rendering looks like a scrolling log instead of in-place updates, don't treat it as a failure. Say: "The progress is scrolling here due to the environment; in a normal terminal, it would redraw in place. Either way, the install is working." Confidence matters.

- **Use session NAMES, not IDs.** If `tm sessions list` appears empty, don't panic. Use the name from the `tm sessions new` output (printed to screen). Names always work; it's just a UI glitch.

- **Idempotent retry is your friend.** If the install fails, just run `curl | sh -y` again. It's designed to be safe on re-run.

- **Optional beats are bonus.** If you finish Beat 4 early and the demo timing allows, the optional beats (durability + PR workflow) add richness. But the core story is complete after Beat 4: provision from nothing, launch Claude Code, everything is in context.

---

**Ready? Start with T-Minus Prep on the new MacBook the night before. Core demo is ~8 min. Good luck!**
