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
> automates step 1 and the changelog staging. It reads the current package
> `version` from `crates/<crate-dir>/Cargo.toml`, computes the next semver,
> rewrites the package version line in place (dependency `version =` pins are
> left untouched), calls `scripts/generate-changelog.sh <crate-dir> <tag-prefix>`
> to prepend the unreleased `CHANGELOG.md` section, then **prints — but does not
> run —** the `git tag <prefix>-v<version>` and `git push origin <prefix>-v<version>`
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
7. Publish: `cargo publish -p <crate-name>`.
   - **UI-embedding crates** (trusty-search, trusty-memory, trusty-analyze): prefix with `SKIP_UI_BUILD=1`:
     ```bash
     SKIP_UI_BUILD=1 cargo publish -p <crate-name>
     ```
     The committed `ui-dist/` bundle is already in the repo; without this flag, `build.rs` will attempt to invoke `pnpm` inside cargo's verification tarball, which fails because it tries to modify files outside `OUT_DIR`.
8. Build the release binary (if not already fresh): `cargo build --release -p <crate-name>`.
9. Install the binary locally with `cargo install --path crates/<dir> --locked`
   (for crates with binaries, e.g. trusty-search, trusty-mpm). This ensures the
   binary on PATH is always the version that was just released.

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

## Developer ID Signing for Persistent macOS FDA (fixes #873)

### The Problem

`cargo install` produces a new binary with a new **cdhash** every time it
runs (ad-hoc / linker-signed). macOS TCC keys the Full Disk Access grant by
cdhash, so FDA is revoked after every reinstall. The workaround is to
re-grant FDA manually each time — tedious and easy to forget.

### The Fix: Developer ID Application Signing

Signing with a Developer ID Application certificate + a fixed `--identifier`
per binary makes the **designated requirement (DR)** stable across rebuilds.
TCC matches the DR, not the cdhash, so the FDA grant persists permanently.

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

### Signed Install Script

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
2. Auto-detects the Developer ID identity from the login keychain.
3. Codesigns both binaries with:
   - `--identifier com.trusty.trusty-search` / `com.trusty.trusty-embedderd`
   - `--options runtime` (Hardened Runtime — required for notarization)
   - `--timestamp` (secure timestamp from Apple's servers)
4. Verifies signatures with `codesign --verify --strict`.
5. Prints one-time FDA grant instructions and daemon restart commands.

### TRUSTY_SIGN_IDENTITY Override

If you have multiple Developer ID Application certificates (e.g. personal and
org), specify which one to use:

```bash
TRUSTY_SIGN_IDENTITY="Developer ID Application: Acme Corp (ABCDE12345)" \
  scripts/install-trusty-search-signed.sh
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
