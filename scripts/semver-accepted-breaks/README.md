# Accepted SemVer breaks

One file per release that ships a public-API break. `scripts/preflight-publish.sh`
CHECK 5 writes `<package>-<version>.txt` here itself, automatically, whenever
it computes a BREAK for that exact package and version — this is the crate's
durable record of a break the trusty-tools internal numbering policy never
required a version bump for. The policy, and what CHECK 5 does with a computed
break, are in
[semver-gate.md](../../docs/reference/semver-gate.md#versioning-policy-owner-ruling-2026-09-26-stands-until-the-owner-revokes-it).

**Auto-recorded, not hand-declared (owner ruling 2026-09-26).** `semver_decide`
in `scripts/preflight-publish.sh` calls `semver_record_break`, which
regenerates this file from what the gate computed on THIS run — same bytes on
every re-run over the same gate output, so nothing here is ever hand-edited or
duplicated. A break here never fails the publish and never picks a version;
CHECK 5 prints `[WARN] semver: RECORDED BREAK` and continues. Leave the file in
place after the release as the record of what shipped; a later version's break
gets its own file.

**This supersedes the 2026-09-22 declare-first flow for CHECK 5 only.** The
pull-request-time `Public API / SemVer` check
(`scripts/semver_ci_accept.sh`) is unchanged and still reads a
hand-committed declaration through `semver_accept_present` /
`semver_accept_decide` in `scripts/lib/semver_accepted_breaks.sh` — a PR still
cannot merge an unbumped break with no declaration on its base. Only the
release-time gate (CHECK 5) stopped needing one.

## Format

Whitespace-separated rows. `#` starts a comment line.

```text
crate   trusty-mpm
version 1.7.0
reason  Owner ruling 2026-09-22: accept the breaking API changes on main
accept  enum_variant_added              ManagedError
accept  derive_trait_impl_removed       BuildersConfig Eq
accept  method_parameter_count_changed  ClaudeCodeAdapter::new
```

- `crate`, `version`, `reason`: exactly one each. `crate` and `version` must
  equal the file name and the publish.
- `accept <lint> <item>...`: covers every `Failed in:` entry of `<lint>` that
  contains all of the item tokens as whole tokens. `ManagedError` does not
  cover `ResumeManagedError`. Every entry the gate prints needs a covering row.
