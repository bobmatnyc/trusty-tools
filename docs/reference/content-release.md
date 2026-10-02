# Content releases (`content-vX.Y.Z`)

Instructional content — agents, skills, PM instruction sections, output
styles — ships on its own release channel, separate from every crate
([ADR-0064](../adr/0064-instructional-content-tracked-separately-from-code.md),
epic [#8378](https://github.com/bobmatnyc/trusty-tools/issues/8378)). A
content release is a GitHub Release tagged `content-vX.Y.Z` that carries one
tarball and its sha256. It never publishes to crates.io and never builds a
crate.

## Cutting a release

Releases are cut by `local-ops` with supervisor sign-off, by dispatching
`.github/workflows/content-release.yml` against `main`:

```bash
# 1. Dry run: package, run the selftest, upload a workflow artifact. No release.
gh workflow run content-release.yml --ref main -f version=0.1.0
# 2. Read the run summary (manifest + sha256), then cut for real.
gh workflow run content-release.yml --ref main -f version=0.1.0 -f dry_run=false
```

The workflow refuses to run when:

- the version is not SemVer `X.Y.Z[-pre]` (no leading `v`), checked against
  the whole string, so a value holding a newline or carriage return fails;
- the tag already exists, because a published content release is immutable;
- a real cut (`dry_run=false`) was dispatched against any ref but `main`;
- the packager selftest fails.

The `release` job is the only one with `contents: write`. It re-verifies the
sha256, then checks the tag once more: only a 404 from
`gh api repos/<repo>/git/ref/tags/<tag>` lets it continue, and any other
status stops the job. It then runs `gh release create`, which creates the tag
at the packaged commit. The release is created with `--latest=false`, so it
never replaces a crate release as the repository's "Latest". A pre-release
version (`X.Y.Z-pre`) is published with `--prerelease`.

🔴 **Never push a `content-v*` tag by hand.** `release.yml`,
`pre-publish.yml` and `semver-checks.yml` exclude `content-v*` from their
`*-v*` tag triggers (Refs #8389), so a hand-pushed tag starts nothing. It
still uses up the version: this workflow refuses a tag that already exists,
so no release can be cut for it. Pick the next version.

## Installing a release (`tm content`)

`tm` pins one release in `~/.trusty-mpm/content/content-lock.toml` (tag and
sha256) and keeps the bundle beside it as `<tag>.tar.gz`. Nothing is compiled
in (#8974), so a host with no cache and no network must install a bundle by
hand.

| Command | What it does |
|---|---|
| `tm content update` | Installs the newest published `content-v*` release and re-pins to it. "Published" means the highest `content-v*` tag (found through one unpaged `git/matching-refs` request) whose `releases/tags/<tag>` release is not a pre-release; a pre-release is passed over, and a draft or a tag with no release (GitHub answers 404 for both) is an error. Sends `GITHUB_TOKEN`, else `GH_TOKEN`, when non-empty (60 requests/hour otherwise). A failed or empty listing is an error, and the previous pin stays in force. |
| `tm content update --content-ref content-vX.Y.Z` | Pins exactly that release. |
| `tm content install --from content-vX.Y.Z.tar.gz` | Offline. The `.sha256` sidecar must sit beside the bundle. |
| `tm content status` | Prints the source (`dev`, `bundle` or `none`), the pinned tag and sha256, and the binary version. With nothing installed it prints an `info:` line and exits 0 while `tm` still compiles its content in; otherwise it exits non-zero when nothing serves. |

The pin changes only when one of these commands runs. The `.sha256` sidecar
is required by both `update` and `install`. It proves the transfer only: it
comes from the same release as the bundle, so it catches a corrupt or
truncated download, not a release replaced together with its sidecar. Trust is
on first use: the pinned sha256 is what every later read checks, and a
re-fetch of the currently pinned tag that returns other bytes is refused. Only
the current pin is checked.

Every write fails closed. A bundle is pinned only after it matches its
sidecar and passes the checks `content::resolve` applies at run time. It is
stored before `content-lock.toml` names it, and every write holds an exclusive
lock on `.update.lock`. A sha256 mismatch, a missing sidecar, a tag missing
upstream, a re-fetch of the pinned tag that returns other bytes, a newer or missing
`schema_major` or an unreachable host leaves the previous pin in force.
`tm doctor`'s `content` row reports the same facts as `tm content status`.
With nothing installed it reports INFO while `tm` still compiles its content
in, and WARN once ADR-0064 PHASE_1 removes it.

## What the bundle holds

`scripts/package_content.sh --version X.Y.Z` writes two files:

| File | Contents |
|---|---|
| `content-vX.Y.Z.tar.gz` | `bundle-manifest.toml` first, then one directory per content class |
| `content-vX.Y.Z.tar.gz.sha256` | `<hex>  content-vX.Y.Z.tar.gz`, checkable with `sha256sum -c` |

Content classes and where they are read from today:

| Class | Source directory |
|---|---|
| `agents` | `crates/trusty-agents-common/src/assets/agents/` |
| `skills` | `crates/trusty-mpm/src/assets/skills/` |
| `instructions` | `crates/trusty-mpm/src/assets/instructions/` |
| `output-styles` | `crates/trusty-mpm/src/assets/output-styles/` |
| `sm_instructions` | `crates/trusty-mpm/src/assets/sm_instructions/` |
| `harness_understanding` | `crates/trusty-agents-common/src/assets/harness_understanding/` |

Superseded 2026-09-30 (owner ruling, #8974, ADR-0064 amendment): the
`output-styles` row above describes the seed packager only. The target IA has
no `content/output-styles/`; output styles become a separate file inside
`content/instructions/`.

PHASE_1 (PR-D, [#8387](https://github.com/bobmatnyc/trusty-tools/issues/8387))
moves these to `content/<class>/`. The packager then changes one line,
`DEFAULT_SOURCE_ROOT="content"`, and the archive layout stays the same, so a
consumer of the bundle sees no difference.

`bundle-manifest.toml`:

```toml
# Generated by scripts/package_content.sh (ADR-0064). Do not edit.
bundle_version = "0.1.0"
tag = "content-v0.1.0"
schema_major = 1
file_count = 220

[[class]]
name = "agents"
source = "crates/trusty-agents-common/src/assets/agents"
files = 43
# ... one [[class]] table per class, in the order above
```

`schema_major` starts at 1. It changes only when the archive layout or the
manifest keys change in a way an older reader cannot handle.

## Determinism

The resolver pins a bundle by sha256, so the same tree must give the same
bytes on every run. Every archive entry has mtime 0, uid/gid 0, empty owner
names, and mode 0644 (files) or 0755 (directories). Entries are sorted by
path. The gzip header has mtime 0 and no file name. Dot-files are skipped.

The packager refuses to write a bundle, and exits 1, when a class directory
is missing, holds no files, or holds a symlink or other non-regular file.
It also deletes a bundle left in the output directory by an earlier run, so
a failed run leaves nothing a later step could upload.

`scripts/package_content_selftest.sh` proves all of the above. It runs in
`ci.yml`'s `changes` job on every PR and again before packaging in the
release workflow.
