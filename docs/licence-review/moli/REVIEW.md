# moli licence review note (D31, slice S1)

Date: 2026-10-12. Status: findings recorded, decisions OPEN.
Not legal advice. A person with licence authority signs off the decisions below.

## Scope and evidence

D31 offers moli, lexmount's headless browser, as an opt-in skill that the user installs.
Distribution case U (the user fetches upstream's binary) is the recommended case.
This note decides nothing. It records what the evidence shows and what each open decision would settle.

- Reviewed: moli tag `v1.1.16`, SHA `9616d01ab1faf30fef389d6dbce234e7019d1e18` (step0-findings.md:3-7). Any pinned tag or binary hash in the skill must match this SHA, or this note is re-run at the new tag.
- Method: `cargo deny` 0.20.2 `check licenses`, run on EVO inside a container with the trusty-tools root `deny.toml` unchanged (step0-findings.md:61-63). Nothing was built or run from moli.
- Evidence files, all under `.trusty-mpm/plans/moli-poc/step0/` (local, gitignored): `deny.out`, `license-table.txt`, `deny-list.tsv`, `step0-findings.md`. Line numbers below cite these.
- This note was written from those files only. No new scan ran.

## Cases

| Case | What we do | Duties that fall on us |
|---|---|---|
| U | The skill tells the user to fetch upstream's release binary (or build with `cargo install --git`). We ship only our own skill text. | Our skill text carries a licence. We state what the user fetches and from where. We do not misstate moli's licence. |
| D | We host or bundle a prebuilt `moli` binary. | Notices for every crate, an MPL-2.0 source offer, the IJG acknowledgement, and the licence status of the three crates with no `license` field. |

## Decisions this note waits on (all OPEN, owner Bob)

| ID | Question | Recommended answer (not a decision) |
|---|---|---|
| D32 | Which case: U only, or D? | U only. D needs its own review. |
| D33 | Accept the WTFPL and IJG exceptions, and the residual risk of the three crates with no licence field? | Accept under U. Under D, treat the three crates as a blocker. |
| D39 | May an upstream issue go to lexmount/moli asking for `license.workspace = true` in the three manifests? Is it a ship gate? | File the issue; do not make it a ship gate. The Architect's review (B4, option 1) says the same. |

No issue has been filed, nothing has been sent upstream.

## Findings

Columns "Case U" and "Case D" give the obligation. "Waits on" names the open decision.

| # | Crate and version | Licence | Evidence | Case U | Case D | Waits on |
|---|---|---|---|---|---|---|
| 1 | `jpeg-encoder` 0.6.1 (via `moli-image`) | `(MIT OR Apache-2.0) AND IJG` | `deny.out:1-31` (manifest line `jpeg-encoder-0.6.1/Cargo.toml:33`); `step0-findings.md:64`; `deny-list.tsv:205` | None for us. IJG is permissive. Its duty is an acknowledgement in documentation shipped with a binary, and upstream ships the binary. | Blocker until the acknowledgement ("this software is based in part on the work of the Independent JPEG Group") ships with the binary. The crate's own licence text was not read; check the exact wording first. IJG is an AND term, so it applies even though MIT or Apache could be chosen for the rest. | D33 (accept), D32 |
| 2 | `typed-num` 0.3.0 (via `moli-cookie-cache`, `moli-opfs`, `moli-renderer-v8`) | `WTFPL` | `deny.out:164-231` (manifest line `typed-num-0.3.0/Cargo.toml:26`); `step0-findings.md:65` | Accept as permissive with no conditions, or record as an unusual-licence risk. An unusual licence is a policy call. | Same, plus a choice between "allow with justification" and "ask upstream to replace `typed-num`". | D33 |
| 3 | `moli-html-input-type` 0.1.0 | none declared | `deny.out:33`, `:83`; `step0-findings.md:66` | Residual risk. Upstream ships `LICENSE-APACHE` and `LICENSE-MIT`, and sibling crates use `license.workspace = true` (`step0-findings.md:66`); that is inference, not a statement by upstream. A missing answer is a risk note, not a blocker. | Blocker. Needs written upstream confirmation, or manifests fixed at a tag we pin. | D33, D39 |
| 4 | `moli-url-policy` 0.1.0 | none declared | `deny.out:86`, `:135`; `step0-findings.md:66` | As row 3. | As row 3. | D33, D39 |
| 5 | `moli-window-features` 0.1.0 | none declared | `deny.out:138`, `:161`; `step0-findings.md:66` | As row 3. | As row 3. | D33, D39 |
| 6 | MPL-2.0 set, 17 crates: `app_units` 0.7.8, `cssparser` 0.37.0, `cssparser-macros` 0.7.0, `dtoa-short` 0.3.5, `moli-site` 0.1.0, `moli-xpath` 0.1.0, `selectors` 0.40.0, `stylo` 0.20.0, `stylo_atoms`, `stylo_derive`, `stylo_dom`, `stylo_static_prefs` (all 0.20.0), `stylo_taffy` 0.3.0-beta.1, `stylo_traits` 0.20.0, `to_shmem` 0.5.0, `to_shmem_derive` 0.1.0, `uluru` 3.1.0 | `MPL-2.0` (`moli-site` is `(MIT OR Apache-2.0) AND MPL-2.0`) | `license-table.txt:19`; `step0-findings.md:90` | State in the skill that MPL-2.0 is file-level copyleft and that moli is not purely MIT or Apache, because `moli-site` and `moli-xpath` are moli's own MPL crates. No other duty. | Source offer: link the exact upstream tag and SHA above for the MPL files, unmodified. Publish any patch to them. | D32 |

The MPL-2.0 licence is already in the root `deny.toml` allow-list, so it needs no exception row.

Counts across the graph: 576 crates; no GPL, LGPL, AGPL or SSPL (`step0-findings.md:68-91`, `license-table.txt`).
`license-table.txt:49` lists `jpeg-encoder` as `Apache-2.0 / IJG / MIT`, which flattens the manifest's `AND`. The manifest expression (`deny.out:4`) is the authority.

## Gaps (not reviewed)

1. V8, libc++, PartitionAlloc and ICU data inside the prebuilt `librusty_v8` archive were never scanned. `cargo deny` does not read the contents of a linked prebuilt archive (`step0-findings.md:94`). The notices shipped with the rusty_v8 release at the pinned tag were not read.
   - Case U: record as "not reviewed; upstream's release".
   - Case D: blocker. Read those notices first.
2. Dependency drift. The graph can change at a new tag. Re-run the scan at any tag other than `v1.1.16` and re-check for GPL-family licences.
3. Crates.io. Plan assumption A1 (moli is not on crates.io) rests on its git dependencies and `[patch.crates-io]` rows (`step0-findings.md:45`). No registry query was made.
4. The crates' own licence files (IJG wording, the repository's `LICENSE-*` files) were not read; only manifests and the `cargo deny` output were.

## Our own skill text

The Architect decided that the skill text is under this repository's own licence.
The repository ships a single `LICENSE`, which is MIT (Bob Matsuoka, 2026); there is no `LICENSE-APACHE`. The Architect's review said "MIT OR Apache-2.0 pending a check of the LICENSE files". The check shows MIT only. The skill text is therefore MIT, unless the Architect says otherwise.

## deny.toml beside this note

`docs/licence-review/moli/deny.toml` holds the root allow-list plus two per-crate `[[licenses.exceptions]]` rows (`jpeg-encoder@0.6.1` for IJG, `typed-num@0.3.0` for WTFPL). Each is commented as pending D33.
It has no `clarify` rows, so the three crates stay `unlicensed` errors. It is not wired into CI. The root `/deny.toml` is unchanged.
The file has not been parsed or run. The expected result on EVO is exactly three `error[unlicensed]` and nothing else; a `clarify` case cannot be exercised until upstream answers.

## Ship gate under Case U

The skill may ship once D32 and D33 are answered and this note is current for the pinned tag. D39 (the upstream issue) is not a gate unless Bob makes it one.
