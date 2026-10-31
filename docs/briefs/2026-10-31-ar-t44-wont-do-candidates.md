Won't-do candidates for owner review (ar-T44, 2026-10-31). Each row would close as not planned.

| # | issue | crate/area | age (days) | priority | why won't-do (one line) |
|---|---|---|---|---|---|
| 1 | [#7570](https://github.com/bobmatnyc/trusty-tools/issues/7570) | trusty-mpm / issue audit | 49 | P3 | `--repo` override for cross-repo audit; `cd` into the repo is the workaround; no owner ask since 2026-09-12; no linked PR |
| 2 | [#8581](https://github.com/bobmatnyc/trusty-tools/issues/8581) | trusty-mpm / test-hermeticity | 36 | P2 | Cleanup hooks after a rate-limit kill; one observation (2026-09-25), 0 comments, harness kills the agent so tm cannot run the hook; no linked PR |
| 3 | [#10489](https://github.com/bobmatnyc/trusty-tools/issues/10489) | docs / specs | 2 | none | Reformat DOC-86 to SPEC- anchors; grandfathered past `check_sld.sh` by ruling df-D43; no user impact; the one linked PR (#10490) is closed |
| 4 | [#10562](https://github.com/bobmatnyc/trusty-tools/issues/10562) | trusty-mpm / build-lease | 1 | P3 | Drop `serial_test` from 11 unleased-cap tests; tests pass today; cleanup only, no user impact; no linked PR |

Totals: 4 candidates (P2 x1, P3 x2, unlabelled x1; age 1 to 49 days). If all approved: open count 249 -> ~245.
