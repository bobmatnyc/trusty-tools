# D61 / D7d revised: patch cuts instead of `-r` re-cuts (plan, df-G01)

Plan only, 2026-11-03. All calls were reads (`gh api` GET, `git show origin/main` at `bac374040d`, crates.io GET). Nothing was changed on GitHub, the tap or crates.io.
Replaces parts of `2026-11-03-d61-d7d-release-recreate-plan.md` (the earlier plan; sections 1 and 3.4 and the D61 rewrite mechanics still stand).

Rulings applied (Bob, 13:59Z):
- D05: "Instead of -r names, cut a new patch version per crate with rebuilt binaries."
- D06: "Cut content-v2.3.1 from rewritten main and ship the skip-tags-without-release fix, then delete the 6 old tags."
- D08: "Yes: add refs/tags/*-r* to ruleset 26091265 after the re-cut."

Rulings applied (Bob, 2026-11-05 00:47-00:49Z, relayed by the Architect):
- D50 (Q1): "Mixed: Route A for code, installer, memory, search; Route B for analyze 0.12.8, console 0.12.7, mpm 1.11.2, review 0.38.6."
- D51 (Q9): "Yes, publish each new patch."
- D52: "GO now" for `drafts-cleanup-61.sh` (61 `-r` draft releases).
- D77 (df-D93): "Cut content-v2.3.4; read D06's 2.3.1 as the next content patch from rewritten main." Every `content-v2.3.1` / `2.3.1` below now reads `content-v2.3.4` / `2.3.4`, including the `--require-release` argument in section 10 item 5. The D06 quote above stays verbatim.

Probe P facts (`.trusty-mpm/sessions/df-records/d7d-probe-20261103/evidence.txt`, `probe-step7-retry2.log`): an old tag cannot be moved after its immutable release is deleted (GH013); it can be deleted, but its name cannot be pushed again (GH013, creations restricted); a draft publishes onto a new commit only if the PATCH sends `tag_name` and `target_commitish` together. Fallback X: step D deletes the 67 releases and the 67 tags.

## 0. Summary

1. Eight binary crates plus the `content` channel are affected. Old tags and releases of the 67 vanish for good; each binary crate gets one new `-v` release. Table in section 1.
2. A "cut" in this repo = bump PR, merge, `<crate>-v<new>` tag push, `release.yml` builds and attaches binaries and updates the Homebrew tap itself, then a manual `cargo publish`. `release.yml` does not publish to crates.io (`release.yml:1573-1580`). Each cut burns one permanent crates.io version.
3. Tension to settle (Q1): `main` is already a minor ahead for 4 crates (analyze 0.13.0, console 0.13.0, mpm 1.12.0, review 0.39.0), with 6 to 31 unreleased changelog fragments each. A true patch (0.12.8, 0.12.7, 1.11.2, 0.38.6) can only be cut from a `release/<crate>-<semver>` branch off the rewritten old-release commit (allowed by D96, `scripts/check-tag-ancestry.sh`), not from `main`. The other 4 crates are one patch ahead on `main` already (code 0.7.1, installer 0.17.1, memory 0.36.5, search 0.62.2).
4. Outage window (section 2.4): from step D to each new release, roughly 2 to 3 hours for the small crates and 3 to 4 hours for trusty-mpm, if everything is staged. Hard floor about 1.5 hours. During it `install.sh`, `tctl` and Homebrew serve nothing or old versions.
5. D08: the Architect reads it as moot under D05, tm-dogfood says it is not moot; both positions in section 6, undecided.
6. Irreversible steps: D (delete releases and tags), then each crates.io publish and each release publish (new names burn on use). Section 7.

## 1. Crates affected

"Highest released" = highest GitHub release. crates.io state read 2026-11-03 (`/api/v1/crates/<c>`, none yanked). "Main" = `Cargo.toml` `[package] version` on `origin/main`. "Next free" = highest crates.io version plus one patch; no candidate name exists among the 931 tags in `ls-remote-before-step5.txt`.

| Crate | In the 67 | Highest released | Highest tag | crates.io max | Main | Next free patch | Remaining highest release after D |
|---|---|---|---|---|---|---|---|
| trusty-analyze | 1 | 0.12.7 | 0.12.7 | 0.12.7 | 0.13.0 | 0.12.8 | 0.12.6 |
| trusty-code | 1 | 0.7.0 | 0.7.0 | 0.7.0 | 0.7.1 | 0.7.1 (= main) | 0.5.1 |
| trusty-console | 6 | 0.12.6 | 0.12.6 | 0.12.6 | 0.13.0 | 0.12.7 | 0.12.0 |
| trusty-installer | 5 | 0.17.0 | 0.17.0 | 0.17.0 | 0.17.1 | 0.17.1 (= main) | 0.13.8 |
| trusty-memory | 15 | 0.36.1 | 0.36.4 | 0.36.4 | 0.36.5 | 0.36.5 (= main) | 0.28.2 |
| trusty-mpm | 10 | 1.11.1 | 1.11.1 | 1.11.1 | 1.12.0 | 1.11.2 | 1.7.9 |
| trusty-review | 7 | 0.38.5 | 0.38.5 | 0.38.5 | 0.39.0 | 0.38.6 | 0.36.1 |
| trusty-search | 16 | 0.62.1 | 0.62.1 | 0.62.1 | 0.62.2 | 0.62.2 (= main) | 0.54.5 |
| content (channel) | 6 | 2.3.0 | 2.3.0 | n/a | manifest set at cut | 2.3.1 (D06) | 0.1.0 |

Notes:
- trusty-memory: crates.io has 0.36.2 and 0.36.4 and tags exist for both, but no GitHub release after 0.36.1. Those two tags are not in the 67, so they survive the rewrite.
- No binary crate among the 8 pins another of the 8 (grep of `crates/*/Cargo.toml`: no hit), so bumps are independent.
- Total of 67 = 8 binary crates (61) + 6 content.

Reading of D05 ("old versions deleted and not replaced"): confirmed as the only reading that fits the probe. Of the 67, 53 releases are older versions of these crates (for example trusty-search 0.54.6 to 0.62.0) and they are not recreated. Two consequences to flag (Q2):
- Pins to those versions stop working: `tctl install --version 0.62.0`, `TRUSTY_VERSION=` in `install.sh`, and `select_exact_version` (`crates/trusty-installer/src/download/release.rs:546-580`) return not-found. crates.io copies stay.
- trusty-analyze and trusty-code have exactly one release in the 67, so for them "old version" and "current latest" are the same release; the patch is the whole replacement.
- Release notes: `release.yml:1862` runs git-cliff `--latest` against the previous `<crate>-v*` tag. With the old tags gone, the previous tag is the last survivor (for example search 0.54.5), so each new body spans all changes since then. Sum of the old 67 bodies per crate is at most 47,524 characters (mpm), under GitHub's body limit (125,000 per GitHub docs, not re-verified here). Low risk; bodies are editable after publish.

## 2. The patch-cut route

### 2.1 Per-crate steps (repo convention, `docs/reference/release-workflow.md:49-166`)
1. Bump: `scripts/bump-version.sh <crate-dir> patch` (rewrites `Cargo.toml`, syncs `Cargo.lock`, assembles `changelog.d/` into `CHANGELOG.md`, prints the tag commands). The crate's `changelog.d/<issue>-<slug>.md` fragment: one `Changed` line saying the release was re-cut after the history rewrite (Refs the D61 issue). A crate with a minor-ahead `main` needs Route B below.
2. PR, green CI, merge. Preflight before tagging: `scripts/preflight-publish.sh --check-only <crate>` (step 4b).
3. Tag `<crate>-v<new>` on the merged commit and push it. `check-tag-ancestry.sh` requires the commit to be on `origin/main` or on `release/<crate>-<semver>`.
4. `release.yml` fires on the tag: setup, pre-publish gate, build matrix (3 targets, plus AL2023 where configured), attaches assets and `.sha256` sidecars with git-cliff notes, then `homebrew-bump` (variable `HOMEBREW_TAP_ENABLED=true` and secret `HOMEBREW_TAP_TOKEN` both exist, GET of actions variables and secrets).
5. `check-publish-ready.sh` and `preflight-publish.sh`, then `SKIP_UI_BUILD=1 cargo publish -p <crate>` where the crate embeds a UI; install back with `cargo install <crate> --version <new> --locked` from outside the workspace (steps 7-9). Publish and release assets are independent; the repo's parity and version-parity guards expect both.
6. Release scale: trusty-mpm is LARGE (workspace test, code-critic round, pre-publish CI on the tag SHA); the others are SMALL (a patch of one crate, all changes rung 1-4). Multi-crate wave also counts as LARGE: `release-workflow.md:44-45` says "a multi-crate wave" is LARGE, so the whole wave is LARGE. Q4.

### 2.2 Route A and Route B
- **Route A, cut from rewritten `main`:** `main` version is the new version (code 0.7.1, installer 0.17.1, memory 0.36.5, search 0.62.2). The bump PR only assembles the changelog. The binaries then contain everything unreleased on `main` (for search 19 fragments, code 18), so they are not a rebuild of the old release.
- **Route B, release branch:** `git branch release/<crate>-<new> <rewritten old-release commit>`, then a PR that bumps the patch and adds the fragment, tag at the branch tip (D96 conditions: branch name matches `RELEASE_BRANCH_RE`, the commit's `Cargo.toml` version equals the tag, commit is the tip). Binaries equal the old release's source apart from the rewritten fixture paths. Needed for analyze, console, mpm, review if the new version must be a patch. Ruleset 26091257 on `release/*` requires a pull request and blocks creation and non-fast-forward; the sole bypass actor is repository role 5 (admin), which Bob holds. Branch creation must be done by Bob or with bypass.
- Mixed approach recommended for Q1: Route A for the 4 patch-ahead crates, Route B for the other 4. A minor on `main` (1.12.0, 0.13.0, 0.39.0) is the other option and is simpler, but it is not what D05 says.

### 2.3 Ordering under the freeze
- Nothing lands before the D61 rewrite push is verified (scan PASS, ref counts, protections restored). Bumps before that would be rewritten or lost.
- Prepare in advance, without pushing: the bump diffs as patch files (versions, `Cargo.lock`, changelog) and the Route B branch names. They apply to the rewritten tree without conflicts because the touched files are not in the 14 rewritten blobs. They are not committed or pushed until the freeze lifts.
- Freeze lift is two-stage (Q3): tm-dogfood unfreeze lets the 8 release PRs and the content PR merge; the 24 held PRs wait their rebase turn.
- The `tm content update` fix (section 3) must merge to `main` before the trusty-mpm cut; Route B for mpm would need it cherry-picked into the release branch, so Route A for mpm is easier (Q1).

### 2.4 Outage window and how to shorten it
From step D (releases and tags gone) the tap, `install.sh` and `tctl` point at nothing or at older versions:
- `install.sh` and the installer pick the newest remaining release: installer 0.13.8, mpm 1.7.9, memory 0.28.2, search 0.54.5, review 0.36.1, console 0.12.0, code 0.5.1, analyze 0.12.6 (table above). A fresh install silently installs those.
- Homebrew formulas for the 8 crates (`Formula/*.rb` lines 13/21, mpm 15/23) 404 until `homebrew-bump` writes the new URL and sha256.

Estimate, with the pieces measured from past runs (`gh api actions/workflows/290891315/runs`, success durations): push window about 1 to 2 minutes; D61 step 7 verification (fresh mirror and scan) 15 to 30 minutes; CI on the rewritten main 28 to 40 minutes (D61 plan); bump PR CI 28 to 40 minutes; `release.yml` runs: installer 5 to 6, console 6, memory 8 to 11, search 12 to 15, mpm 17 to 23 minutes (analyze, code, review not in the sample: assume 6 to 12); crates.io publish 5 to 15 minutes per crate. Serial total after the push: about 2 to 3 hours for the small crates, 3 to 4 hours for mpm (adds code-critic round and LARGE gates). Floor with all PRs opened in parallel the moment the scan passes: about 1.5 hours.

Ways to keep it short:
1. Do step D as late as possible: release downloads, patch files, branches, drafts-free state, tap diff and the content PR all ready beforehand.
2. Delete the 67 tags inside the same atomic rewrite push rather than as a separate step (one transaction: if it fails, the tags are intact and the push is repeatable; only the release deletions are already final).
3. Open all 8 bump PRs plus the content PR in parallel as soon as the scan passes; do not wait for CI on the new `main` before opening them (it runs on the PR).
4. Cut trusty-installer first (it drives install.sh), then trusty-mpm (flagship, longest), then the rest in parallel. macOS runner concurrency for the 8 simultaneous tag runs is unchecked; if limited, start mpm and search first.
5. Announce the window to the people who run `tctl upgrade` and `tm content update` (section 3).

## 3. Content (D06)

- **The fix.** Where: `crates/trusty-mpm/src/content/bundle_cache.rs`, function `latest_tag` (line 538; `source.release(&tag)` at about line 560). `GithubReleases::release` in `crates/trusty-mpm/src/content/release_source.rs` (about lines 523-545) maps a 404 to a `FetchError` with `status: Some(404)`; `latest_tag` turns every `FetchError` into `CacheError::Network`. The fix: treat a 404 for a stable tag as "no release" and continue to the next lower tag (the same loop that already skips pre-releases), keeping drafts handled as 404 per the existing comment. Doc comment at `bundle_cache.rs:525-537` says the opposite on purpose ("A draft, or a tag with no release, is an error, not passed over") and the test at `bundle_cache_tests.rs:268-283` (`update(...).expect_err("a draft is a 404")`) and `:813` (`expect_err("no release")`) pin it. Both change; this reverses a design decision from #8389/#9036, so Architect sign-off is needed (Q5).
- **Existing issue.** None found (read-only `gh issue list --search`, five phrasings). Related and closed: #9036 (matching-refs rework that introduced the behaviour), #9396, #8389. Needs a new issue; per `CLAUDE.md` search-before-filing, this is that search. Rung 3 (one crate, localized) plus a regression test that failed before.
- **Installed `tm` binaries keep the old behaviour.** Only a release after the fix helps. So the 6 old content tags must go in step D, otherwise `content-v2.3.0` (a tag without a release) is the highest stable tag and every existing `tm content update` errors.
- **The cut.** `content-release.yml` is `workflow_dispatch` only; a real cut must run against `main` (`content-release.yml:92-93`) and creates the tag through `gh release create --target "$GITHUB_SHA" --latest=false` (`:194-198`). Prerequisite: a PR setting `[bundle].version = "2.3.1"` in `content/manifest.toml` and rolling `content/changelog.d/` (16 fragments) into `content/CONTENT-CHANGELOG.md` (`docs/reference/content-release.md:10-17`). Dry run first (`-f version=2.3.1`), then `-f dry_run=false`. Released by local-ops with supervisor sign-off. The tag `content-v2.3.1` is free.
- **Order against deleting the old tags.** Not possible to publish first: `content-v2.3.1` must be created on a rewritten commit, which exists only after the rewrite push. So: step D (and the push) removes the 6 old tags; after the scan, merge the manifest PR, dispatch the cut. In the gap, the highest remaining content tag is `content-v0.1.0` (a release exists), so `tm content update` and the installer's content step would pin `content-v0.1.0`, a downgrade (and `trusty-review 0.39.0` needs files that bundle lacks, `crates/trusty-installer/src/commands/content_step_tests.rs:56-91`). Shortest is the content cut right after the scan, in parallel with the crate PRs (a bundle cut takes minutes, no build matrix). Pins to the removed versions (for example `tm content update --content-ref content-v0.2.0`, `crates/trusty-mpm/src/bin/tm/tests.rs:2103`) stop resolving; the cached bundle on installed machines still verifies by sha256.
- Content pins old bundle tags in lock files; the manifest tag must equal the lock tag (`crates/trusty-common/src/content/bundle.rs:194`), so nothing can be renamed. A new version is the only route, as D06 says.

## 4. Homebrew

- Tap `bobmatnyc/homebrew-trusty`, `Formula/<crate>.rb`. Known lines (read 2026-11-03): `version` at line 8; macOS `url` and `sha256` at lines 13-14; Linux at 21-22; `trusty-mpm.rb` at 15-16 and 23-24. Eight formulas change: analyze, code, console, installer, memory, mpm, review, search. `trusty-audit.rb` and `trusty-git-analytics.rb` point outside the 67 and stay.
- Who edits: `release.yml` job `homebrew-bump` (`:2033-2170`) does it per tag with `scripts/generate-homebrew-formula.sh`, using the rebuilt assets in `release-assets` for the sha256 values, then commits to the tap's `main` with `HOMEBREW_TAP_TOKEN`. No manual edit is planned. The sha256 values come from the NEW tarballs (rebuilt binaries are not byte-identical), so the current sha256 lines cannot be reused.
- Fallback if `homebrew-bump` fails for a tag: Bob (or the executor with the token) runs `generate-homebrew-formula.sh --crate <c> --version <v> --tag <c>-v<v> --binaries "<list>" --repo-slug bobmatnyc/trusty-tools --assets-dir <downloaded assets> --formula-dir <tap>/Formula` and commits. The job treats exit 3 (byte-identical render) as success.
- Check after each cut: `curl -sIL` on both URLs returns 200 and `brew fetch` for the formula on a clean machine (not run here).
- Repo fixtures `scripts/test-data/homebrew/expected/*.rb` pin old versions (mpm 1.3.6 and so on) not in the 67; no edit.

## 5. The 61 `-r` drafts

State read: 61 draft releases exist on origin now, all with `-r` tag names (live `releases` list: 445 entries = 384 + 61), manifest `.trusty-mpm/sessions/df-records/d7d-drafts-20261103/manifest.tsv` (61 rows; draft ids in column 4). They are unpublished, so they are mutable, hold no tag, and can be deleted. They also hold no information not in the local copy (`.../d7d-drafts-20261103/assets`, 4.2 GiB, digest-checked).

Plan: delete them after Bob/Architect approve this plan and before step D (so none can be published by mistake and the release list is clean for the post-cut checks). Command: one `DELETE repos/bobmatnyc/trusty-tools/releases/<draft-id>` per manifest row (61 calls), then `GET releases` must show 384 entries and `git ls-remote origin 'refs/tags/*-r[0-9]*'` must show no `-r<X.Y.Z>` tag. Keep the local assets until the Support purge confirms, as the only copy of the original 442 files after step D. The 6 content releases were never staged as drafts (61 = 67 minus 6).
Irreversible: no; the drafts can be restaged with `stage_drafts.sh` from the local copy. Do not use the drafts for the new releases: the new binaries are rebuilt and the names differ.

## 6. D08: is it moot?

Positions (not decided here):
- Architect: D08 is moot under D05; no `*-r*` tag will ever exist, so the rule would protect an empty set.
- tm-dogfood: D08 is NOT moot. Its reason is not stated in this plan or in the d61-step5 / d7d records.

Plan author's original reading: D05 replaces the `-r` scheme, the 61 drafts are deleted unpublished, and no `*-r*` tag will ever be created. A rule on `refs/tags/*-r*` protects an empty set. Do not add it; it also adds an unneeded entry to a ruleset the wrapper must disable and restore (D3e).

Other tag-protection changes worth raising (all optional, none part of this plan):
- Ruleset 26091265 includes `refs/tags/*-v*` and excludes `refs/tags/content-v*`. After the cut, `content-v*` tags are the only release tags without it, and the failure that triggered D06 came from a content tag without a release. Immutability protects a tag only while its release exists. Raise: include `content-v*`, or accept the gap. Bob decides.
- The same ruleset allows only the admin role to bypass, which the rewrite and the tag deletions rely on (D3e disables it for the window). No change needed.

## 7. Order of operations (revised)

Steps 5, 6, 9 and 12 below are superseded by section 10 (tm-dogfood review: 61 + 6 split, 98-ref push, content deletes after `content-v2.3.1`, gate moved).

Marks: `[R]` reversible, `[X]` irreversible. Architect GO per step as in the D61 plan.

0. `[R]` Approve this plan; settle Q1 to Q5.
1. `[R]` Backup (done, confirm): ref list, `d19-pre-rewrite.bundle`, release JSON, original assets (4.2 GiB local copy, digests per the earlier plan section 3.2).
2. `[R]` Delete the 61 `-r` drafts (section 5).
3. `[R]` Prepare, unpushed: 8 bump patches and Route B branch plan, the `tm content update` fix PR, the content manifest PR, preflight notes. Open the issue for the fix.
4. `[R]` D61 F0-F4: freeze acks, workflow disable (5 ids), snapshots, dry run, scan, leases.
5. `[X]` **D1: delete the 67 releases** (`DELETE releases/<id>`, newest first, ids logged). Names burn; ids, URLs, download counts (1,725) and attestations are gone. Window starts. Do not start until step 3 is ready and the executor can run the push within minutes.
6. `[X]` **D2, run by `run-push-window.sh` r3 inside one window:** pre-D lease recheck; step D (`step-d-61.sh --releases-only`, 61 release deletes); re-check (61 releases 404, 6 content releases present); post-D lease recheck; one atomic push of 98 ref updates plus the 61 tag deletes (159 refspecs, `push-cmd-98.sh --execute`); ref-count after-check (heads 52 unchanged, tags 931 to 870, else exit 4). The 6 `content-v*` tags and releases stay until after `content-v2.3.1` (section 10, item 5). The 30-minute deadline is checked only before step D. If the atomic push is rejected, the tags are intact and the push can be retried; only the release deletes are final. Protections and rulesets are restored on every exit. Workflows per M2(ii): after a push that exits 0 they stay disabled for step 7, and a watchdog runs `--restore-only` at push end + 1800 s unless step 7 passed; on any other exit the trap re-enables them. Owner of the M2 restore: tm-trusty-issues (executor).
7. `[R]` Verify: fresh mirror scan, ref-count check, no old SHA reachable from remaining refs, CI on `main`.
8. `[R]` Unfreeze stage 1 (tm-dogfood): release PRs and the content PR only.
9. `[R]` In parallel: merge the content manifest PR and dispatch `content-release.yml` (dry run, then real); merge the 8 bump PRs (Route A on `main`, Route B on release branches); merge the `tm content update` fix before the mpm bump.
10. `[X]` Per crate: push `<crate>-v<new>` (burns the name; assets lock on publish) and `release.yml` builds. Order: installer, mpm, then the rest in parallel. Check each release: assets, sha256 sidecars, `immutable: true`, `homebrew-bump` green.
11. `[X]` Per crate: `cargo publish` (crates.io versions are permanent), then `cargo install --version --locked` smoke test, local-ops.
12. `[R]` Tap check, `install.sh` and `tctl` resolve the new versions, `tm content update` resolves `content-v2.3.1`, re-enable workflows, `gh run list` shows no stray tag run except the 8 intended, unfreeze stage 2 (held PRs rebased).
13. `[R]` Optional: Bob decides on the `content-v*` protection (section 6).

## 8. Rollback limits

| After step | Cannot be undone | Restore path |
|---|---|---|
| 1 to 4 | Nothing | Delete local copies, re-enable workflows, restore protections from `*-before.json`; restage drafts from `stage_drafts.sh` if wanted. |
| 5 (D1) | 67 release names burned for releases; release ids and URLs; download counts; original attestations; external links. | Assets only from the local copy; old names cannot be republished. Tags still at old SHAs until step 6. |
| 6 (D2) | The 67 old tag names cannot be pushed again (probe). Old objects stay fetchable by SHA until Support purges (`refs/pull/*`, 8 forks, D61 section 5). Clones and forks that saw the tags. | Other 98 refs: push old objects back from the bundle clone, atomic and leased (D61 section 8). The 67 tags: none. |
| 8 to 9 | A merged PR on the rewritten `main`. | Revert PR. Content tag and release not yet created. |
| 10 | Each new tag name and release (immutable on publish); asset bytes; a wrong target commit. | None; the fix is the next patch number. |
| 11 | Each crates.io version (permanent; `cargo yank` hides but does not free the number). | Next patch. |
| 12 | Tap history only. | Revert tap commit. |

## 9. Open questions

For Bob:
1. **Q1.** Patch from where? Route A from `main` for code, installer, memory and search (versions 0.7.1, 0.17.1, 0.36.5, 0.62.2 already on `main`), and for analyze, console, mpm, review either Route B (patch from the old release: 0.12.8, 0.12.7, 1.11.2, 0.38.6, branch plus PR each) or accept `main`'s minor (0.13.0, 0.13.0, 1.12.0, 0.39.0, which ships 6 to 31 unreleased fragments each). The plan does not choose.
2. **Q2.** Accept that the 53 older versions disappear for `tctl install --version`, `TRUSTY_VERSION=` and the installer's pinned lookups, and that older content pins (`content-v0.2.0` and so on) stop resolving?
3. **Q9.** Does a crates.io publish go with every cut (repo convention: yes) or only the GitHub release and tap? Each publish burns a version permanently.

For the Architect:
4. **Q3.** Two-stage unfreeze (release PRs first, held PRs after) acceptable?
5. **Q4.** Is the 8-crate wave plus mpm one LARGE release (whole wave under the heavy gates) or per-crate scale? `release-workflow.md:44-45` says a multi-crate wave and the mpm binary are LARGE.
6. **Q5.** Approve reversing the "tag without a release is an error" design of #8389/#9036 in `latest_tag`, and the new issue for it.
7. **Q6.** Window: accept 2 to 4 hours of degraded install paths, or pre-notify users and pin `install.sh` fallbacks during it? Is it acceptable that a user running `tm content update` in the gap is pinned to `content-v0.1.0`?
8. **Q7.** Deleting the 67 tags inside the atomic rewrite push (recommended) versus a separate deletion step before it.
9. **Q8.** macOS runner concurrency for 8 simultaneous tag builds is unchecked; confirm or stagger.
10. **Q10.** Executor for steps 5 to 6: tm-trusty-issues with tm-dogfood reviewing the 67 release ids and the push command first, as before.

## 10. tm-dogfood review of step D (items 1 to 6)

Files, all under `/Users/masa/d19-private/d7d-scripts/` (outside the repo, written, not run; mode 600 data, 700 scripts). Hashes are in `SHA256SUMS-d7d-r2.txt (r3 hashes; supersedes `SHA256SUMS-d7d-followup.txt`)` beside them:

| File | sha256 | Content |
|---|---|---|
| `step-d-61.tsv` | `8b7a56e7522194f5e3ec4cb337507a535015c3fed9bdc0e8964b9a515b743baa` | 61 rows: release id, tag, expected tag SHA (step-5 ls-remote), expected commit SHA |
| `step-d-61.sh` | `97c132e53488900d5f54a1a26c2510e6e9efae7feb4b057b1ebbf71041af634d` | r2: release DELETE only (`--releases-only` mandatory for the 61), 200/404/stop status branch, copy-2 marker checked by hash; dry by default |
| `step-d-content-6.tsv` | `1be731fe84072e023b6dbf07d2d2a30a37152d90d8026897719faad15aba96d9` | same columns, 6 content-v* rows, separate |
| `refs-98.txt` | `5e4328cbb9246b2315b0488d5c49bd3375b708325ef18a861bd82168c6d5132f` | 98 refs (38 heads, 60 tags): ref, lease (old SHA), rewritten SHA |
| `push-cmd-98.sh` | `7742346f92f803aae95e203004e249714659de8b9a8b00d47f23820f76c5deff` | r3 (df-D99: recheck fails closed off 52/931; was r2 `c95eb94c…`): print, recheck, dry-run, and `--execute` (always 98 updates + 61 tag deletes = 159 refspecs; only under the wrapper) |
| `asset-copy-2.sh` | `7c9f5eb51d2969d1d2e7d14cbe353b20e8db2b053d548c3a9f32da1c534cd9d0` | r2: copy 2 of all 67 releases (442 assets + 2 metadata) in `d7d-assets/`, whole-copy re-hash; marker `.verified-ok` sha256 `856f1cbcac50465d373d992d218acc5e939726e49dc1906a934165009bae5270` |
| `drafts-cleanup-61.sh` | `cbb4eb408f6cb9b888e313c72416c35075c2fefab9618ed63c252aca9f82ae93` | r2: delete the 61 `-r` drafts after the copy-2 hash check; 200/404/stop status branch |
| `run-push-window.sh` (in `/Users/masa/d19-private/`) | `dc1706b9e54480223478d57329da973b4d5a004896e4acfb184d0d114d922d48` | r4 wrapper (df-D99 re-pin; was r3 `39b7d06a…`; ran step D 2026-11-07T01:25Z): pre-D recheck, step D, post-D checks, push, ref-count check, M2 watchdog; `wrapper-r2.diff`, `wrapper-r3.diff` |

Checks done while writing (read-only): the 61 + 6 split equals `gh013-tags.txt` (67); release ids come from the live API; leases equal the step-5 `ls-remote-before-step5.txt` for all 165 refs; every one of the 98 refs in `/Users/masa/d19-run/work.git` equals the rewritten SHA in `refs-after-sha.txt` (0 mismatches); no rewritten SHA equals its old SHA.

**Irreversibility, stated plainly.** Deleting a release is irreversible (name burned for releases, id, URL, download count, attestation lost). Deleting a TAG is also irreversible: the same name cannot be pushed again (probe P: GH013, creations restricted). Neither a backup bundle nor the rollback in D61 plan section 8 can bring a deleted tag name back. Every `--execute` in these scripts needs `--confirm-sha256 <hash of its TSV>`, so the reviewed file is the one that runs.

**Item 1, delete list narrowed to 61.** `step-d-61.tsv` holds the 61 non-content ids. The 6 `content-v*` rows are only in `step-d-content-6.tsv` and are never in the same run. `asset-copy-2.sh --content` takes the missing backup (12 files, about 4 MB, `gh release download` plus API digest check) into `/Users/masa/d19-private/d7d-assets-content/` before any content delete.

**Item 2, executable 61-row file.** `step-d-61.sh --tsv step-d-61.tsv` replaces the "D. Delete the 67 releases" bullet and its tap-window sub-bullet (old-plan lines 216 and 218) and plan section 7 step 5. Preconditions it reads before acting: copy-2 marker, the five B1 workflows disabled, ruleset 26091265 `enforcement=disabled` (tag deletes through the API), and per row the release id, `tag_name`, `draft=false`, `immutable=true` and the tag object SHA. A moved tag SHA aborts the run. Stops on the first error; a re-run skips rows already gone. It must run inside the B1 and B3 window opened by the D3e wrapper; the script cannot open the window itself.

**Item 3, second hashed copy.** `asset-copy-2.sh` copies `.../d7d-drafts-20261103/assets` (61 release dirs, 430 files, 4.2 GiB) to `/Users/masa/d19-private/d7d-assets/`, verifies each source and copy against the API digests in `releases-all.json`, checks counts and bytes against `manifest.tsv`, writes `SHA256SUMS` per release and `copy-manifest.tsv`, and only then writes `.verified-ok`. Both `step-d-61.sh --execute` and `drafts-cleanup-61.sh --execute` refuse to run without it. Caveat: both copies are on this Mac's internal disk. For independence from disk loss, add an external or off-host copy before D (Q11).

**Item 4, 98 refs with new leases.** `refs-98.txt` is the 165 step-5 refs minus the 67 release tags: 38 heads (including `main`: lease `bac374040d`, rewritten `48bbda1fc0`) and 60 tags. Leases are the step-5 values. **Lease risk:** I re-read every head and tag on origin by GET (`git/matching-refs/heads` 52, `/tags` 931) at the time of writing: all 165 refs still equal the step-5 values and no ref outside step 5 exists. That is a snapshot, not a guarantee. Session pushes, branch deletion after a merge, or a force-push by a frozen session before the window would make a lease fail (the push is atomic, so nothing partial lands) or, worse, leave the rewritten ref unaware of new commits on an unleased head. Re-check immediately before the push with `push-cmd-98.sh --recheck [--with-tag-deletes]` (GET only; prints every moved ref and exits 1; also counts refs outside the 98). Branch protection and the pre-push gate remain the other guard. If `--recheck` reports a moved ref: do not push; regenerate `refs-after-sha` and `refs-98.txt` from a fresh F1, as the D61 plan says.

**Item 5, scan and unfreeze gate after D06; the residue.** The 6 content tags cannot move with the push (their immutable releases lock them), and cannot be deleted by the push either, because a tag cannot be deleted while its release exists. Deleting their releases first would put `tm content update` on `content-v0.1.0` for hours (section 3). D06 says cut `content-v2.3.1`, then delete the 6, and that order also removes the content outage. So:
1. The 98-ref push excludes the 6. Between the push and their deletion the residue is 6 tags and 6 releases (`content-v0.2.0`, `2.1.0`, `2.1.1`, `2.2.0`, `2.2.1`, `2.3.0`) still on pre-rewrite commits. Because `content-v2.3.0` sits on `main` ancestry, almost all of the old history stays reachable on origin through these tags until they go. The D61 scan therefore cannot PASS at its normal point: `for-each-ref --contains 26c066888a` would list exactly those 6.
2. Gate A (after the push): fresh mirror, scan with the expected residue (exactly the 6 names, nothing else; the `scan_post-20261102-1636.py` allow-list option is not confirmed to exist, so the check is `for-each-ref --contains 26c066888a` equal to the 6, plus the existing blob and tree checks), ref counts (tags 931 - 61 = 870, heads unchanged), protections and rulesets restored, workflows still disabled. Gate A permits unfreeze stage 1 only (content PR, release PRs).
3. Cut `content-v2.3.1`; then `step-d-61.sh --tsv step-d-content-6.tsv --require-release content-v2.3.1` (after `asset-copy-2.sh --content`), which refuses to run unless the new release is published and immutable. Content tags are outside ruleset 26091265, so no ruleset change is needed for these deletes.
4. Gate B (the real D61 scan and unfreeze stage 2, held-PR rebases, Support request) runs only after step 3. Until then the Support request is not sent: it would be built on a residue.
5. The residue lasts from the push to the content cut and delete: the content PR CI plus a dry run plus the cut, about 1 to 2 hours after Gate A. Also unchanged: `refs/pull/*` and 8 forks keep old SHAs fetchable until GitHub Support purges, whatever order is used.

**Item 6, drafts after the external copy.** `drafts-cleanup-61.sh` checks the marker's pinned sha256, that `copy-manifest.tsv` has 445 lines (442 assets + 2 metadata + header), and that the marker names the manifest's sha256, before deleting anything; each draft must read `draft=true` with the `-r` tag name. It belongs after copy 2 and before D (section 7 step 2).

**Conflict: tag deletes inside the atomic push, or 98 refs plus separate tag deletes.**
Recommendation: put the 61 tag deletions inside the atomic push (`push-cmd-98.sh --with-tag-deletes`: 98 updates + 61 deletes = 159 refspecs, one transaction) and run `step-d-61.sh --releases-only`. Reasons:
1. A separate tag delete before the push is unrecoverable if the push then fails: the 61 old names are gone for good (probe), so rollback from the bundle (D61 section 8) cannot put them back, and the repo sits with 61 missing tags and an unrewritten history. Inside the push, a rejection leaves every tag untouched and the push can be repeated.
2. The deletion and the rewrite become one event. No state exists where tags are gone but heads are not rewritten.
3. The cost is auditability: per-row logs for tags come from the push output plus a post-push check that none of the 61 refs exist (`step-d-61.sh` re-run is a no-op for absent tags; `push-cmd-98.sh --recheck` shows them GONE). The release deletes keep the per-row log and stop-on-first-error.
Where dogfood's route is better: a separate delete exposes any surprise on tag deletion at row 1, before the big push. Probe P already showed the delete works, and the push's `--dry-run` cannot evaluate GH013, so this advantage is small. **Q12 settled (Architect, 15:31Z): the 61 tag deletes go inside the atomic push** (159 refspecs, `push-cmd-98.sh --execute`, which always carries them). `step-d-61.sh` r2 has no API tag-delete path for the 61; `--releases-only` is mandatory for `step-d-61.tsv`.

Also confirmed: trusty-memory's next patch is 0.36.5 or higher (crates.io max 0.36.4, `main` already 0.36.5); section 1 stands.

Open questions added: **Q11** (Bob/Architect) external or off-host copy of the 4.2 GiB before D, or accept same-disk? **Q12** settled: in-push tag deletes (Architect 15:31Z). **Q13** (Architect) accept Gate A with the 6-tag residue and a two-gate unfreeze, and the scan adapter for it?

## Evidence index
- Versions: `git show origin/main:crates/<c>/Cargo.toml`; crates.io `/api/v1/crates/<c>`.
- Releases and drafts: `gh api --paginate repos/bobmatnyc/trusty-tools/releases` (445 entries, 61 draft).
- Run durations: `gh api repos/bobmatnyc/trusty-tools/actions/workflows/290891315/runs`.
- Tap: `repos/bobmatnyc/homebrew-trusty/contents/Formula/*.rb`; variables `HOMEBREW_TAP_ENABLED=true`; secret name `HOMEBREW_TAP_TOKEN` present.
- Code: `release.yml:1573-1580,1862,2033-2170`; `bundle_cache.rs:525-565`; `release_source.rs:495-545`; `content-release.yml:92-198`; `release-workflow.md:44-166`; `release.rs:237-262,546-580`.
