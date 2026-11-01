# ar-T44 release readiness (origin/main 828560d574, 2026-10-31)

Read-only. Evidence: `.claude/scratch-ar-t44/triage-{A,B,C1,C2,C3}.md`, `git merge-base --is-ancestor` against each crate's last tag, GitHub REST. Preflight was read, not run.

## Cross-cutting

| Item | Effect |
|---|---|
| trusty-common is 0.67.5 on main, crates.io has 0.67.4 (2 fragments) | Both mpm 1.12.0 and search 0.62.2 consume 0.67.5 (`registry-index-id`, [#10531](https://github.com/bobmatnyc/trusty-tools/issues/10531)). **trusty-common 0.67.5 must publish first.** It is in none of the three releases. |
| [#10639](https://github.com/bobmatnyc/trusty-tools/issues/10639) (trusty-common INT8 reference test red on main; diagnostic fix 828560d574 landed, issue open) | Main green is a precondition for the trusty-common gate and everything above it. |
| [#10507](https://github.com/bobmatnyc/trusty-tools/issues/10507) version-parity drift on main (open; bot comments 09:22Z, crate not named in the body) | Preflight CHECK 6 / release.yml parity fails closed. Identify the crate before any cut. |
| [#10587](https://github.com/bobmatnyc/trusty-tools/issues/10587) gh shim `GH_TOKEN` fails `gh auth status` | Blocks preflight CHECK 2 on the publishing host. |

## 1. trusty-mpm 1.12.0 (ar-G0b)

| Current state | Value |
|---|---|
| Cargo.toml on main | 1.12.0 (already bumped) |
| crates.io | 1.11.1 |
| Latest tag | `trusty-mpm-v1.11.1` |
| Pending fragments | 11 in `crates/trusty-mpm/changelog.d/` |

Cut blockers (5):

| # | Blocker |
|---|---|
| 1 | [#10610](https://github.com/bobmatnyc/trusty-tools/pull/10610) Architect dashboard + Intent Ledger: open, `do-not-merge`, history rewritten per De8; owner call whether it is in 1.12.0 (milestone has 19 open issues). Phase 2 [#10628](https://github.com/bobmatnyc/trusty-tools/pull/10628) is stacked and also held. |
| 2 | Other held PRs targeting the milestone: [#10625](https://github.com/bobmatnyc/trusty-tools/pull/10625) (self-clear root, #10599), [#10626](https://github.com/bobmatnyc/trusty-tools/pull/10626) (SIGTERM keeps sessions, #10528 owner decision open), [#10259](https://github.com/bobmatnyc/trusty-tools/pull/10259) build lease. Drop or land. |
| 3 | trusty-common 0.67.5 unpublished; [#10639](https://github.com/bobmatnyc/trusty-tools/issues/10639) red; [#10507](https://github.com/bobmatnyc/trusty-tools/issues/10507) parity (see above). |
| 4 | Semver ledger: [#10511](https://github.com/bobmatnyc/trusty-tools/pull/10511) (1.10.0 accepted breaks) is open and `do-not-merge`. CHECK 5 does not block on a computed break, but it is the ledger for the last cut. |
| 5 | Open P1 trusty-mpm bugs the owner may want in the cut: [#10570](https://github.com/bobmatnyc/trusty-tools/issues/10570), [#10565](https://github.com/bobmatnyc/trusty-tools/issues/10565), [#10600](https://github.com/bobmatnyc/trusty-tools/issues/10600), [#10586](https://github.com/bobmatnyc/trusty-tools/issues/10586) (triage-A). Not preflight failures. |

## 2. Content publish (`content-vX.Y.Z`; no crate, no crates.io; workflow `content-release.yml`)

| Current state | Value |
|---|---|
| `[bundle].version` on main | 2.3.3 |
| Latest tag / release | `content-v2.3.0` (published 2026-10-29T12:46Z, carries PR #10252). 2.3.1, 2.3.2 never tagged. |
| `CONTENT-CHANGELOG.md` top section | 2.3.0 (fragments not rolled up) |
| Pending fragments | 13 in `content/changelog.d/` |

Cut blockers (3):

| # | Blocker |
|---|---|
| 1 | A PR must roll the 13 fragments into `CONTENT-CHANGELOG.md` under the version being cut (docs/reference/content-release.md: workflow refuses a manifest/tag mismatch). Owner decides 2.3.3 vs a lower unused number; the gate only requires the version never goes down. |
| 2 | [#10228](https://github.com/bobmatnyc/trusty-tools/pull/10228) (Architect agents, skills, verify-claim scripts, touches content): draft, `do-not-merge`. Decide in or out. |
| 3 | `local-ops` dispatch with supervisor sign-off; dry run first (`dry_run` default). |

## 3. trusty-search 0.62.2

| Current state | Value |
|---|---|
| Cargo.toml on main | 0.62.2 (already bumped) |
| crates.io | 0.62.1 |
| Latest tag | `trusty-search-v0.62.1` |
| Pending fragments | 19 in `crates/trusty-search/changelog.d/` |

Cut blockers (3):

| # | Blocker |
|---|---|
| 1 | trusty-common 0.67.5 must be on crates.io first (search pins its feature set). |
| 2 | [#10639](https://github.com/bobmatnyc/trusty-tools/issues/10639) and [#10507](https://github.com/bobmatnyc/trusty-tools/issues/10507) (see above). |
| 3 | Open search test bugs in the SMALL-gate rung 4 run: [#10633](https://github.com/bobmatnyc/trusty-tools/issues/10633) (bm25 debug-log test fails Linux/Mac), [#10458](https://github.com/bobmatnyc/trusty-tools/issues/10458). Red `test -p trusty-search` blocks the gate; PRs: none open for either. |

The releases above are not SMALL if both mpm and trusty-common change version in the same window (release-workflow.md line 44 SMALL = one crate); use the LARGE gate.

## Issues closing at the live check

| Issue | Release | Live check |
|---|---|---|
| [#10260](https://github.com/bobmatnyc/trusty-tools/issues/10260) | mpm 1.12.0 (PR #10561) | Next tm install: build-lease census ignores threads. |
| [#10262](https://github.com/bobmatnyc/trusty-tools/issues/10262) | mpm 1.12.0 (PR #10559) | `tm fleet init` accepts a dir with its own linked worktrees. |
| [#10357](https://github.com/bobmatnyc/trusty-tools/issues/10357) | mpm 1.12.0 (PR #10563) | `tm fleet` restores `tm-architect-poll`. |
| [#10425](https://github.com/bobmatnyc/trusty-tools/issues/10425) | mpm 1.12.0 (PR #10564) | Doctor dry-run and `--only` on the installed tm. Conditions 3-4 live in #10565. |
| [#10548](https://github.com/bobmatnyc/trusty-tools/issues/10548) | mpm 1.12.0 (0e4925d047) | Confirm the release contains the commit; credential rules no longer false-positive. |
| [#10566](https://github.com/bobmatnyc/trusty-tools/issues/10566) | mpm 1.12.0 (PR #10623, rung 5) | Next tm install: `$()` and rebound names refused. |
| [#10622](https://github.com/bobmatnyc/trusty-tools/issues/10622) | mpm 1.12.0 (PR #10629) | Already 20/20 PASS; closes when the release contains 175ed67be0. |
| [#10614](https://github.com/bobmatnyc/trusty-tools/issues/10614) | mpm 1.12.0 (PR #10615) | `session pause` never pushes its ref to a public origin. |
| [#10071](https://github.com/bobmatnyc/trusty-tools/issues/10071) | mpm 1.12.0 (trusty-events; already merged, in 1.10.x+) | Restart daemon onto the new build, then the hung-daemon heartbeat path. |
| [#10042](https://github.com/bobmatnyc/trusty-tools/issues/10042) | mpm 1.12.0 | Same: restart daemon, then tctl-supervised launchd check. |
| [#10127](https://github.com/bobmatnyc/trusty-tools/issues/10127) | mpm 1.12.0 | P2b live check on the installed build. Last comment: P3 #10147 held for D24 (deny-list contents). Owner decision. |
| [#10531](https://github.com/bobmatnyc/trusty-tools/issues/10531) | mpm 1.12.0 + trusty-common 0.67.5 + search 0.62.2 (PR #10613 touches all four) | `search.project.ensure` on a sandbox daemon (`scripts/sandbox_search_daemon.sh`); closes only when every carrier is On Prod. |
| [#10545](https://github.com/bobmatnyc/trusty-tools/issues/10545) | search 0.62.2 (5f0290d4d7) | After install: `cargo audit` / `cargo deny` exit 0 on the shipped lockfile. |
| [#10551](https://github.com/bobmatnyc/trusty-tools/issues/10551) | search 0.62.2 (365eb8e9d9) | Release smoke: resume of a failed semantic stage; auto-retry ticker not yet exercised live. |
| [#10532](https://github.com/bobmatnyc/trusty-tools/issues/10532) | search 0.62.2 (77e9171eff) | Next trusty-search install: std-v1 profile. |
| [#10079](https://github.com/bobmatnyc/trusty-tools/issues/10079) | content-v2.3.0 (already released; PR #10252 is in the tag) | Verify the epic on an installed bundle. The "not yet released" comment of 10-29 08:00Z is stale; the tag was cut 12:46Z. |
| [#10450](https://github.com/bobmatnyc/trusty-tools/issues/10450) | **Next content cut** (neon-postgres is not in `content-v2.3.0`) | Owner set closure at publish; conditions 1-2 verified live after the cut. |

## Not covered by the three releases

| Issue | Crate | Where it ships |
|---|---|---|
| [#10083](https://github.com/bobmatnyc/trusty-tools/issues/10083) | trusty-common + trusty-mpm | Fix 9188eb43ea is already in `trusty-mpm-v1.11.1`. Shipped; live check is "no child-fd flake after install". |
| [#10514](https://github.com/bobmatnyc/trusty-tools/issues/10514) | trusty-common | Fix ecba9ebe10 is in the mpm 1.11.1 tag. Reaches crates.io with trusty-common 0.67.5 (not in the three). `status:merged` already set. |
| [#10506](https://github.com/bobmatnyc/trusty-tools/issues/10506) | CI (`tag-publish` parity, no crate) | 8784d34c2e is in the 1.11.1 tag. Live check: `--vcs-info auto` on the next tag-publish run (the trusty-common cut). |
| [#10609](https://github.com/bobmatnyc/trusty-tools/issues/10609) | trusty-memory 0.36.4 | Cargo.toml and crates.io both 0.36.4 (published). Live check: default recall excludes the retired `fact_key` occupant on the installed daemon. |
