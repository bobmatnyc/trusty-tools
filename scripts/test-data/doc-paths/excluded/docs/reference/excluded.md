# excluded

One line per EXCLUDED TOKENS class in `check_doc_paths.sh`'s header. Not one of
these tokens resolves as a literal path, and the gate must still exit 0 — that
is what makes the class an exclusion rather than a bug.

- placeholder: `crates/<crate>/changelog.d/<issue>-<slug>.md`
- interpolation: `docs/$CRATE/README.md`
- brace template: `crates/{trusty-common,trusty-search}/Cargo.toml`
- glob: `crates/*/Cargo.toml` and `docs/**/*.md`
- single-char glob: `scripts/check_?.sh`
- ASCII elision: `crates/trusty-search/src/core/...`
- Unicode elision: `crates/trusty-search/src/core/…`
- Rust module path: `crates/tc-services::cto_db`
- alternation: `docs/adr/0044|0048`
- crate-relative `src/…` with no crate to resolve against: `src/mcp/tools.rs`

The last one is the structural rule, not a character rule: this page lives under
`docs/`, so no ancestor directory holds a `Cargo.toml` and there is no single
crate the token could mean. The `crate-relative/` fixture pins the other half —
inside a crate, the same shape of token IS checked.
