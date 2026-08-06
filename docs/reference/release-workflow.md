# Release Workflow Reference

Each crate is tagged independently using the pattern `<crate-name>-v<version>`,
e.g. `trusty-mcp-core-v0.2.0`. The version comes from the crate's `Cargo.toml`.

> **`tga` tag aliases (issue #1128):** `trusty-git-analytics` publishes under the
> short package name `tga`. The binary-release workflow accepts **both**
> `tga-v<version>` and `trusty-git-analytics-v<version>` tags — they resolve to
> the same build config and Homebrew formula. The parse step canonicalizes the
> `tga` prefix to `trusty-git-analytics` for config/path lookups while keeping
> changelog `--tag-pattern` matched to the literal pushed tag. Either form works;
> you no longer need to push a second `trusty-git-analytics-v<version>` tag after
> a `tga-v<version>` push.

## Release Steps

1. Bump the crate version in `crates/<name>/Cargo.toml`.
2. Update any dependent crates that pin that version.

> **Convenience helper:** `scripts/bump-version.sh <crate-dir> <major|minor|patch>`
> automates step 1, the Cargo.lock sync, and the changelog staging. It reads
> the current package `version` from `crates/<crate-dir>/Cargo.toml`, computes
> the next semver, rewrites the package version line in place (dependency
> `version =` pins are left untouched), runs
> `cargo update -p <package-name> --precise <next-version>` so the root
> `Cargo.lock` entry for that package matches the bumped manifest (resolving
> `<package-name>` from the crate's `[package] name` field, which can differ
> from the crates/ directory name — see the `tga` note below; this closes
> issue #3199, where every release PR previously failed CI's `--locked` build
> until a human pushed a manual lock-sync follow-up commit), calls
> `scripts/assemble-changelog.sh <crate-dir> <next-version>` to fold the crate's
> per-PR `changelog.d/` fragments into a `## [<next-version>]` `CHANGELOG.md`
> section and delete the consumed fragments (issue #4476 — it pre-flights that
> assembly with `--check` BEFORE touching `Cargo.toml`, so a changelog problem
> never leaves the repo half-bumped), then **prints — but does not run —** the
> `git tag <prefix>-v<version>` and `git push origin <prefix>-v<version>`
> commands. This preserves the manual-tag convention (the human reviews and
> tags). The tag prefix defaults to the crate-dir name, with the one documented
> exception baked in: `trusty-git-analytics` (package name `tga`) releases under
> the `trusty-git-analytics-v*` tag series, NOT `tga-v*`.
>
> ```bash
> scripts/bump-version.sh trusty-search patch
> scripts/bump-version.sh trusty-git-analytics minor
> ```
>
> The `--suggest` auto-bump-from-commit-types enhancement noted in #1322/#1345
> is intentionally deferred and not yet implemented.
3. Run `cargo test -p <name>` and `cargo clippy --workspace -- -D warnings`.
4. Commit the version bump.
5. Create the tag: `git tag <crate-name>-v<version>`.
6. Push the tag: `git push origin <crate-name>-v<version>`.
7. Run `scripts/check-publish-ready.sh <crate-name>` (or
   `make publish-check CRATE=<crate-name>`) — must pass before publishing. See
   [Publish-Only-From-Merged-Main Guard](#publish-only-from-merged-main-guard-issue-2227) below.
   Also runs automatically post-merge: `make version-parity-check` (or wait
   for `.github/workflows/version-parity.yml` on the merge commit) — see
   [Version-Parity Guard](#version-parity-guard-issue-3366) below.
8. Publish: `cargo publish -p <crate-name>`.
   - **UI-embedding crates** (trusty-search, trusty-memory, trusty-analyze): prefix with `SKIP_UI_BUILD=1`:
     ```bash
     SKIP_UI_BUILD=1 cargo publish -p <crate-name>
     ```
     The committed `ui-dist/` bundle is already in the repo; without this flag, `build.rs` will attempt to invoke `pnpm` inside cargo's verification tarball, which fails because it tries to modify files outside `OUT_DIR`.

     **This is not optional for `trusty-search`/`trusty-console`** (verified via `cargo package --list`): their `Cargo.toml` `include` list ships only the pre-built `ui-dist/`/`ui/dist/` bundle, never `ui/src` — so `cargo publish`, with or without `SKIP_UI_BUILD`, can never rebuild the UI from source during packaging. Before running step 8 for any UI-embedding crate, confirm the committed bundle is actually current: `.github/workflows/release.yml`'s `ui-freshness-check` job does this automatically on every tag push by rebuilding fresh from `ui/src` and diffing against the committed bundle (issue #3568) — if it's red, regenerate (`cd crates/<crate>/ui && pnpm install --frozen-lockfile && pnpm run build`, then for `trusty-search` also copy `ui/dist/*` into `ui-dist/`, or run `make release-prep`) and commit before publishing. Do not rely on `cargo publish --dry-run` to catch this — see the `publish-dry-run` job's header comment for why it structurally cannot.
9. Build the release binary (if not already fresh): `cargo build --release -p <crate-name>`.
10. Install the binary locally with `cargo install --path crates/<dir> --locked`
   (for crates with binaries, e.g. trusty-search, trusty-mpm). This ensures the
   binary on PATH is always the version that was just released.

## Publish-Only-From-Merged-Main Guard (issue #2227)

🔴 **Publish only from merged main / the pushed release tag — never an
unmerged branch.** `scripts/check-publish-ready.sh <crate-name-or-dir>` (also
exposed as `make publish-check CRATE=<crate-name-or-dir>`) is a mandatory
preflight gate that must pass before running `cargo publish -p <crate-name>`.

It re-fetches the TRUE `origin/main` tip via `git ls-remote origin
refs/heads/main` (never trusting a possibly-stale local
`refs/remotes/origin/main`) and enforces two guards:

1. **GUARD 1 (merged-main)** — current HEAD must be `origin/main` itself or an
   ancestor of it (`git merge-base --is-ancestor`).
2. **GUARD 2 (version tag on main)** — the release tag `<crate>-v<version>`
   (or `tga-v<version>` for `trusty-git-analytics`) must be pushed to origin
   and its commit must be an ancestor of origin/main.

Either guard failing aborts with an actionable message and a non-zero exit
code; both passing prints an "OK — safe to publish" line and exits 0.

**Why**: issue #2209 — a publish ran from an unmerged feature branch that was
missing a P0 fix. That branch's build became the crates.io "latest" version,
and every concurrent worktree session that subsequently ran
`cargo install <crate>` silently picked up the regressed build until the
mistake was caught. Because this repo routinely runs multiple concurrent
Claude Code worktree sessions, "publish from whatever branch happens to be
checked out" is a correctness hazard, not just a process nicety — this script
turns "publish only from merged main" into a mechanical gate instead of a
convention someone can forget under pressure.

**Escape hatch (rare, deliberate use only)**: setting `ALLOW_UNMERGED_PUBLISH=1`
downgrades both guard failures to a loud warning and exits 0 instead of
failing. Use it only when you have a specific, understood reason to publish
from an unmerged commit; the default path is always "merge to main first."

**Independently enforced in CI (issue #3366)**: this same rule is now also a
mechanical gate on the tag-triggered binary-release pipeline itself, not just
something a human is instructed to check before `cargo publish`.
`.github/workflows/release.yml`'s `preflight` job fetches full history and
runs `git merge-base --is-ancestor` on the ACTUAL tagged commit (dereferenced
via `HEAD^{commit}`, never trusting `github.sha` or a naive `github.ref ==
'refs/heads/main'` string comparison — the latter can never match on a tag
push and would silently disable every release) against the true
`origin/main` tip. A tag pushed from an unmerged branch now fails this job
loudly and the whole pipeline — build, release, homebrew-bump, and the
publish-dry-run job below — is skipped rather than silently proceeding.

## Version-Parity Guard (issue #3366)

🔴 **Before bumping a crate's version, confirm it hasn't already drifted.**
`scripts/check-version-parity.sh` (also `make version-parity-check`) compares
every publishable crate's local `src/` tree against the crates.io tarball for
that crate's CURRENT (pre-bump) `Cargo.toml` version. If that version is
already live and the source no longer matches it, the crate is flagged as
drifted — source changes landed on `main` without a version bump, so the
version number on crates.io no longer describes what's actually in the repo.

This runs automatically on every push to `main`
(`.github/workflows/version-parity.yml`), so drift is caught right after it
merges instead of being discovered later as a `cargo publish --dry-run`
failure (which is exactly how issue #3366 was found: `trusty-common` and
`trusty-agents-common` had each drifted this way, and `cargo publish -p
trusty-mpm --dry-run` failed with "symbol not found" errors for symbols that
only existed in the unpublished, drifted source).

The check fails CLOSED: if a crate's live/local comparison can't be verified
(registry unreachable, unexpected response), that counts as a failure, never
a silent pass. See `crates/trusty-publish-guard` for the underlying engine.

For the cross-crate publish-ORDERING hazard (a related but distinct problem —
publishing a crate before a sibling it depends on is live), see
`scripts/publish-dry-run-order.sh` / `make publish-dry-run-order`, documented
in `.claude/skills/cargo-publish/SKILL.md` under "Cross-Crate Publish
Ordering." This ALSO now runs automatically and independently of human
memory: every `*-v*` tag push runs it via `.github/workflows/release.yml`'s
`publish-dry-run` job, scoped to just the tagged crate and its publishable
dependency closure (not the whole workspace, and not on every PR — see that
job's header comment for the cost/scope tradeoff). It is deliberately NOT a
dependency of the binary build/release jobs (a failing crates.io dry-run says
nothing about whether the GitHub binary tarball is safe to build), but its
own failure still turns the tag's workflow run red.

## macOS Code-Signing Critical Alert

🔴 **Never `cp target/release/<binary> ~/.cargo/bin/<binary>` on macOS.**

`cargo build` ad-hoc ("linker-signed") signs every release binary, and the
kernel's code-signing cache is keyed by the executable's `cdhash`. A plain
`cp` over an existing on-PATH binary can leave the kernel with a stale
cached identity, so the next exec is SIGKILL'd with
`EXC_CRASH / CODESIGNING — Taskgated Invalid Signature` **before any code
runs** — the process dies with `zsh: killed` and zero output, which looks
exactly like an OOM kill but is not. `cargo install` writes to a temp path
and renames atomically, which keeps the cache consistent. If you must copy
manually, follow it with `codesign --force --sign - ~/.cargo/bin/<binary>`
to regenerate the ad-hoc signature against the final file.

## Developer ID Signing for Persistent macOS TCC Grants (fixes #873, #2558)

### TCC Scope Summary — read this before re-granting anything

🟢 **Full Disk Access scope: `trusty-search` and external-volume daemons
only.** `trusty-mpm` / `tm` does **NOT** require FDA re-granting — the daemon
manages tmux sessions and git worktrees under `$HOME` only and never reads
TCC-protected or external (`/Volumes/…`) paths. `trusty-memory` and
`trusty-analyze` read `$HOME` locations only and also do NOT require FDA.

🟢 **App Data TCC scope: `trusty-mpm` IS in scope — a *different* category
than FDA above** (owner-authorized extension, #2558, 2026-07-14). `tm` reads
other apps' `$HOME` containers (Claude config dirs, tmux state), so macOS
re-prompts `'trusty-mpm' would like to access data from other apps` after
every ad-hoc-signed rebuild, for the same cdhash-identity reason FDA does.

🔴 **"No FDA" is not "no signing" — do not read the first bullet as an
exemption from the rest of this section.** Until 2026-08-06 that bullet was
the only mention of `trusty-memory` here, and its absence from
`SIGNABLE_BINARIES` was easy to mistake for a decision it never was. FDA is
one TCC category out of several. `trusty-memory` needs no FDA *and* needs a
stable Developer-ID identity, because `trusty-memory setup` and
`trusty-memory migrate` walk `$HOME` to depth 8 looking for
`.claude/settings*.json` (`trusty_common::claude_config::discover_claude_settings`)
and rewrite the ones they find. That walk skips `Library` and `Applications`
but not `~/Desktop`, `~/Documents`, or `~/Downloads` — the macOS
Files-and-Folders categories, keyed by cdhash exactly like FDA and App Data.
Its own store under `~/Library/Application Support/trusty-memory/` is NOT the
reason; an application's own data directory is not TCC-protected. See
[Signed Install: trusty-memory](#signed-install-trusty-memory) below.

**Do not cross the categories:** granting `trusty-search` Full Disk Access does
nothing for the `trusty-mpm` App-Data prompt, and `trusty-mpm` never needs —
and should never be granted — Full Disk Access. Re-signing with a stable
Developer-ID identity (below) fixes every category permanently, each for
the binary that actually needs it.

### The Problem

`cargo install` produces a new binary with a new **cdhash** every time it
runs (ad-hoc / linker-signed). macOS TCC keys grants by cdhash, so every TCC
category is revoked after every reinstall — Full Disk Access for
`trusty-search` (`#873`), and (owner-authorized scope extension, 2026-07-14,
#2558) App Data access for `trusty-mpm`: `tm` reads other apps' `$HOME`
containers (Claude config dirs, tmux state), so macOS re-prompts `'trusty-mpm'
would like to access data from other apps` on every ad-hoc-signed rebuild. The
workaround for either category is to re-grant/re-approve manually each time —
tedious and easy to forget.

### The Fix: Developer ID Application Signing

Signing with a Developer ID Application certificate + a fixed `--identifier`
per binary makes the **designated requirement (DR)** stable across rebuilds.
TCC matches the DR, not the cdhash, so the grant persists permanently — for
both the FDA (search) and App Data (mpm) categories.

### Single Source of Truth (#2558)

The identifier scheme, signing flags, and guidance text live in exactly one
place: `crates/trusty-installer/src/commands/macos_signing.rs`. Both
`tctl install` (automatic, fail-soft post-install hook) and the standalone
`tctl sign <target>` command (hard-failing, for local-source builds or
re-signing) call into it — there is no second, independently-maintained
implementation. `scripts/install-trusty-search-signed.sh` is a thin wrapper:
it still runs `cargo install` from local source (the one thing `tctl install`
cannot do), then shells out to `tctl sign trusty-search` for the actual
codesign step.

🔴 **Identifier migration notice (PR #2657 review):** #2558 canonicalized
`trusty-search`'s / `trusty-embedderd`'s `--identifier` from the short-form
`com.trusty.search` / `com.trusty.embedderd` (used by the pre-PR `tctl
install` hook) to the full-form `com.trusty.trusty-search` /
`com.trusty.trusty-embedderd` (matching the script and the launchd label
convention). Changing the identifier changes the designated requirement (DR),
which **invalidates any existing FDA/App-Data grant keyed to the old value** —
the same `indexes:2` symptom #873 originally described. To avoid a *silent*
re-break, every signing path (`tctl install`'s hook and `tctl sign`) now reads
the binary's CURRENT on-disk identifier before re-signing
(`macos_signing::current_identifier`, via `codesign -d`) and prints a loud,
distinct notice when it is about to change:

```
trusty-installer: ⚠ IDENTIFIER CHANGE for trusty-search: com.trusty.search -> com.trusty.trusty-search.
Existing macOS Full Disk Access / App Data grants keyed to the old identifier will STOP MATCHING
after this signing — re-grant/re-approve once after this install.
```

If you see this notice, re-grant FDA (search) or re-approve the next App-Data
prompt (mpm) exactly once — subsequent reinstalls will not re-trigger it,
since the identifier is now stable at the canonical value.

🟡 **Hardened Runtime split (PR #2657 review, PM decision):** `--options
runtime --timestamp` (Hardened Runtime) is gated per binary set and signing
context rather than always-on, to preserve BOTH pre-PR behaviors exactly:

| Set | `tctl install` auto-hook | `tctl sign` / wrapper script (explicit) |
|---|---|---|
| `trusty-search` / `trusty-embedderd` | **off** (pre-PR hook behavior) | **on** (pre-PR script behavior) |
| `trusty-mpm` | **on** | **on** |
| `trusty-agents` (`tagent`) | N/A — not a `tctl install` member (#4277) | **on** |
| `trusty-memory` + `trusty-bm25-daemon` + `trusty-memory-mcp-bridge` | **off** (same ONNX exposure as trusty-search) | **on** |

trusty-search/embedderd load an ONNX runtime dylib, and Hardened Runtime's
library-validation restriction has not yet been empirically verified safe for
that load path — flipping the automatic hook to hardened-by-default risked
silently breaking `tctl install trusty-search` on every machine with a
Developer ID cert already installed. trusty-memory takes the same conservative
side of the split: it links `ort`/`fastembed` through `trusty-common`'s
`memory-core` feature (`cargo tree -p trusty-memory -i fastembed`), so it
carries the identical unverified exposure. trusty-mpm and tagent load no
dylibs, so they are hardened unconditionally. **This split does not affect TCC grant stability** —
that depends on `--identifier` + Team ID, not `--options runtime` — only on
notarization eligibility and dylib-validation strictness. See the tracking
issue linked from PR #2657 for lifting this split once ONNX-under-Hardened-
Runtime is verified.

### One-Time Certificate Setup

1. **Enroll in the Apple Developer Program** (if not already enrolled):
   https://developer.apple.com/programs/enroll/

2. **Issue a Developer ID Application certificate** via one of:
   - Xcode → Settings → Accounts → select your Apple ID → Manage
     Certificates → "+" → "Developer ID Application"
   - https://developer.apple.com/account/resources/certificates/list →
     "+" → Developer ID → Developer ID Application → download the `.cer`

3. **Install the certificate** in your login keychain:
   - Double-click the downloaded `.cer` file — Keychain Access opens and
     imports it automatically.
   - Or drag the `.cer` into Keychain Access → login keychain.

4. **Verify the certificate is visible**:
   ```bash
   security find-identity -v -p codesigning | grep 'Developer ID Application'
   # Expected output (identity string varies by account):
   # 1) AABBCCDDEEFF "Developer ID Application: Your Name (TEAM1234)"
   ```

### Signed Install: trusty-search

Use `scripts/install-trusty-search-signed.sh` (or `make install-search-signed`)
instead of bare `cargo install`:

```bash
# From the repo root — installs from source and signs both binaries
scripts/install-trusty-search-signed.sh
# or
make install-search-signed
```

The script:
1. Runs `cargo install --path crates/trusty-search --locked` (installs both
   `trusty-search` and the bundled `trusty-embedderd` to `~/.cargo/bin/`).
2. Shells out to `tctl sign trusty-search --dir ~/.cargo/bin`, which:
   - Auto-detects the Developer ID identity from the login keychain (or honours
     `TRUSTY_SIGN_IDENTITY`).
   - Codesigns both binaries with:
     - `--identifier com.trusty.trusty-search` / `com.trusty.trusty-embedderd`
     - `--options runtime` (Hardened Runtime — required for notarization)
     - `--timestamp` (secure timestamp from Apple's servers)
   - Verifies signatures with `codesign --verify --deep --strict`.
3. Prints one-time FDA grant instructions and daemon restart commands.

Already have binaries on PATH and just want to (re-)sign them? Skip the cargo
install step entirely:

```bash
tctl sign trusty-search
```

### Signed Install: trusty-mpm (#2558, #2721)

On macOS the preferred local-source install path is
`scripts/install-trusty-mpm-signed.sh` (or `make install-mpm-signed`):

```bash
# From the repo root (or any worktree) — installs from source and signs both binaries
scripts/install-trusty-mpm-signed.sh
# or
make install-mpm-signed
# dry-run (prints every cargo/codesign command without executing):
scripts/install-trusty-mpm-signed.sh --dry-run
```

The script runs `cargo install --path crates/trusty-mpm --locked`, auto-detects
the Developer ID identity (or honours `TRUSTY_SIGN_IDENTITY`), then codesigns
**both** installed binaries with the canonical scheme (`--options runtime
--timestamp`, `--verify --deep --strict`): `~/.cargo/bin/trusty-mpm` with
`--identifier com.trusty.trusty-mpm` and `~/.cargo/bin/tm` with `--identifier
com.trusty.tm`.

🟡 **Script vs. `tctl sign trusty-mpm`?** (#2721) — as of the part-2 fix, `tctl`'s
`SIGNABLE_BINARIES` table covers the **full** trusty-mpm set: both
`trusty-mpm` (`com.trusty.trusty-mpm`) AND the `tm` binary (`com.trusty.tm`).
So `tctl install trusty-mpm` (automatic post-install hook) and
`tctl sign trusty-mpm` (explicit) now sign **both** binaries — closing the gap
where the primary `tm` binary stayed ad-hoc and kept re-triggering the App-Data
prompt on every reinstall. The dedicated script signs the same two binaries and
remains as a cert-guidance-friendly wrapper for local-source installs:

```bash
cargo install --path crates/trusty-mpm --locked
tctl sign trusty-mpm   # signs BOTH trusty-mpm (com.trusty.trusty-mpm) and tm (com.trusty.tm)
```

Both paths print the App-Data-TCC guidance: approve the next `'trusty-mpm'
would like to access data from other apps` prompt once; a stable Developer ID
identity makes that approval persist across every future reinstall.

**tm needs NO Full Disk Access re-grant** — this fixes the distinct *App-Data /
File-Provider* TCC category only; the FDA re-granting guidance above stays
scoped to trusty-search and other external-volume daemons.

### Signed Install: trusty-agents (#4277)

`trusty-agents` ships TWO artifacts that both re-prompt on every ad-hoc rebuild:
the `tagent` CLI/daemon binary and the `Trusty Agents.app` desktop shell
(`crates/trusty-agents/ui/src-tauri`). Missing either is the same #2721-class
gap — signing only one leaves the other ad-hoc and still re-prompting.

On macOS the preferred local-source install path is
`scripts/install-trusty-agents-signed.sh` (or `make install-agents-signed`):

```bash
# From the repo root (or any worktree) — installs tagent, signs it, and
# best-effort (re-)signs Trusty Agents.app if already built
scripts/install-trusty-agents-signed.sh
# or
make install-agents-signed
# dry-run (prints every cargo/tctl/codesign command without executing):
scripts/install-trusty-agents-signed.sh --dry-run
```

The script runs `cargo install --path crates/trusty-agents --locked`, then
signs `tagent` by shelling out to `tctl sign trusty-agents` (`SIGNABLE_BINARIES`
now carries `("tagent", AGENTS_SET, "com.trusty.tagent")` — the same
single-source-of-truth table used by `trusty-search`/`trusty-mpm`, extended for
#4277). If `Trusty Agents.app` is already built (under
`target/{release,debug}/bundle/macos/`), the script also (re-)signs it directly
with `codesign --deep --force --options runtime --timestamp --identifier
com.trusty.assistant`.

🟡 **`Trusty Agents.app` is signed differently from `trusty-mpm-gui`/
`trusty-code-gui` — deliberately.** Those two hardcode
`bundle.macOS.signingIdentity` directly in their `tauri.conf.json` (see
`docs/reference/common-pitfalls.md`), which makes `cargo tauri build`
**hard-fail** on any machine without that exact certificate — acceptable only
because no CI workflow ever runs `cargo tauri build` for them.
`crates/trusty-agents/ui/src-tauri/tauri.conf.json` deliberately carries **no**
`signingIdentity`, so a bare `pnpm tauri build` / `cargo tauri build` always
succeeds (falling back to Tauri's ad-hoc default). To get a Developer-ID-signed
bundle, export `APPLE_SIGNING_IDENTITY` — Tauri's own env-var override, which
"Overwrites `tauri.conf.json > bundle > macOS > signingIdentity`" per the
[Tauri v2 environment-variables reference](https://v2.tauri.app/reference/environment-variables/):

```bash
cd crates/trusty-agents/ui && pnpm install && \
  APPLE_SIGNING_IDENTITY="Developer ID Application: Bob Matsuoka (4JH68XUHC5)" \
  pnpm tauri build
# output: <repo root>/target/release/bundle/macos/Trusty Agents.app
```

Verified empirically (2026-07-29): the same build with no identity configured
anywhere bundles successfully ad-hoc (`codesign -dv` shows
`flags=0x20002(adhoc,linker-signed)`, `TeamIdentifier=not set`); with
`APPLE_SIGNING_IDENTITY` exported it produces `TeamIdentifier=4JH68XUHC5` and
Hardened Runtime (`flags=0x10000(runtime)`), and `codesign --verify --deep
--strict` passes on both the bundle and its single embedded Mach-O
(`Contents/MacOS/trusty-agents-ui` — `tagent` itself is resolved as a sibling
process via `$PATH`/well-known install dirs at runtime, see
`crates/trusty-agents/ui/src-tauri/src/sidecar.rs`, and is never embedded
inside the `.app`, so there is no separate sidecar binary to sign).

**No Developer ID certificate present is never a hard failure** for either
artifact — `tagent` stays ad-hoc signed with a clear warning, and
`Trusty Agents.app` falls back to Tauri's ad-hoc default; both remain fully
functional, they simply re-prompt for TCC access on the next rebuild.

Already have `tagent` on PATH and just want to (re-)sign it? Skip straight to:

```bash
tctl sign trusty-agents
```

### Signed Install: trusty-memory

Owner ruling 2026-08-06. `trusty-memory` was missing from `SIGNABLE_BINARIES`
for no recorded reason — no comment, doc, ADR, or commit message ever gave
one. The table grew one reported symptom at a time (#2558 search, #2721 `tm`,
#2951 the GUI, #4277 `tagent`) and nobody filed the trusty-memory case.

The TCC surface is the `$HOME` walk, not the palace store. `trusty-memory
setup` and `trusty-memory migrate` call
`trusty_common::claude_config::discover_claude_settings`, which recurses
`$HOME` to depth 8 for `.claude/settings*.json` and rewrites what it finds;
the skip-list covers `Library` and `Applications` but not `~/Desktop`,
`~/Documents`, or `~/Downloads`. Every `cargo install` mints a fresh cdhash
and revokes whatever was approved.

`cargo install --path crates/trusty-memory` produces THREE binaries and all
three are signed — #2721 is the recorded lesson that signing part of an
install set leaves the rest ad-hoc while the prompt keeps recurring:

| Binary | Identifier |
|---|---|
| `trusty-memory` | `com.trusty.trusty-memory` |
| `trusty-bm25-daemon` | `com.trusty.trusty-bm25-daemon` |
| `trusty-memory-mcp-bridge` | `com.trusty.trusty-memory-mcp-bridge` |

```bash
# From the repo root (or any worktree) — installs all three and signs them
scripts/install-trusty-memory-signed.sh
# or
make install-memory-signed
# dry-run (prints every cargo/tctl command without executing):
scripts/install-trusty-memory-signed.sh --dry-run
```

Already have the binaries on PATH and just want to (re-)sign them:

```bash
tctl sign trusty-memory
```

Like `install-trusty-agents-signed.sh`, the script delegates the signing step
to `tctl sign` rather than duplicating `codesign` flags, and a missing
Developer ID certificate is a warning, not a hard failure.

🟡 **Hardened Runtime is applied on explicit signing only** — the same split
trusty-search gets, and for the same reason. `trusty-memory` links
`ort`/`fastembed` through `trusty-common`'s `memory-core` feature
(`cargo tree -p trusty-memory -i fastembed`), and ONNX dylib loading under
Hardened Runtime's library validation has not been empirically verified. So
`tctl sign trusty-memory` and the script sign with `--options runtime
--timestamp`; the automatic `tctl install` hook would not. TCC grant
stability depends on `--identifier` + Team ID, never on `--options runtime`,
so the split costs nothing here.

🟡 **No automatic post-install hook.** `tctl install trusty-memory` does not
sign, matching `trusty-agents` (#4277) — `post_install_search` /
`post_install_mpm` are the only hooks `commands::install` calls. Run the
script or `tctl sign trusty-memory` after a local-source install.

### TRUSTY_SIGN_IDENTITY Override

If you have multiple Developer ID Application certificates (e.g. personal and
org), specify which one to use:

```bash
TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
  scripts/install-trusty-search-signed.sh
# or, for the standalone signing verb:
TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
  tctl sign trusty-mpm
```

### Install from a Specific crates.io Version

```bash
TRUSTY_INSTALL_VERSION=0.24.10 scripts/install-trusty-search-signed.sh
```

### Install from an Alternate Repo Path

```bash
TRUSTY_INSTALL_PATH=/path/to/trusty-tools scripts/install-trusty-search-signed.sh
```

### One-Time FDA Grant (after first signed install)

After the first signed install, grant Full Disk Access once:

1. Open **System Settings → Privacy & Security → Full Disk Access**
2. Click **"+"** and navigate to `~/.cargo/bin/trusty-search`
3. Enable the toggle for `trusty-search`
4. Restart the daemon:
   ```bash
   launchctl bootout  gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
   launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
   ```

After this one-time grant, future `scripts/install-trusty-search-signed.sh`
reinstalls keep the FDA grant — no re-granting needed.

🔴 **Check for an orphan listener BEFORE `bootout`** (issue #4230). `bootout` is
a silent no-op against a process launchd does not own, and the following
`bootstrap` then crash-loops every ~10 s failing to bind the already-held port
while the orphan keeps serving — so the restart reads as successful and ships
nothing. Run this first:

```bash
launchctl print gui/$(id -u)/<label> | grep -E 'state|pid'   # does launchd own a live PID?
lsof -nP -iTCP:<port> -sTCP:LISTEN                           # who actually holds the port?
```

If the listener's PID is not the one `launchctl print` reports, `kill -TERM` it
before restarting, then use `launchctl kickstart -k gui/$(id -u)/<label>`.

🔴 **A 200 `GET /health` right after `bootstrap` is NOT sufficient evidence the
restart succeeded** (issue #2486) — a client racing the restart window (e.g. an
MCP stdio bridge's auto-reconnect) can spawn an orphan daemon that answers
`/health` 200 while missing the plist's `EnvironmentVariables` and running with
the wrong `cwd`. Verify launchd actually owns the new process with one of:

```bash
launchctl print gui/$(id -u)/<label>                        # launchd's own view
curl -s http://127.0.0.1:<port>/health | jq '.supervised'   # trusty-mpm only; must print true
tm doctor                                                   # trusty-mpm only; `daemon_orphan` must be OK
```

⚠️ **`PPID == 1` is NOT evidence of launchd supervision** (issue #4230). Any
daemon whose spawning parent has exited reparents to PID 1, so the #4230 orphan
reported `PPID 1` while launchd's `com.trusty.mpm` was `not running`. Compare the
listening PID against `launchctl print`'s PID instead — that is the only check
that distinguishes the two.

The same caveat applies to `.supervised` when it reads **true**: that flag's
fallback prong is the identical PPID test, so a reparented orphan can report
`supervised: true`. It is conclusive only when it reads `false`. `tm doctor`'s
`daemon_orphan` check therefore treats it as a fallback and prefers the PID
comparison above.

When that comparison cannot be made, `daemon_orphan` reports `Unknown` — never a
pass, and never a failure that would have you kill the listener. There are two
such states, and neither is evidence of an orphan:

- the responding daemon predates the `pid` field on `/health`, so there is nothing
  to compare; or
- `launchctl` could not be asked — missing from `PATH`, sandboxed, or the unit is
  not visible in the caller's bootstrap domain. `launchctl list <label>` resolves
  against the caller's domain, and a `LimitLoadToSessionType = Aqua` agent is not
  visible from every context, where the lookup exits 113 with the same message it
  gives for a unit that was never loaded.

A hard `Fail` requires a POSITIVE answer from launchd: either a different PID than
the one serving, or launchd confirming the job is down while something still
answers the port.

### Notarization Appendix (Optional — for distributing to OTHER machines)

Notarization is **not required** for local FDA persistence. It is only needed
if you distribute the `trusty-search` binary to other machines that do not
have it in their FDA list already (e.g. sharing a build with a team member).

Notarize a signed binary using `xcrun notarytool`:

```bash
# Option A: Apple ID + app-specific password
xcrun notarytool submit ~/.cargo/bin/trusty-search \
  --apple-id "you@example.com" \
  --team-id "TEAM1234" \
  --password "xxxx-xxxx-xxxx-xxxx" \
  --wait

# Option B: App Store Connect API key (preferred for automation)
xcrun notarytool submit ~/.cargo/bin/trusty-search \
  --key /path/to/AuthKey_KEYID.p8 \
  --key-id "KEYID" \
  --issuer "issuer-uuid" \
  --wait
```

After notarization succeeds, staple the ticket (for offline Gatekeeper checks):

```bash
xcrun stapler staple ~/.cargo/bin/trusty-search
```

> Note: binaries (as opposed to `.app` bundles or `.pkg` installers) cannot
> have a notarization ticket stapled — the ticket must be retrieved online by
> Gatekeeper when the binary first runs on the target machine. For local use
> (your own machine), notarization adds no benefit; Developer ID signing alone
> is sufficient for FDA persistence.

## Connection-Safe Daemon Restart (issue #534)

As of trusty-common 0.10.0, all four HTTP daemons (trusty-memory, trusty-search,
trusty-analyze, trusty-mpm) implement graceful shutdown: they drain in-flight
requests before exiting when they receive SIGTERM. The `serve --stdio` proxy
reconnects automatically with exponential backoff when the daemon restarts (the
`trusty-memory-mcp-bridge` binary is a deprecated shim that forwards to
`serve --stdio`; update your `.mcp.json` to `"command": "trusty-memory", "args":
["serve", "--stdio"]`).

**Use `launchctl bootout` (SIGTERM), not `launchctl kickstart -k` (SIGKILL):**

```bash
# Graceful stop → install → restart
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
cargo install --path crates/<dir> --locked
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/<label>.plist
# Re-grant Full Disk Access only for trusty-search (and other external-volume
# daemons) — see the TCC Scope Summary above. trusty-mpm never needs FDA.
```

For verifying the restart actually took (a 200 `GET /health` is not sufficient
evidence, `PPID == 1` is not evidence of supervision, and the orphan-listener
check to run before `bootout`), see the FDA-grant restart playbook above under
[One-Time FDA Grant](#one-time-fda-grant-after-first-signed-install) — the
same verification steps apply regardless of which daemon you restarted.

## Version Management

Every crate manages its own version independently in its own `Cargo.toml`.
The `[workspace.package]` table no longer carries a `version` field (see #343).
When publishing, bump only the crates that actually changed — do not cascade
version bumps to siblings with no functional changes.

### Docker end-to-end validation (release gate)

The Docker E2E harness (`.github/workflows/e2e-docker.yml`, driver `scripts/e2e-docker.sh`, docs `docker/e2e/README.md`) validates that published crates install cleanly from crates.io on a clean Linux image and pass minimal end-to-end scenarios.

**Release gate policy:**
- **On-demand** via `workflow_dispatch` (manual trigger): primary workflow; run anytime from the Actions UI.
- **As a release gate**: when a release batch spans **≥3 crates OR ≥3 PRs**, run the E2E validation manually before declaring the release complete. Smaller releases (1–2 crates, 1–2 PRs) may skip it and rely on standard CI.
- **Nightly schedule**: runs automatically at 02:00 UTC as a regression safety-net, catching published-artifact and crates.io breakage independent of releases.

The workflow is **not** triggered on every PR or every release; it is a deliberate gate. When needed, trigger it from **Actions → e2e-docker → Run workflow** or:
```bash
gh workflow run e2e-docker.yml
```

## Bundled Crates — Intentionally Skipped by `release.yml`

`release.yml` only builds standalone distributable binaries. Three crates are
**bundled** under the Single-Install Convention (epic #943) and have no
standalone binary target of their own — when their tags fire, the workflow
skips the build/release/homebrew-bump jobs and exits success:

| Bundled crate | Ships inside |
|---|---|
| `trusty-console` | `trusty-search`, `trusty-memory`, `trusty-analyze`, `trusty-review`, `trusty-mpm` |
| `trusty-bm25-daemon` | `trusty-memory` |
| `trusty-embedderd` | `trusty-search` |

To add a new bundled crate in the future, add it to the `elif` block in the
"Resolve per-crate config" step of `.github/workflows/release.yml`.
