# Generated Documentation Regions

Some facts a crate README states are already known to the code: how many MCP
tools the server registers, what they are called, what arguments they take.
Maintained by hand, those facts drift. A September 2026 sweep found 13 false
claims across six crates, and the PR that hand-fixed them added a fourteenth —
it cited a function `tool_definitions` in `trusty-search`, where the real symbol
is `tool_descriptors`.

Those regions are now generated from the code. This page is the reference for
how that works and how to extend it.

## Marker syntax

A generated region is delimited by two HTML comments carrying the same id:

```markdown
  <!-- BEGIN GENERATED: mcp-tools -->
  The MCP server registers **21 tools**. Authoritative source: …

  | Tool | Arguments | Summary |
  |---|---|---|
  | `chat` | `index_id`, `message?` | Ask a natural-language question … |
  <!-- END GENERATED: mcp-tools -->
```

(indented here only so this page's own example is not mistaken for a real
region by `scripts/check_generated_regions.sh` — real markers start at column
zero.)

Everything between the markers is owned by the generator. Everything outside
them is hand-written and untouched: prose, taglines, rationale, architecture
diagrams, install instructions, the per-tool explainers that follow the table.

**Never hand-edit inside the markers.** A change there is reverted by the next
regeneration, and the gate fails in the meantime.

## Where the generator lives

The generator is a **test**, not a binary. The descriptor functions build their
payload with `serde_json::json!` macros, so parsing the Rust source is fragile
and executing the function is exact. A test can call it directly in-crate, needs
no new CI job, and runs inside the existing workspace test run.

| Piece | Location |
|---|---|
| Shared machinery | `crates/trusty-common/src/docgen.rs`, behind the `docgen` feature |
| trusty-search | `crates/trusty-search/tests/generated_docs.rs` → `README.md`, `CLAUDE.md` |
| trusty-memory | `crates/trusty-memory/tests/generated_docs.rs` → `README.md` |
| trusty-analyze | `crates/trusty-analyze/tests/generated_docs.rs` → `README.md`, `CLAUDE.md` |
| Orphan-marker guard | `scripts/check_generated_regions.sh` |

`docgen` is test-facing: the three crates enable it in `[dev-dependencies]`, not
`[dependencies]`, so no production build compiles it.

**One render call feeds every target file.** `README.md` and `CLAUDE.md`
previously carried the same table twice, which is why each wrong entry had to be
fixed twice. They now receive the same rendered string.

## Regenerating

```bash
UPDATE_DOCS=1 cargo test -p <crate> --test generated_docs
```

With `UPDATE_DOCS` unset, the test compares and fails on drift. Set to anything
other than empty or `0`, it rewrites the region in place and passes. The failure
message names the file and prints this exact command.

## Determinism

Rows are sorted by tool name inside `render_tool_section`, so no ordering
choice made by a caller — or by `serde_json`'s map implementation — can reach a
committed file. The `Arguments` column lists the schema's `required` names in
declaration order (authored, therefore stable) followed by every remaining
property sorted alphabetically. Duplicate tool names panic rather than render a
silently lossy table.

## Which function is the oracle

Two of the three crates ship a second descriptor function, and each consumer
test proves the choice rather than assuming it:

- `trusty-search` — `tool_descriptors` is the oracle.
  `tool_descriptors_pinned(Some(id))` is a schema-level transform of the same
  list: it moves `index_id` from required to optional and annotates that one
  property. It never changes the roster or any description.
  `tool_descriptors_pinned(None)` is byte-identical to the oracle, and the test
  asserts both facts.
- `trusty-memory` — `tool_definitions` is the oracle. `tool_definitions_with(true)`
  is the shape served under `--palace <name>` and only drops `palace` from each
  `required` array. The README documents the default, where `palace` is required.
- `trusty-analyze` — see below.

## Feature-dependent surfaces

`trusty-analyze` serves 19 tools by default and 22 with `--features review`. A
section stating one number is false under the other configuration, so the
generated table **states the composition** instead: an `Available` column marks
each row `always` or `` `--features review` ``, and the count sentence gives both
numbers.

Making that true required moving `review_tool_descriptors()` out of the
`#[cfg(feature = "review")]` module into `mcp::descriptors`, where it compiles
in every build. The dispatch handler stays gated; only the descriptor data
moved. Without that, the documented review rows would be unverifiable in the
default build — and CI never builds this crate with `--features review`, so they
would have gone unverified everywhere.

`section_is_correct_for_this_build_configuration` asserts the claim against
whichever build is running: a default run proves the 19, a `--features review`
run proves the 22.

## Coverage is opt-in — state plainly what that means

**A crate with no markers is not checked by anything here, silently.** Nothing
scans for crates that *ought* to have a generated region. Adding a new crate to
the workspace, or a new MCP tool table to an existing crate's README, does not
automatically bring it under the gate.

What *is* mechanically enforced:

- A file that **has** markers is checked, on every `cargo test -p <crate>`.
- A file that **loses** its markers fails with `MissingMarker` rather than
  passing vacuously — losing the markers is an error, not a skip.
- `scripts/check_generated_regions.sh` fails when a tracked markdown file
  carries markers but the owning crate has no `tests/generated_docs.rs`, and
  when a `BEGIN` has no matching `END`. It runs in CI as a step of the
  `line-cap` workflow.

The remaining hole is deliberate: deciding which facts in which crates deserve
generation is an editorial judgement, not something a script can infer.

## Extending it to another crate

1. Add `trusty-common = { …, features = ["docgen"] }` to the crate's
   `[dev-dependencies]`.
2. Copy `crates/trusty-analyze/tests/generated_docs.rs` and point it at your
   descriptor function and target files. Cite the function through
   `descriptor_source!(path::to::fn)` — the macro coerces the path to a
   zero-argument function pointer, so a renamed or nonexistent symbol is a
   compile error instead of a false sentence in a README.
3. Put the markers in the target files with any placeholder body.
4. Run `UPDATE_DOCS=1 cargo test -p <crate> --test generated_docs`.
5. Run `bash scripts/check_generated_regions.sh`.

## What is not generated

Only mechanically derivable facts: tool names, counts, arguments, and the first
sentence of each tool's own description. Prose, taglines, rationale,
architecture diagrams, install instructions, and per-tool explainers stay
hand-written outside the markers.

A poor generated summary means the tool's `description` in the source starts
with a poor first sentence. Fix it there — the README follows.
