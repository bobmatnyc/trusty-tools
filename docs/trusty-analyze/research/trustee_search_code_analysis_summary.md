# Trustee Search: Code Analysis and Runtime Profiling Implementation Summary

## Context

**Trustee Search** is a high-performance Rust-based code search and analysis tool. The target design combines:

- AST and syntax tree analysis
- Tree-sitter-style parsing
- Knowledge graphs
- MD5/text indexing
- Lightweight embeddings
- Overlapping search windows
- Chunked and highly indexed codebase representation

The existing Rust version already performs strong code analysis. The next direction is to explore embedding static and dynamic analysis capabilities into the tool, especially in a way that takes advantage of the already indexed, chunked, and graphed representation of the source code.

The scale target discussed was approximately:

- **15,000 files**
- **~1 million lines of Java code**
- **Indexed in under 10 minutes**

The tool currently or potentially supports these languages:

- Rust
- C
- C++
- Go
- TypeScript
- JavaScript
- Java
- Python was also discussed for runtime analysis patterns

---

## Core Direction

The recommended architecture is a **Rust orchestration engine** with a common internal graph/index model, plus language-specific static and runtime analysis adapters.

Rust should remain the central coordinator because it is well suited for:

- High-throughput indexing
- Parallel file walking
- Fast parsing pipelines
- Efficient graph construction
- Managing worker processes
- Running Dockerized analysis jobs
- Normalizing language-specific results into a common schema

The core idea is not to make Rust directly understand every language deeply. Instead, Trustee Search should:

1. Parse and index source code using a fast, uniform baseline such as Tree-sitter.
2. Build a language-neutral graph of files, symbols, functions, classes, imports, calls, and references.
3. Attach deeper semantic analysis from language-specific tools where available.
4. Optionally run dynamic/runtime analysis in isolated containers.
5. Map runtime observations back onto the static graph.

---

## Static Analysis Options by Language

### Universal Baseline: Tree-sitter

Tree-sitter is the best common foundation for multi-language static parsing.

It supports the major target languages discussed:

- Rust
- C
- C++
- Go
- TypeScript
- JavaScript
- Java
- Python

Recommended use:

- Use Tree-sitter as the default parser for all supported languages.
- Extract files, classes, functions, methods, imports, exports, calls, comments, and declarations.
- Assign stable symbol IDs and source ranges.
- Store parsed syntax entities as graph nodes.
- Store relationships as graph edges.

Example graph node types:

- Repository
- Package/module
- File
- Class
- Interface
- Function
- Method
- Field/property
- Import/export
- Call expression
- Test case
- Dependency

Example graph edge types:

- `CONTAINS`
- `IMPORTS`
- `EXPORTS`
- `CALLS`
- `IMPLEMENTS`
- `EXTENDS`
- `REFERENCES`
- `TESTS`
- `DEPENDS_ON`
- `GENERATED_FROM`
- `RUNTIME_OBSERVATION_FOR`

Tree-sitter gives structural consistency, but not complete semantic resolution. It should be treated as the fast, cross-language baseline.

---

### Rust

For Rust code analysis, `rust-analyzer` is specifically focused on Rust and is the best semantic analysis source.

Use cases:

- Symbol resolution
- Type information
- Definitions and references
- Module graph analysis
- Diagnostics
- IDE-grade semantic metadata

Possible integration approach:

- Use Tree-sitter for fast structural indexing.
- Use rust-analyzer where deeper semantic resolution is needed.
- Store rust-analyzer-derived facts as enriched graph metadata.

Runtime/profiling options:

- `cargo build`
- `cargo test`
- `cargo bench`
- `criterion`
- `perf`
- `cargo-flamegraph`
- coverage tools such as `tarpaulin`

Notes:

- Rust runtime instrumentation usually requires controlling the build or running the compiled binary under a profiler.
- Dynamic function-level injection is harder than in Python or JavaScript.
- Prefer build-time instrumentation, profiling wrappers, or sampling profilers.

---

### Java

Static analysis options:

- JavaParser
- Spoon
- Eclipse JDT
- Tree-sitter Java grammar

Recommended use:

- Use Tree-sitter for fast structure.
- Use JavaParser or Spoon for deeper class/method/type-level analysis.
- Use Maven/Gradle introspection to understand dependencies and build structure.

Runtime/profiling options:

- Java Flight Recorder / JFR
- async-profiler
- VisualVM
- JProfiler or YourKit, though these are commercial
- AspectJ for AOP-style instrumentation

AOP suitability:

Java is one of the strongest candidates for build-time or load-time instrumentation.

Possible workflow:

1. Clone repository.
2. Detect Maven or Gradle.
3. Build project inside Docker.
4. Apply AspectJ or Java agent instrumentation.
5. Run tests or known entrypoints.
6. Collect method-level timing data.
7. Map method names/signatures back to graph nodes.

---

### TypeScript and JavaScript

Static analysis options:

- TypeScript Compiler API
- ESLint parser ecosystem
- Babel parser
- SWC parser
- Tree-sitter TypeScript/JavaScript grammars

Important distinction:

- TypeScript Compiler API and ESLint are static-analysis tools.
- They do not perform runtime profiling by themselves.

Runtime/profiling options:

- Node.js built-in profiler
- V8 profiler
- Chrome DevTools protocol
- Clinic.js
- OpenTelemetry instrumentation
- Babel or SWC transforms for injected instrumentation
- Runtime wrappers/proxies

Instrumentation options:

- Babel plugin to wrap functions
- SWC transform for faster instrumentation
- TypeScript transform before compilation
- Node require/import hooks
- Monkey patching for selected functions/classes
- OpenTelemetry spans around functions

A practical design:

1. Parse functions/classes using Tree-sitter or TypeScript compiler.
2. Generate an instrumentation transform that wraps selected functions.
3. Compile TypeScript if needed.
4. Execute tests or configured scripts using Node.
5. Collect timing, call count, errors, memory snapshots if available.
6. Map runtime events back to source ranges.

Example metrics:

- Function execution count
- Total execution time
- Mean execution time
- P95/P99 time
- Error count
- Allocation/memory impact where available
- Call graph edges observed at runtime

---

### Python

Static analysis options:

- Python `ast` module
- LibCST
- parso
- Tree-sitter Python grammar

Runtime/profiling options:

- `cProfile`
- `profile`
- `py-spy`
- `line_profiler`
- decorators
- `wrapt`
- OpenTelemetry

Python is one of the easiest languages for function-level runtime instrumentation.

Possible techniques:

- Decorator injection
- AST rewriting
- Import hooks
- Monkey patching
- Wrapper functions via `wrapt`
- Running tests under `cProfile`

Possible workflow:

1. Parse Python files and identify functions/classes.
2. Optionally rewrite AST to wrap selected functions.
3. Run tests or entrypoints inside Docker.
4. Capture profiling data.
5. Normalize function names and file/line numbers.
6. Attach runtime metrics to graph nodes.

Python is especially suitable for “inject data into a function and observe behavior” workflows.

---

### Go

Static analysis options:

- Go standard library packages such as `go/ast`, `go/parser`, `go/types`
- `golang.org/x/tools/go/packages`
- Tree-sitter Go grammar

Runtime/profiling options:

- `pprof`
- `go test -bench`
- `go test -cpuprofile`
- `go test -memprofile`
- Delve for debugging

Notes:

- Go supports strong profiling, especially via `pprof`.
- Function-level profiling is very feasible.
- Dynamic injection is less natural than Python/JavaScript.
- Prefer build/test orchestration and profiler output parsing.

Possible workflow:

1. Detect Go module.
2. Run `go test ./...`.
3. Run benchmarks if available.
4. Run CPU/memory profiling where possible.
5. Parse pprof output.
6. Attach function-level data to static graph nodes.

---

### C and C++

Static analysis options:

- Clang/LLVM tooling
- libclang
- clangd
- Tree-sitter C/C++ grammars

Runtime/profiling options:

- Valgrind
- perf
- gprof
- Intel VTune, commercial
- DynamoRIO
- Intel PIN
- Sanitizers
- LLVM instrumentation

Notes:

- C and C++ are the hardest targets for safe automated build/run/instrument workflows.
- Build systems are highly variable.
- Dynamic binary instrumentation is possible but complex.
- Security risk is higher.
- Compilation and dependency discovery can be difficult.

Recommended approach:

- Support static analysis first.
- Add runtime profiling later as an advanced optional adapter.
- Prefer Clang/LLVM-based analysis for semantic depth.
- Use Docker sandboxing and strict resource limits.

---

## Runtime Analysis / Dynamic Profiling Strategy

The main dynamic-analysis idea discussed was:

> Pull down a repository, inspect it, compile or prepare it, execute it, inject AOP or instrumentation artifacts, measure performance, and map results back to the source graph.

This is feasible, but language-specific.

### Best Candidates for Runtime Instrumentation

Easiest:

- Python
- JavaScript
- TypeScript
- Java

Moderate:

- Go
- Rust

Hardest:

- C
- C++

---

## AOP and Instrumentation

Aspect-oriented programming can be useful, especially for Java and build-time instrumentation workflows.

Important clarification:

- AOP generally requires either a compiled artifact, build-time weaving, load-time weaving, or a running program.
- It is not purely static analysis.
- To get runtime metrics, the instrumented code must execute.

Where AOP-style approaches fit:

- Java: AspectJ, Java agents, bytecode instrumentation
- Python: decorators, AST transforms, import hooks
- JavaScript/TypeScript: Babel/SWC transforms, proxies, wrappers
- Rust: procedural macros or build/test instrumentation, but not easy to inject after the fact
- Go: build/test/profiler approach rather than classic AOP

---

## Docker as Execution Environment

Docker is strongly recommended for both static and dynamic analysis.

Benefits:

- Isolation from host machine
- Reproducible toolchains
- Language-specific images
- Dependency installation containment
- Build and execution sandboxing
- Resource limits
- Safer handling of untrusted repositories

Recommended Docker model:

- One base image per language/toolchain.
- Optional specialized images for heavyweight analysis.
- Run each repo analysis as an isolated job.
- Mount repo read-only when possible.
- Write outputs to a controlled workspace volume.
- Disable network after dependencies are installed where possible.
- Set CPU, memory, process, disk, and timeout limits.

Suggested images:

- `trustee-java-analyzer`
- `trustee-node-analyzer`
- `trustee-python-analyzer`
- `trustee-go-analyzer`
- `trustee-rust-analyzer`
- `trustee-cpp-analyzer`

---

## Security Considerations

Dynamic repo execution is risky. Treat all analyzed repositories as untrusted unless explicitly trusted.

Recommended safeguards:

- Docker or stronger sandboxing
- Non-root execution
- Read-only filesystem where possible
- Resource limits
- Network isolation
- Timeout enforcement
- No host secrets mounted
- Separate workspace per run
- Dependency allow/deny policies
- Audit logs for commands executed
- Optional manual approval for unknown build scripts

For higher security, consider:

- Firecracker microVMs
- gVisor
- Kata Containers
- Seccomp/AppArmor profiles
- Egress-blocked build environments

---

## Suggested Architecture

### 1. Repository Intake

Responsibilities:

- Clone or receive repository path.
- Identify languages.
- Detect build systems.
- Detect package managers.
- Detect test commands.
- Detect entrypoints.

Detection examples:

- Java: `pom.xml`, `build.gradle`, `settings.gradle`
- Node/TS/JS: `package.json`, `tsconfig.json`
- Python: `pyproject.toml`, `setup.py`, `requirements.txt`, `tox.ini`
- Go: `go.mod`
- Rust: `Cargo.toml`
- C/C++: `CMakeLists.txt`, `Makefile`, `compile_commands.json`

---

### 2. Static Indexing Pipeline

Responsibilities:

- Walk files.
- Ignore vendored/generated/build artifacts.
- Parse supported languages.
- Chunk code with overlapping windows.
- Compute hashes such as MD5 or stronger content hashes.
- Generate embeddings.
- Extract symbols and syntax entities.
- Build graph nodes and edges.

Outputs:

- Text index
- Embedding index
- Symbol index
- File index
- AST-derived graph
- Dependency graph
- Call/reference graph where possible

---

### 3. Semantic Enrichment Layer

Language-specific analyzers add richer data.

Examples:

- rust-analyzer for Rust
- TypeScript Compiler API for TypeScript
- JavaParser/Spoon/JDT for Java
- Go `go/packages` and `go/types`
- Clang/libclang for C/C++
- Python AST/LibCST for Python

This layer should be optional and incremental. Tree-sitter remains the baseline.

---

### 4. Runtime Analysis Layer

Responsibilities:

- Build or prepare the project.
- Inject or enable instrumentation.
- Run tests, benchmarks, or configured entrypoints.
- Collect runtime data.
- Normalize results.
- Map runtime events back to graph nodes.

Runtime result schema should include:

- Symbol ID
- Language
- File path
- Function/class/method name
- Source range
- Invocation count
- Total time
- Average time
- P95/P99 time where available
- Error count
- Memory data if available
- Profiler source/tool
- Run ID
- Environment metadata

---

### 5. Query Layer

Trustee Search can answer queries using combined evidence:

- Text search
- Embeddings search
- AST/symbol search
- Graph traversal
- Runtime metrics
- Dependency relationships

Example query types:

- “Find the slowest functions in this repo.”
- “Show functions related to checkout that call external services.”
- “Find classes with high runtime cost and many dependencies.”
- “Find all code paths from controller X to database writes.”
- “Which methods are central in the call graph but poorly tested?”
- “Find semantically similar functions across packages.”

---

## Implementation Strategy

### Phase 1: Strong Static Foundation

Build or refine:

- File walker
- Language detector
- Tree-sitter parser adapters
- Symbol extraction
- Chunking
- MD5/content hashing
- Embeddings
- Graph construction
- Search API

Goal:

- Fast, reliable indexing across all target languages.

---

### Phase 2: Language-Specific Static Enrichment

Add adapters in this order:

1. TypeScript/JavaScript using TypeScript Compiler API or Babel/SWC parser
2. Java using JavaParser or Spoon
3. Go using `go/packages`
4. Rust using rust-analyzer
5. Python using AST/LibCST
6. C/C++ using Clang/libclang

Goal:

- Improve semantic precision where Tree-sitter is insufficient.

---

### Phase 3: Dockerized Runtime Execution

Build a job runner that can:

- Select the right Docker image.
- Install dependencies.
- Build project.
- Run tests or benchmarks.
- Capture logs and profiler outputs.
- Enforce resource limits.
- Produce structured JSON results.

Start with the easiest languages:

1. Python
2. JavaScript/TypeScript
3. Java
4. Go
5. Rust
6. C/C++ later

---

### Phase 4: Runtime-to-Graph Mapping

Create a robust normalization layer that maps profiler output back to graph nodes.

Matching keys:

- File path
- Function name
- Class name
- Method signature
- Line range
- Symbol ID
- Language-specific qualified name

This is one of the most important parts of the system. Runtime data is only useful if it reliably connects back to static code entities.

---

### Phase 5: Advanced Search and Ranking

Use combined scoring:

- Text relevance
- Embedding similarity
- Graph centrality
- Static complexity
- Runtime cost
- Error frequency
- Test coverage
- Dependency risk

This turns Trustee Search from a code search tool into a code intelligence tool.

---

## Key Design Recommendation

Use a plugin/adapter architecture.

Each language adapter should expose a common interface:

```rust
trait LanguageAnalyzer {
    fn detect(&self, repo: &Repo) -> DetectionResult;
    fn parse_static(&self, files: &[SourceFile]) -> StaticAnalysisResult;
    fn enrich_semantics(&self, repo: &Repo) -> SemanticAnalysisResult;
    fn prepare_runtime(&self, repo: &Repo) -> RuntimePlan;
    fn run_runtime(&self, plan: RuntimePlan) -> RuntimeAnalysisResult;
}
```

The concrete implementations can call external tools, run Docker jobs, parse JSON outputs, or invoke embedded libraries.

---

## Practical Priorities

Most valuable first version:

1. Tree-sitter indexing for all languages.
2. Symbol graph and chunk graph.
3. Embedding search over overlapping chunks.
4. TypeScript/JavaScript static enrichment.
5. Python runtime profiling via decorators or `cProfile`.
6. Node runtime profiling via instrumentation transforms.
7. Java method-level runtime profiling via async-profiler/JFR or AspectJ.
8. Go profiling via pprof.
9. Rust profiling via cargo tooling/perf/flamegraph.
10. C/C++ runtime analysis as a later advanced capability.

---

## Bottom Line

The concept is technically doable and architecturally sound.

The strongest approach is:

- Rust orchestration core
- Tree-sitter as the universal static parsing baseline
- Language-specific semantic adapters
- Dockerized build/test/runtime execution
- Optional instrumentation/AOP per language
- Runtime metrics mapped back to a unified graph

Python, JavaScript/TypeScript, and Java are the best first runtime-analysis targets. Go and Rust are feasible with profiler-driven workflows. C and C++ are possible but should be treated as advanced due to build complexity, instrumentation difficulty, and security risk.
