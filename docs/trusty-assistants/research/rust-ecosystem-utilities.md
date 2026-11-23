# Rust Ecosystem Utilities Research

**Date**: 2026-04-23
**Scope**: Atomic file writing, Markdown processing, YAML, format translation, code formatting

---

## 1. Atomic File Writing

### Crates Compared

| Crate | Version | Downloads | Last Updated |
|---|---|---|---|
| `tempfile` | 3.27.0 | 535M | 2026-03-11 |
| `atomicwrites` | 0.4.4 | 14.8M | 2024-09-19 |

### Recommendation: `tempfile` with `NamedTempFile::persist()`

`tempfile` is the production standard. Pattern:

```rust
use tempfile::NamedTempFile;
use std::io::Write;

let mut tmp = NamedTempFile::new_in(target_dir)?;
tmp.write_all(content)?;
tmp.persist(target_path)?;  // atomic rename on same filesystem
```

**Why `tempfile` over alternatives:**
- `NamedTempFile::persist()` uses `fs::rename` under the hood — atomic on POSIX if same filesystem, atomic on Windows via `MoveFileEx`
- Creates temp file in same directory as target (critical: ensures same filesystem for atomic rename)
- 535M downloads vs `atomicwrites` 14.8M — `tempfile` is universal infrastructure
- `atomicwrites` (0.4.4, last updated Sep 2024) adds little value over `tempfile` + `persist()`
- Manual `fs::rename` is equivalent but `tempfile` handles cleanup on panic/error

**Key caveat**: temp file must be on same filesystem as target. Use `NamedTempFile::new_in(parent_dir_of_target)`, not the default `/tmp`.

---

## 2. Markdown Processing

### Crates Compared

| Crate | Version | Downloads | Last Updated | Standard |
|---|---|---|---|---|
| `pulldown-cmark` | 0.13.3 | 86M | 2026-03-22 | CommonMark |
| `comrak` | 0.52.0 | 4.6M | 2026-04-04 | CommonMark + GFM |
| `markdown` | 1.0.0 | 6.1M | 2025-04-23 | CommonMark/mdast |

### Recommendation by Use Case

**Markdown → HTML**: `comrak` v0.52.0
- Full CommonMark + GitHub Flavored Markdown extensions (tables, strikethrough, autolinks, task lists)
- Spec-compliant, actively maintained (Apr 2026), powers GitHub's own rendering
- Simple API: `comrak::markdown_to_html(input, &Options::default())`
- Use `pulldown-cmark` if you need a streaming/event-based API or the binary size is a concern

**Markdown validation / linting**: `pulldown-cmark` v0.13.3
- Event-based parser exposes parse errors and structure — good for walking the AST to validate
- 86M downloads vs comrak's 4.6M — more widely battle-tested
- Lower-level gives more control for custom validation passes

**Structure extraction (AST)**: `markdown` v1.0.0 or `comrak`
- `markdown` crate produces an `mdast` (Markdown AST) mirroring the unified/remark ecosystem — good if you need typed node traversal
- `comrak` also provides an AST via `parse_document()` — full node tree with source positions

**Bottom line**: `comrak` for most use cases (HTML output, structure extraction). `pulldown-cmark` when you need streaming events or minimal dependencies.

---

## 3. YAML in Rust

### Crates Compared

| Crate | Version | Downloads | Status |
|---|---|---|---|
| `serde_yaml` | 0.9.34+deprecated | 261M | **Deprecated** (Mar 2024) |
| `yaml-rust2` | 0.11.0 | 34.4M | Maintained |
| `serde-yaml-ng` | 0.10.0 | 3.2M | Maintained fork |
| `serde_yml` | 0.0.12 | 13.1M | Active |

### Recommendation: `serde_yml` or `serde-yaml-ng`

`serde_yaml` was officially deprecated by its author in March 2024 with the version string `0.9.34+deprecated`. Do not use it for new projects.

**Community situation (fragmented):**
- `serde_yml` (0.0.12) — most-downloaded maintained fork (~13M downloads), drop-in replacement, API-compatible with `serde_yaml`
- `serde-yaml-ng` (0.10.0) — conservative fork focused on correctness
- `yaml-rust2` (0.11.0) — lower-level YAML parser (not serde-integrated by itself), used as backend by the forks

**Recommendation**: Use `serde_yml` for the path of least resistance (near-identical API to `serde_yaml`). If correctness over compatibility is the priority, consider `serde-yaml-ng`. Either is a valid choice — the community has not converged on a single winner.

---

## 4. Format Translation Crates

### Pandoc-equivalent in Pure Rust

No mature pure-Rust pandoc equivalent exists. Options:

- **`pandoc` crate** (0.8.11, 258K downloads, last updated Nov 2023) — thin wrapper that shells out to the `pandoc` binary. Requires pandoc installed separately. Not pure Rust.
- There is no `pulldown-cmark` → DOCX or → LaTeX pipeline in stable Rust yet

For serious document conversion needs, the practical answer is: invoke `pandoc` as a subprocess or use a WASM build.

### JSON ↔ TOML Conversion

No dedicated conversion crate is necessary. The standard pattern:

```rust
// JSON → TOML
let value: serde_json::Value = serde_json::from_str(json_str)?;
let toml_string = toml::to_string(&value)?;

// TOML → JSON
let value: toml::Value = toml::from_str(toml_str)?;
let json_string = serde_json::to_string(&value)?;
```

Both `serde_json` (1.0.149) and `toml` (1.1.2) implement `serde::Serialize/Deserialize` for their `Value` types, so round-tripping through serde is idiomatic.

`toml_edit` (0.25.11, 522M downloads) is the preferred crate when you need to read/modify TOML while preserving formatting and comments (e.g., editing `Cargo.toml` programmatically).

### Universal Document Crates

Nothing production-grade. The ecosystem has purpose-built crates (Markdown, TOML, JSON, CSV via `csv`) but no universal document model.

---

## 5. Code Formatting

### Options

| Option | Status | Use Case |
|---|---|---|
| `rustfmt` subprocess | Works | Simple, universal, stable |
| `rustfmt-nightly` crate | Abandoned (2020) | Do not use |
| `prettyplease` | 0.2.37, 312M downloads | proc-macro / codegen contexts |

### Recommendation: `prettyplease` for programmatic use

**`rustfmt` subprocess**: Works well for formatting files on disk. Call via `std::process::Command`. Cannot be linked as a library on stable Rust — the `rustfmt` binary API is not stable.

**`rustfmt-nightly` crate**: Last updated 2020, 460K downloads — effectively dead. Do not use.

**`prettyplease`** (0.2.37, 312M downloads, updated Aug 2025): The correct answer for programmatic formatting in proc-macros and code generation tools. Takes a `syn::File` (token stream after parsing) and returns formatted source. Used by `prost`, `tonic`, and most serious code-gen crates:

```rust
use prettyplease;
use syn;

let syntax_tree: syn::File = syn::parse_str(&generated_code)?;
let formatted = prettyplease::unparse(&syntax_tree);
```

**When to use each**:
- Formatting generated source in a build script or proc-macro → `prettyplease`
- Formatting an arbitrary `.rs` file on disk → shell out to `rustfmt`
- Formatting arbitrary token streams without a full parse → not currently possible without nightly

---

## Summary Table

| Use Case | Recommended Crate | Version |
|---|---|---|
| Atomic file write | `tempfile` (`NamedTempFile::persist`) | 3.27.0 |
| Markdown → HTML | `comrak` | 0.52.0 |
| Markdown parsing/validation | `pulldown-cmark` | 0.13.3 |
| Markdown AST traversal | `comrak` or `markdown` | 0.52.0 / 1.0.0 |
| YAML serialization | `serde_yml` | 0.0.12 |
| JSON ↔ TOML conversion | `serde_json` + `toml` (serde Value) | 1.0.149 / 1.1.2 |
| Preserve-format TOML editing | `toml_edit` | 0.25.11 |
| Programmatic code formatting | `prettyplease` | 0.2.37 |
| Format files on disk | `rustfmt` subprocess | N/A |
| Pandoc equivalent | None (pure Rust) — shell out to `pandoc` | — |
