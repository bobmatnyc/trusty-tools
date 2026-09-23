# Accepted SemVer breaks

One file per release that the owner has allowed to ship a public-API break
without a breaking version bump. `scripts/preflight-publish.sh` CHECK 5 reads
`<package>-<version>.txt` here only when the gate computes a BREAK for that
exact package and version. The rule, and every case where it fails closed, is
in [semver-gate.md, "Accepted breaks"](../../docs/reference/semver-gate.md#accepted-breaks-owner-ruling-2026-09-22).

A file lands on `main` through a reviewed PR before the release. It must be a
plain tracked file (mode 100644): CHECK 5 reads the content committed at HEAD
and refuses a symlink, an untracked file, or an edited working copy. Leave it
in place after the release as the record of what shipped.

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
