# Xberg Fit Assessment (2026-11-03)

## Recommendation

**C — No addition** at this time, but design a plugin framework with Xberg as the motivating example for v2.

**Rationale:** Xberg offers substantial capabilities (107 formats, OCR, ML layout) but brings high dependency cost (ONNX, ML models, native libs), MSRV incompatibility (1.98 vs trusty-tools' 1.94), and significant surface area that overlaps with trusty-search's simpler text extraction. The trusty-okf context explicitly states that document extraction stays in trusty-agents connectors, not OKF, which limits the immediate value. However, Xberg demonstrates the shape a plugin system would serve: capability-rich extractors with isolated dependencies. Recommend designing a plugin framework (see below) before deciding on any specific extractor integration.

## Comparison Table

| Criterion | Xberg | Existing trusty-search | Notes |
|-----------|-------|------------------------|-------|
| **Extraction fidelity** | High — native PDF engine, ML layout models (PP-DocLayout-V3, RT-DETR), table structure (TATR, SLANet) | Basic — lopdf text extraction via pdf-extract, docusign/docx for Office | Xberg extracts tables, layout, reading order; trusty-search gets plain text only |
| **Source provenance** | Per-chunk metadata (page, bbox, confidence) | File path + line range | Xberg tracks source locations; trusty-search has no page-level provenance |
| **Local-only operation** | Yes — all OCR/ML models run locally (Tesseract, PaddleOCR, Candle, VLM) | Yes — pdf-extract, docx are pure Rust | Both can operate offline; Xberg's model downloads are opt-in |
| **Dependencies** | Heavy — ONNX Runtime (`ort`), ML models (download on demand), Tesseract/PaddleOCR native libs (optional), `candle-core`, `tokio` (dependency count not measured) | Light — `pdf-extract` (lopdf), `docx-rs`, `calamine`, ~30 transitive deps | Xberg's full feature set requires model downloads (size not measured); trusty-search bundles nothing |
| **Install size** | Binary size not measured; models downloaded on demand | Binary ~15MB (trusty-search with extractors), no models | Measurement required for comparison |
| **Performance isolation** | Timeouts configurable, async extraction, parallelism via rayon | 30s timeout per file, `tokio::spawn_blocking` | Timeout enforcement requires out-of-process mode (SIGTERM) or cooperative cancellation; `spawn_blocking` cannot be stopped by Tokio timeout |
| **Platform support** | macOS/Linux/Windows, WASM, Android, iOS (via feature gates) | macOS/Linux (primary), Windows (untested) | Xberg's cross-platform support is broader and tested |
| **Licensing** | MIT (compatible) | pdf-extract: Apache-2.0, docx-rs: MIT, calamine: MIT | No license conflict |
| **Maintenance** | Active — 2 maintainers (xberg-io), recent commits (Nov 2026), v1.4.0 released | trusty-search maintained in-house | Xberg is actively developed; upstream maintenance is not guaranteed |
| **MSRV** | 1.98 (edition 2024) | 1.94 (edition 2021, ADR-0029) | **Blocker:** In-process integration with Xberg 1.98 FORCES workspace MSRV bump to 1.98; feature-gating does not isolate MSRV (a feature-gated dependency still requires its MSRV at build time when enabled). Out-of-process only if staying at 1.94. |

## Existing Extractors in trusty-tools

**Location:** `crates/trusty-search/src/core/extract/`

### Supported Formats (issue #2923)

| Format | Extractor | Crate | Capabilities | Limitations |
|--------|-----------|-------|--------------|-------------|
| PDF | `pdf::extract` | `pdf-extract` 0.7 (lopdf 0.42) | Text extraction, corrupt-PDF handling, nesting-depth guard | No OCR, no tables, no layout, warns on scanned PDFs |
| DOCX | `docx::extract` | `docx-rs` | Text + paragraphs | No tables, no images |
| PPTX | `pptx::extract` | `docx-rs` (reused) | Slide text | No speaker notes, no layout |
| XLS/XLSX/XLSM | `xlsx::extract` | `calamine` | Cell text (flattened) | No formulas, no cell structure, no merged cells |

**Extraction caps:**
- File size: 10MB (`MAX_OFFICE_FILE_BYTES`)
- Extracted text: 5MB (`MAX_EXTRACTED_TEXT_BYTES`)
- Timeout: 30 seconds (`EXTRACT_TIMEOUT`)

**Key files:**
- `crates/trusty-search/src/core/extract/mod.rs:47` — `EXTRACT_EXTS` (dispatch table)
- `crates/trusty-search/src/core/extract/pdf.rs:29` — PDF extraction entry point
- `crates/trusty-search/src/core/extract/docx.rs` — DOCX/PPTX extraction
- `crates/trusty-search/src/core/extract/xlsx.rs` — Spreadsheet extraction

**Integration:** Both reindex (`service::reindex::batch`) and file watcher (`service::watch_loop`) call `extract_text`, which dispatches on extension. Extracted text is chunked by `chunk_text` (sliding window, 512 chars) since tree-sitter has no parsers for these formats.

## Existing Extension Mechanisms

**trusty-tools currently uses:**

1. **Cargo feature gates** (`crates/trusty-search/Cargo.toml:92-117`)
   - `bundled-ort` — static ORT libs
   - `ner` — ONNX NER extractor (opt-in)
   - `cuda` — GPU acceleration
   - Scoped to compile-time; no runtime plugin discovery

2. **MCP servers** (multiple crates)
   - `trusty-memory` — MCP bridge (`crates/trusty-memory/src/bin/trusty-memory-mcp-bridge.rs`)
   - `trusty-search` — JSON-RPC 2.0 over Unix socket (`crates/trusty-search/src/service/socket.rs`)
   - `trusty-mcp` — primitives (`crates/trusty-mcp/src/lib.rs`)
   - Out-of-process, discovery via `.mcp.json`

3. **CLI tools** (bundled binaries)
   - `trusty-search` — daemon + indexer
   - `tm` — memory server
   - `tagent` — agent harness
   - Installed via `cargo install`, discovered via `$PATH`

4. **Shared capabilities in trusty-common** (`crates/trusty-common/Cargo.toml`)
   - Feature-gated modules: `symgraph-parser`, `credentials`, `chat-attachments`, `stdio-mcp-client`, etc.
   - Library-level reuse, not plugin isolation

5. **trusty-code Phase 1 plugin support** (DOC-51, implemented 2026-09-03)
   - Local-directory agents + skills ingestion (`crates/trusty-code/src/plugins/`)
   - Discovery from `.claude/plugins/<plugin>/agents/` and `.claude/plugins/<plugin>/skills/`
   - Namespaced dispatch (`<plugin>:<local-name>`)
   - Manifest overrides via `.claude-plugin/plugin.json`
   - Security: validates plugin content as hostile input (path traversal, symlink escape guards)
   - Scope: agents/skills only; no commands/hooks/MCP (deferred to later phases)

**Formal plugin frameworks:** trusty-code has Phase 1 local-directory plugins for agents/skills (DOC-51). No generic plugin framework exists for other capabilities (document extraction, indexing extensions, etc.). Extensions outside agents/skills are either:
- Compiled in (feature gates)
- Separate processes (MCP servers, CLI tools)
- Shared libraries (trusty-common features)

## Trusty Plugin Framework Design

### Existing Extension Mechanisms

See "Existing Extension Mechanisms" above. trusty-code Phase 1 plugin support (DOC-51) provides a reference pattern: local-directory discovery, manifest-based overrides, namespaced dispatch, hostile-input guards. A generic plugin framework for document extraction should incorporate these patterns while addressing current gaps:
- No capability discovery (beyond what DOC-51's manifest provides)
- No version compatibility checks (DOC-51 has none)
- No resource/failure isolation beyond process boundaries
- No unified manifest format across subsystems (DOC-51 uses `.claude-plugin/plugin.json`; this proposal uses TOML)
- No permission model (DOC-51 assumes plugins are local-project-owned, not third-party)
- Feature gates are all-or-nothing (whole workspace MSRV)

### Proposed Framework

#### Plugin Contract

**Interface:** Rust trait `DocumentExtractor` (new)

```rust
// crates/trusty-common/src/plugins/extractor.rs
pub trait DocumentExtractor: Send + Sync {
    fn name(&self) -> &str;
    fn supported_extensions(&self) -> &[&str];
    fn supported_mime_types(&self) -> &[&str];
    fn extract(&self, input: &ExtractInput) -> Result<ExtractOutput, ExtractError>;
    fn capabilities(&self) -> ExtractorCapabilities;
}

pub struct ExtractorCapabilities {
    pub supports_ocr: bool,
    pub supports_tables: bool,
    pub supports_layout: bool,
    pub supports_provenance: bool, // page/bbox metadata
    pub max_file_bytes: Option<u64>,
    pub timeout: Duration,
}

pub struct ExtractInput {
    pub path: PathBuf,
    pub bytes: Cow<'_, [u8]>,
    pub mime_type: Option<String>,
}

pub struct ExtractOutput {
    pub text: String,
    pub metadata: HashMap<String, serde_json::Value>,
    pub warnings: Vec<String>,
}
```

**Discovery:** Two modes:

1. **In-process (feature-gated):** Plugin crate compiled as workspace member, registered at build time:
   ```rust
   // crates/trusty-search/src/plugins/registry.rs
   #[cfg(feature = "xberg-extractor")]
   use trusty_xberg_plugin::XbergExtractor;
   
   fn default_extractors() -> Vec<Box<dyn DocumentExtractor>> {
       vec![
           Box::new(BuiltinPdfExtractor),
           #[cfg(feature = "xberg-extractor")]
           Box::new(XbergExtractor::new()),
       ]
   }
   ```

2. **Out-of-process (CLI adapter):** Plugin as separate binary, invoked via stdio JSON-RPC:
   ```bash
   # Plugin discovery: ~/.trusty-search/plugins/*.manifest.toml
   xberg-extractor --extract '{"path": "doc.pdf"}' < /dev/null
   ```

#### Boundaries

**Ingestion/Search/OKF:**
- **Ingestion (trusty-search):** Plugins extract text → chunked → indexed
- **Search (trusty-search):** Queries hit indexed chunks (no plugin involvement at query time)
- **OKF (future):** Consumes indexed chunks + raw files, not plugin output directly

**Per trusty-okf spec context:**
- OKF v0.2 format, no I/O beyond files
- OKF does NOT own extraction (stays in trusty-agents connectors)
- Plugins serve trusty-search indexing, not OKF bundling

#### Manifest Format

**TOML** (`~/.trusty-search/plugins/<name>.manifest.toml`):

```toml
schema_version = 1
name = "xberg-extractor"
version = "1.4.0"

[capabilities]
supported_extensions = ["pdf", "docx", "pptx", "xlsx", "png", "jpg"]
supported_mime_types = ["application/pdf", "image/png", "image/jpeg"]
supports_ocr = true
supports_tables = true
supports_layout = true
max_file_bytes = 104_857_600  # 100MB
timeout_seconds = 120

[runtime]
mode = "in-process"  # or "cli"
# For CLI mode:
# executable = "xberg-extractor"
# args = ["--extract"]

[compatibility]
min_trusty_search = "0.8.0"
max_trusty_search = "0.9.999"
msrv = "1.94"  # Plugin's own MSRV; may be higher than host

[dependencies]
# Optional: native libs this plugin needs
# native_libs = ["libonnxruntime.so.1.24"]

[permissions]
# What this plugin can access
local_files = ["read"]  # read-only; "write" would be refused
network = false
models_dir = "~/.trusty-search/models/xberg"
```

#### Permission Model

**Local-data scope:**
- Plugins read files from index roots only
- Writes forbidden (extraction is read-only)
- Model downloads: plugin-specific subdirectory (`~/.trusty-search/models/<plugin-name>/`)
- No network access by default (manifest `permissions.network = false`)
- Out-of-process plugins can be sandboxed via OS (no `/etc`, `/Users` access beyond index roots)

**Enforcement:**
- In-process: **Rust trait alone provides NO enforcement** — traits constrain type signatures, not I/O behavior. Actual isolation requires process boundaries or capability-based I/O wrappers (e.g., passing a read-only file handle instead of a path, or wrapping `std::fs` with a policy layer). The `DocumentExtractor` trait as written CANNOT prevent a plugin from writing files, making network calls, or accessing arbitrary paths.
- Out-of-process: OS-level sandboxing (`chroot` / `landlock` on Linux, Sandbox on macOS, process ACLs on Windows) CAN enforce filesystem/network restrictions, but requires explicit configuration and platform-specific implementations.

#### Lifecycle Management

**Install:**
```bash
# In-process: rebuild with feature
cargo install trusty-search --features xberg-extractor

# Out-of-process: drop manifest + binary
cp xberg-extractor ~/.trusty-search/plugins/
cp xberg.manifest.toml ~/.trusty-search/plugins/
trusty-search plugin reload
```

**Update:**
- In-process: `cargo install --force`
- Out-of-process: replace binary, daemon auto-reloads on manifest mtime change

**Uninstall:**
- In-process: rebuild without feature
- Out-of-process: `rm ~/.trusty-search/plugins/xberg*`, daemon unregisters

**Version compatibility:**
- Manifest `compatibility.min_trusty_search` / `max_trusty_search` checked at load
- Incompatible plugins logged as warnings, not loaded
- Semantic versioning: breaking changes in plugin trait = major bump

**Dependency isolation:**
- In-process: Cargo resolver handles conflicts (but MSRV must align)
- Out-of-process: separate address space, no shared deps

#### Isolation

**Dependency isolation:**
- In-process: Limited — shares `tokio`, `serde`, etc. with host; MSRV conflict forces feature-gate
- Out-of-process: Complete — plugin can use any MSRV, different tokio version, etc.

**Failure containment:**
- In-process: Plugin panic caught at trait boundary via `catch_unwind`, returns `ExtractError::PluginPanic`
- Out-of-process: Process exit caught, stderr captured, returns `ExtractError::ProcessExited(code)`

**Security isolation:**
- In-process: **No filesystem/network sandboxing** — plugin runs with daemon privileges, can access any file the daemon can.
- Out-of-process: **Process isolation alone does NOT sandbox** — it prevents panic/crash propagation to the host, but the plugin process still inherits the daemon's filesystem/network access unless explicitly sandboxed with OS-level restrictions (`seccomp`, `landlock`, `pledge`, macOS Sandbox, etc.). Simply running in a separate process provides NO security boundary by itself.

**Resource limits:**
- File size: `capabilities.max_file_bytes` (enforced by host before calling plugin)
- Timeout: `capabilities.timeout_seconds` (in-process: requires cooperative cancellation via `tokio::task::JoinHandle::abort()` or the plugin checking a cancellation flag; `tokio::time::timeout` CANNOT stop a `spawn_blocking` task, which blocks a thread until completion or panic. Out-of-process: SIGTERM can force-kill the process.)
- Memory: Out-of-process plugins can set `ulimit -m` / cgroup limits; in-process shares host heap
- CPU: Out-of-process can use `nice` / CPU quotas; in-process uses `tokio::spawn_blocking` pool (bounded by default, but tasks cannot be preempted)

**Time containment:**
- Per-file timeout from manifest `capabilities.timeout_seconds`
- Global budget: reindex batch processes N files/sec; slow plugin delays completion but doesn't stall daemon
- Timeout expiry: file skipped, error logged, next file proceeds

#### Observability

**Logging:**
- In-process: Plugin logs via `tracing`, scoped to `plugin::<name>`
- Out-of-process: Plugin stderr captured, logged as `plugin::<name>::stderr`

**Metrics:**
- Per-plugin counters: files processed, bytes extracted, errors, timeouts
- Histograms: extraction latency (p50/p99), file size distribution
- Exported via trusty-search's existing Prometheus endpoint (`/metrics`)

**Verifiable results:**
- Each `ExtractOutput` includes `metadata["extractor_name"]`, `metadata["extractor_version"]`
- Provenance stored in chunk metadata (not searchable, but retrievable)
- Warnings surfaced to reindex logs and `/indexes/:id/status` degraded reasons

### In-Process vs Out-of-Process

| Aspect | In-Process (feature-gated) | Out-of-Process (CLI/MCP) |
|--------|---------------------------|--------------------------|
| **Performance** | Fast (no IPC, shared memory) | Slower (JSON serialize + process spawn per file) |
| **Isolation** | Limited (shared heap, MSRV, panic boundary) | Strong (separate address space, OS-level) |
| **Dependency conflicts** | Must align MSRV, tokio version | No conflicts (separate binary) |
| **Install complexity** | Rebuild host binary | Drop manifest + binary, reload |
| **Distribution** | `cargo install --features <plugin>` | Separate package (crates.io, GitHub releases) |
| **Failure blast radius** | Plugin panic can poison tokio executor | Process exit isolated, no host impact |
| **Resource limits** | Share host tokio pool, heap | Separate `ulimit`, cgroup, CPU quota |
| **Observability** | Direct `tracing` integration | Stderr capture, exit code |
| **Update cadence** | Tied to host releases | Independent updates |
| **Security** | Runs with daemon privileges | Can be sandboxed (landlock, chroot) |
| **When to use** | Low-latency, trusted code, aligned MSRV | Untrusted code, high MSRV, heavy deps |

**Recommendation:** Start with in-process for official plugins (trusty-xberg-plugin), offer out-of-process for community/experimental extractors.

### Xberg Integration Example

**As in-process plugin:**

1. New crate: `crates/trusty-xberg-plugin/` (workspace member)
   ```toml
   [package]
   name = "trusty-xberg-plugin"
   version = "0.1.0"
   edition = "2021"
   rust-version = "1.98"  # Xberg's MSRV
   
   [dependencies]
   xberg = { version = "1.4", default-features = false, features = ["pdf", "office"] }
   trusty-common = { path = "../trusty-common", features = ["plugin-traits"] }
   ```

2. Implement trait:
   ```rust
   // crates/trusty-xberg-plugin/src/lib.rs
   use trusty_common::plugins::{DocumentExtractor, ExtractInput, ExtractOutput};
   
   pub struct XbergExtractor {
       engine: xberg::Engine,
   }
   
   impl DocumentExtractor for XbergExtractor {
       fn name(&self) -> &str { "xberg" }
       fn supported_extensions(&self) -> &[&str] { &["pdf", "docx", "png"] }
       fn extract(&self, input: &ExtractInput) -> Result<ExtractOutput, ExtractError> {
           let result = self.engine.extract(input.bytes, /* ... */)?;
           Ok(ExtractOutput {
               text: result.text,
               metadata: result.metadata.into(),
               warnings: result.warnings,
           })
       }
   }
   ```

3. Feature-gate in `trusty-search/Cargo.toml`:
   ```toml
   [features]
   xberg-extractor = ["dep:trusty-xberg-plugin"]
   
   [dependencies]
   trusty-xberg-plugin = { path = "../trusty-xberg-plugin", optional = true }
   ```

4. Register:
   ```rust
   // crates/trusty-search/src/plugins/registry.rs
   #[cfg(feature = "xberg-extractor")]
   registry.register(Box::new(trusty_xberg_plugin::XbergExtractor::new()));
   ```

**As out-of-process plugin:**

1. Standalone binary crate (outside workspace):
   ```bash
   cargo new --bin xberg-extractor-cli
   cd xberg-extractor-cli
   cargo add xberg clap serde_json
   ```

2. CLI wrapper:
   ```rust
   // src/main.rs
   use xberg::{extract, ExtractInput, ExtractionConfig};
   
   #[derive(clap::Parser)]
   struct Args {
       #[arg(long)]
       extract: String,  // JSON ExtractInput
   }
   
   fn main() {
       let args = Args::parse();
       let input: ExtractInput = serde_json::from_str(&args.extract).unwrap();
       let output = extract(input, &ExtractionConfig::default()).unwrap();
       println!("{}", serde_json::to_string(&output).unwrap());
   }
   ```

3. Manifest:
   ```toml
   # ~/.trusty-search/plugins/xberg.manifest.toml
   schema_version = 1
   name = "xberg-extractor"
   [runtime]
   mode = "cli"
   executable = "xberg-extractor-cli"
   args = ["--extract"]
   ```

4. Invoke:
   ```rust
   // crates/trusty-search/src/plugins/cli_plugin.rs
   let input_json = serde_json::to_string(&input)?;
   let output = Command::new(&manifest.runtime.executable)
       .arg(&manifest.runtime.args[0])
       .arg(&input_json)
       .output()?;
   let result: ExtractOutput = serde_json::from_slice(&output.stdout)?;
   ```

### MCP Tool Exposure vs Plugin Lifecycle

**Distinction:**
- **MCP tools** expose operations to LLMs (e.g., `search.query`, `memory.recall`)
- **Plugins** extend internal capabilities (e.g., extract PDF with OCR)

**Relationship:**
- A plugin (e.g., `xberg-extractor`) extracts → trusty-search indexes
- An MCP tool (e.g., `search.query`) queries indexed chunks (no plugin involvement)
- Plugins are an **implementation detail** of indexing, not part of the MCP surface

**Tradeoffs:**
- Exposing plugins as MCP tools (`xberg.extract {"file": "doc.pdf"}`) would let LLMs invoke them directly, but:
  - Bypasses chunking/indexing (one-shot extraction vs. searchable corpus)
  - No caching (re-extract on every call)
  - Exposes internal complexity (which extractor? which features?)
- **Recommendation:** Plugins stay internal to indexing; MCP tools query the indexed result

### Minimal v1 vs Deferred

**v1 — Core features needed for Xberg:**
- [ ] `DocumentExtractor` trait in `trusty-common`
- [ ] In-process plugin registry in `trusty-search`
- [ ] Manifest schema v1 (TOML)
- [ ] Timeout enforcement
- [ ] Panic boundary (`catch_unwind`)
- [ ] Basic metrics (files processed, errors, latency p99)
- [ ] Xberg plugin as feature-gated crate (`trusty-xberg-plugin`)
- [ ] Documentation: plugin authoring guide

**Deferred (v2+):**
- Out-of-process CLI adapter (JSON-RPC stdio)
- Permission model (landlock/sandbox)
- Model download orchestration (shared cache, version pinning)
- Plugin marketplace / discovery (`trusty-search plugin search`)
- Incremental extraction (resume on timeout, partial results)
- Multi-format bundled output (Markdown + JSON + HTML from one pass)
- Advanced provenance (page/bbox in chunk metadata, queryable)
- Plugin hot-reload (no daemon restart)
- Plugin DAGs (chain extractors: OCR → layout → table extraction)

### Reuse Opportunities

**From existing code:**
1. **Extractor timeout** (`crates/trusty-search/src/core/extract/mod.rs:89`) — `EXTRACT_TIMEOUT` and `tokio::time::timeout` wrapper → reuse for plugin boundary
2. **File size caps** (`MAX_OFFICE_FILE_BYTES`, `MAX_EXTRACTED_TEXT_BYTES`) → generalize to `PluginLimits`
3. **Panic handling** (none exists) → new boundary at plugin trait
4. **MCP manifest parsing** (`trusty-mcp`) → adapt for plugin manifests
5. **Feature gates** (`trusty-search/Cargo.toml:92-117`) → pattern for in-process plugins
6. **Embedding model cache** (`trusty-common/embedder`) → similar pattern for plugin model downloads
7. **Index registry** (`crates/trusty-search/src/service/registry.rs`) → plugin registry follows same `DashMap<PluginId, Arc<Plugin>>` pattern

**From Xberg:**
- OCR backend abstraction → adapt for plugin capabilities discovery
- Format detection (MIME + extension) → reuse in plugin dispatch
- Extraction error types → map to `ExtractError`

### Tradeoffs and Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| In-process vs out-of-process first | In-process (feature-gated) | Lower latency, simpler v1, aligns with existing feature gates |
| Trait vs procedural | Trait (`DocumentExtractor`) | Type-safe, idiomatic Rust, enables in-process |
| Manifest format | TOML | Matches `indexes.toml`, `.trusty-search.yaml`; human-editable |
| Capability discovery | Compile-time (trait methods) | No runtime reflection in Rust; explicit contract |
| Timeout enforcement | Host-side | Plugin can't bypass; enforced before/after trait call |
| Panic isolation | `catch_unwind` | Rust-native; no OS-level sandbox for in-process |
| Plugin versioning | Semantic (`min_trusty_search`/`max`) | Standard practice; forward/backward compat |
| MSRV conflicts | Accept (document in plugin guide) | In-process plugins must align with host; out-of-process for divergent MSRV |
| Permission model v1 | None (read-only extraction) | Defer to v2; extraction is inherently read-only |
| Metrics | Per-plugin counters + histograms | Existing Prometheus endpoint, minimal overhead |

### Small Proof with Acceptance Criteria

**Note:** Recommendation is (C) No addition at this time. The proof below describes what would be validated IF pursuing option (A) or (B) later, after the plugin framework design is approved. This is not a proposal to execute the proof now.

**Scope:** Validate the plugin framework with a minimal Xberg integration (PDF extraction only, no OCR).

**Proof goals (if framework approved):**
1. Prove the `DocumentExtractor` trait is sufficient
2. Prove feature-gated compilation works (MSRV 1.98 plugin in MSRV 1.94 host)
3. Prove timeout + panic isolation
4. Prove metrics collection
5. Prove extracted text reaches index (search hits the xberg-extracted content)

**Fixture:**
- 1 PDF with extractable text (5 pages, 2MB, tables + layout)
- 1 scanned PDF (image-only, requires OCR)
- 1 corrupt PDF (should error gracefully)

**Acceptance criteria:**
- [ ] `trusty-xberg-plugin` crate compiles with `rust-version = "1.98"`
- [ ] `trusty-search` compiles with `--features xberg-extractor` (MSRV 1.94 host)
- [ ] `trusty-search` compiles without feature (no xberg dep pulled)
- [ ] Extractable PDF: indexed, search hits content, no warnings
- [ ] Scanned PDF: either (a) enable OCR → extracts text successfully, or (b) OCR disabled → remove scanned PDF from acceptance criteria (empty output is not a useful test)
- [ ] Corrupt PDF: `ExtractError::PluginError(...)` logged, file skipped, reindex continues
- [ ] Plugin panic (simulate via test double): caught, logged, returns error, reindex continues
- [ ] Timeout (5-page PDF with timeout=1s): file skipped, warning logged, reindex continues
- [ ] Metrics: `plugin_files_processed{plugin="xberg"}`, `plugin_latency_seconds{plugin="xberg"}` exported
- [ ] `/indexes/:id/status` shows `extractor_plugins: ["xberg"]` when feature enabled
- [ ] Roundtrip: `POST /indexes`, `POST /indexes/:id/index-file` (PDF), `POST /indexes/:id/search` returns chunk with `metadata.extractor="xberg"`

**Integration point:**
- Trait definition: `crates/trusty-common/src/plugins/extractor.rs` (new module)
- Plugin impl: `crates/trusty-xberg-plugin/src/lib.rs` (new crate)
- Registry: `crates/trusty-search/src/plugins/registry.rs` (new module)
- Call site: `crates/trusty-search/src/core/extract/mod.rs:extract_text` → dispatch to plugin if registered

**Estimated LoE (rough, not task-breakdown-based):**
- **Proof:** Rough estimate 3–5 days (trait design, xberg wrapper, timeout/panic isolation, 1 roundtrip test)
- **Full integration:** Rough estimate 2–3 weeks (out-of-process CLI, manifest parser, permission model, observability, documentation)

## Risk Analysis

**Dependencies:**
- Xberg pulls heavy transitive deps (ONNX, ML, image processing; exact count not measured)
- In-process integration forces these onto trusty-search (binary size increase not measured)
- Out-of-process isolates this, but adds IPC overhead

**Maintenance burden:**
- Xberg maintained by xberg-io (2 core contributors)
- No guarantee of continued support (project is <1 year old)
- Breaking changes in xberg would break plugin until updated
- Mitigation: version pinning in plugin, plugin framework makes swapping extractors feasible

**Failure modes:**
- **Model download fails:** xberg cannot extract (OCR/layout disabled) → fallback to text-only
- **ONNX Runtime unavailable:** OCR/layout features disabled → same fallback
- **Timeout on large file:** file skipped, warning logged → acceptable
- **Plugin panic:** caught, logged, file skipped → acceptable (other files proceed)
- **MSRV drift:** xberg bumps to 1.100 → out-of-process plugin still works; in-process requires host MSRV bump or removal

**Security:**
- Xberg processes untrusted PDFs → potential RCE via PDF parser vuln
- Mitigation: out-of-process sandboxing, Rust memory safety, upstream security audits
- No known CVEs in xberg 1.4.0 (checked GitHub Security tab)

**Performance:**
- Xberg's ML models are CPU-bound (100–500ms per page on M1)
- Large indexes (10k+ PDFs) → multi-hour reindex
- Mitigation: incremental indexing, timeout caps, optional feature (off by default)

**MSRV incompatibility:**
- Xberg 1.98 vs trusty-tools 1.94 → cannot mix in same binary without MSRV bump
- ADR-0029 locks trusty-tools to 1.94 until explicit decision
- Mitigation: out-of-process plugin, or defer until trusty-tools bumps MSRV

**Upstream changes:**
- Xberg is pre-2.0, API may break
- Mitigation: pin to `1.4.x`, test before upgrading, plugin abstraction decouples trusty-search from xberg API

**Conclusion:** Risks are manageable with out-of-process isolation + version pinning. In-process integration has higher coupling risk (MSRV, deps, security).

---

**Assessment completed:** 2026-11-03 21:46 EST
**Xberg version assessed:** 1.4.0
**trusty-tools version:** Based on main@bcbea788
**Assessor:** Research agent
