# Publishing to crates.io

> **Publishing is fully automated.** Every push to `main` triggers the
> continuous-delivery pipeline (`.github/workflows/cd.yml`) which:
> 1. Runs the full smoke-test suite (fmt + clippy + tests + release build).
> 2. Bumps the workspace patch version (`X.Y.Z` → `X.Y.Z+1`).
> 3. Commits the bump as `chore: bump version to X.Y.Z [skip ci]`, tags `vX.Y.Z`, and pushes.
> 4. Publishes all five crates to crates.io in dependency order:
>    `tga-core` → (`tga-collect` ‖ `tga-classify` ‖ `tga-report`) → `tga-cli`.
>
> You should not need to run any of the manual steps below in normal operation.
> They are documented for first-time bootstrapping and emergency recovery only.

## How the CD Pipeline Works

The pipeline is defined in `.github/workflows/cd.yml` and consists of three
sequential job groups:

### 1. `gate` — Smoke Test Gate

Runs on every push to `main` *except* the bot's own version-bump commits. The
guard is two-fold:

- The bot's commit message ends with `[skip ci]`, which GitHub Actions honors.
- The `if:` condition on the job additionally rejects pushes whose actor is
  `github-actions[bot]`.

Steps:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace -- -D warnings`
- `cargo test --workspace -- --skip collect_duetto_frontend` (skips live integration tests)
- `cargo build --release --bin tga`

### 2. `bump` — Patch-Bump and Tag

Reads the current version from the root `Cargo.toml`, increments the patch
component, updates the manifest, commits as
`chore: bump version to X.Y.Z [skip ci]`, creates tag `vX.Y.Z`, and pushes both
the commit and the tag to `origin/main`.

### 3. `publish-core` → `publish-mid` → `publish-cli`

Each publish job checks out the freshly-created tag and runs
`cargo publish -p <crate>`. The mid-tier jobs run in parallel via a matrix on
`{tga-collect, tga-classify, tga-report}`. A 30-second sleep between stages
gives the crates.io sparse index time to propagate.

## Version Constraints in Path Dependencies

The dependent crates (`tga-collect`, `tga-classify`, `tga-report`, `tga-cli`)
specify both a path and a version on every internal dependency:

```toml
tga-core = { path = "../tga-core", version = "0.1" }
```

The `version = "0.1"` constraint is minor-compatible (`>=0.1.0, <0.2.0`), so
patch bumps from the CD pipeline never require manifest edits. When the workspace
crosses a minor boundary (e.g. `0.1.x` → `0.2.0`) these constraints must be
bumped by hand before the next release.

## Required Repository Secrets

| Secret | Purpose |
|--------|---------|
| `CARGO_REGISTRY_TOKEN` | Authenticates `cargo publish` to crates.io. |
| `GITHUB_TOKEN` | Provided automatically; used to push the bump commit and tag. |

Generate a crates.io token at https://crates.io/settings/tokens with the
`publish-new` and `publish-update` scopes. Add it under
**Settings → Secrets and variables → Actions** as `CARGO_REGISTRY_TOKEN`.

## First-Time Bootstrap

The very first publish of each crate must be done manually, because the CD
pipeline assumes the crate already exists on crates.io. Run from a clean,
up-to-date `main`:

```bash
cargo login                          # only once per developer machine
cargo publish -p tga-core
sleep 30                             # let the index settle
cargo publish -p tga-collect
cargo publish -p tga-classify
cargo publish -p tga-report
sleep 30
cargo publish -p tga-cli
```

After this bootstrap, every subsequent push to `main` will auto-publish.

## Manual Publish (Emergency Fallback)

If the CD workflow is broken or you need to publish out-of-band, perform the
steps below by hand. The same dependency order applies.

### Pre-flight Checklist

1. All tests pass on the workspace:
   ```bash
   cargo test --workspace
   ```
2. Clippy is clean:
   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   ```
3. Formatting is clean:
   ```bash
   cargo fmt --check
   ```
4. The version in the root `Cargo.toml` is the version you intend to publish,
   and there is no existing tag with that number.

### Publish Order

```
1. tga-core           (no internal deps)
        │
2a. tga-collect       (depends on tga-core)
2b. tga-classify      (depends on tga-core)    ← these three can publish in parallel
2c. tga-report        (depends on tga-core)
        │
3. tga-cli            (depends on all four)
```

### Commands

```bash
# Bump the workspace version manually
# Edit Cargo.toml: [workspace.package] version = "X.Y.Z"
git commit -am "chore: bump version to X.Y.Z"
git tag "vX.Y.Z"
git push origin main --follow-tags

# Publish in order
cargo publish --dry-run -p tga-core
cargo publish           -p tga-core
sleep 30                                       # allow index propagation

cargo publish --dry-run -p tga-collect && cargo publish -p tga-collect
cargo publish --dry-run -p tga-classify && cargo publish -p tga-classify
cargo publish --dry-run -p tga-report && cargo publish -p tga-report
sleep 30

cargo publish --dry-run -p tga-cli && cargo publish -p tga-cli
```

## Verifying a Successful Publish

```bash
# Search crates.io index (may take a minute to propagate)
cargo search tga-core

# Or open the page directly
open https://crates.io/crates/tga-core
```

Verify the version number and README content on the crates.io page match
expectations.

## Loop Prevention

The CD workflow could in principle trigger itself when it pushes the bump
commit. Two independent guards prevent this:

1. **Commit-message marker**: the bump commit ends with `[skip ci]`, which
   GitHub Actions treats as a signal to skip workflow runs.
2. **Actor check**: the `gate` job's `if:` rejects pushes whose actor is
   `github-actions[bot]`.

Both guards must be present. Removing either one risks a publish loop.
