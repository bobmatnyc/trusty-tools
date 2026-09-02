# trusty-sld-lint

Repository linter and traceability reporter for Spec-Linked Documentation
(SLD). It validates the reference grammar, frontmatter, anchors, catalog
entries, revision drift, and the relationship between behavior contracts and
code described by [DOC-38](../../docs/specs/spec-linked-documentation.md).

Run the required workspace check through its guarded wrapper:

```bash
bash scripts/check_sld.sh
```

Run the binary directly when developing the linter:

```bash
cargo run -p trusty-sld-lint -- --root .
cargo run -p trusty-sld-lint -- gap-report --root .
```

The default lint fails on non-allowlisted errors. `--strict` applies all
spec-document conventions to every specification and verifies that allowlist
entries still correspond to real findings. `gap-report` is read-only and exits
successfully by default; add its own `--strict` flag only when gaps should fail
the command.

The implementation and public API are in [`src/lib.rs`](src/lib.rs). The
workspace allowlist is [`.sld-lint-allowlist.tsv`](../../.sld-lint-allowlist.tsv).

## Development

```bash
cargo check -p trusty-sld-lint
cargo test -p trusty-sld-lint --no-fail-fast
cargo clippy -p trusty-sld-lint --all-targets --all-features -- -D warnings
```

Licensed under the [MIT License](../../LICENSE).
