# Agent Roster Reference

Generated from `bundle::ALL` (filtered to `agents/*.md`) + `agent_metadata::agent_metadata_from_str` — the same frontmatter parser `agent_builder` uses at compose time. Regenerate with `tm generate capabilities`.

Every agent below transitively `extends: base-agent` (directly, or via `base-engineer`/`base-qa`/`base-ops`/`base-research`) — see `agents/BASE-AGENT.md` and its four role-specific bases for the shared foundation, not repeated per row here.

37 concrete agents.

| Agent | Role | Extends | Description | Declared Skills |
|---|---|---|---|---|
| `api-qa` | qa | base-qa | Specialized API and backend testing for REST, GraphQL, and server-side functionality | systematic-debugging, test-driven-development, testing-anti-patterns |
| `code-analyzer` | code-analyzer | base-research | Code analysis specialist. Reviews code for correctness, quality, security, and architectural health using static analysis. |  |
| `code-critic` | qa | base-qa | Adversarial code review using a structured rubric. Outputs APPROVE/WARN/BLOCK verdict with line-level citations. Independent of implementer to avoid anchoring bias. | code-review-standards, contract-driven-testing |
| `dart-engineer` | engineer | base-engineer | Specialized Dart/Flutter engineer for cross-platform mobile, web, and desktop development with modern null safety and state management | systematic-debugging, test-driven-development |
| `data-engineer` | data-engineer | base-engineer | Data transformation specialist. Builds ETL pipelines, performs database migrations, and processes large datasets efficiently. | systematic-debugging, test-driven-development |
| `documentation` | documentation | base-agent | Documentation specialist. Creates, reorganises, and maintains technical documentation with consistency across the project. | documentation-style |
| `dotnet-engineer` | engineer | base-engineer | Modern C#/.NET 8+ specialist: ASP.NET Core, EF Core, xUnit, nullable reference types, minimal APIs, async/await — plus legacy VB.NET maintenance awareness |  |
| `engineer` | engineer | base-engineer | General-purpose software engineer. Implements features, fixes bugs, refactors code. | systematic-debugging, test-driven-development |
| `gcp-ops` | ops | base-ops | Specialized agent for Google Cloud Platform operations, authentication, and resource management | systematic-debugging |
| `golang-engineer` | engineer | base-engineer | Go 1.23-1.24 specialist: concurrent systems, goroutine patterns, interface-based design, high-performance idiomatic Go | systematic-debugging, test-driven-development |
| `java-engineer` | engineer | base-engineer | Java 21+ LTS specialist delivering production-ready Spring Boot applications with virtual threads, pattern matching, modern performance optimizations, and comprehensive JUnit 5 testing | systematic-debugging, test-driven-development |
| `javascript-engineer` | engineer | base-engineer | Vanilla JavaScript specialist: Node.js backend (Express, Fastify, Koa), browser extensions, Web Components, modern ESM patterns, build tooling | systematic-debugging, test-driven-development |
| `local-ops` | ops | base-ops | Local development environment specialist for process supervision, Docker, database lifecycle, and quality gates | systematic-debugging |
| `memory-manager` | memory-manager | base-agent | Manages project memory via the trusty-memory MCP backend — store, recall, tag, and prune facts using domain-aware organisation |  |
| `mpm-agent-manager` | mpm-agent-manager | base-agent | Manages agent lifecycle in trusty-mpm — discovery, validation, bundled-asset deployment, and contribution workflow for the agent catalog | tm-capabilities |
| `mpm-skills-manager` | mpm-skills-manager | base-agent | Manages skill lifecycle in trusty-mpm — discovery, deployment, tech-stack-based recommendations, and contribution workflow for the skills catalog | tm-capabilities |
| `nextjs-engineer` | engineer | base-engineer | Next.js 15+ specialist: App Router, Server Components, Partial Prerendering, performance-first React applications | systematic-debugging, test-driven-development |
| `ops` | ops | base-ops | Local operations specialist for deployment, DevOps, and process management. Manages environments, services, and quality gates. | systematic-debugging |
| `phoenix-engineer` | engineer | base-engineer | Elixir/Phoenix specialist for building web applications, APIs, and LiveView experiences with solid OTP and Ecto foundations | systematic-debugging, test-driven-development |
| `php-engineer` | engineer | base-engineer | PHP 8.4-8.5 + Laravel 11-12 specialist: strict types, modern security (WebAuthn/passkeys), performance-first applications | systematic-debugging, test-driven-development |
| `prompt-engineer` | engineer | base-engineer | Expert prompt engineer specializing in LLM optimization: model selection, extended thinking, tool orchestration, structured output, and context management. Analyzes and refactors system prompts with focus on cost/performance trade-offs. |  |
| `python-engineer` | engineer | base-engineer | Python 3.12+ development specialist: type-safe, async-first, production-ready implementations with SOA and DI patterns | systematic-debugging, test-driven-development |
| `qa` | qa | base-qa | Expert quality assurance engineer. Designs test strategies, implements automation, and validates software quality. | systematic-debugging, test-driven-development, testing-anti-patterns |
| `react-engineer` | engineer | base-engineer | Specialized React development engineer focused on modern React patterns, performance optimization, and component architecture | systematic-debugging, test-driven-development |
| `refactoring-engineer` | engineer | base-engineer | Safe, incremental code improvement specialist focused on behavior-preserving transformations with comprehensive testing | systematic-debugging, test-driven-development |
| `research` | research | base-research | Expert research analyst. Investigates codebases, maps architectures, assesses technology stacks, and captures structured findings. |  |
| `ruby-engineer` | engineer | base-engineer | Ruby 3.4 + YJIT + Rails 8 specialist: 30% faster method calls, Kamal deployment, service objects, production-ready Rails applications | systematic-debugging, test-driven-development |
| `rust-engineer` | engineer | base-engineer | Rust 2024 edition specialist: memory-safe systems, zero-cost abstractions, ownership/borrowing mastery, async patterns with tokio. Defers all pattern decisions to the toolchains-rust-core skill. | systematic-debugging, test-driven-development, rust-build-performance |
| `security` | security | base-agent | Security specialist. Performs vulnerability assessment, attack vector detection, secret scanning, and compliance review. | security-scanning |
| `svelte-engineer` | engineer | base-engineer | Specialized agent for modern Svelte 5 (Runes API) and SvelteKit development. Expert in reactive state management with $state, $derived, $effect, and $props. Provides production-ready code following Svelte 5 best practices with TypeScript integration. | systematic-debugging, test-driven-development |
| `tauri-engineer` | engineer | base-engineer | Tauri desktop application specialist: hybrid web UI + Rust backend, IPC patterns, state management, system integration, cross-platform development with <10MB bundle sizes | systematic-debugging, test-driven-development, rust-build-performance |
| `ticketing` | ticketing | base-agent | Ticket management specialist. Creates, updates, and tracks issues with scope validation, scope-aware linking, and workflow state intelligence. |  |
| `typescript-engineer` | engineer | base-engineer | TypeScript 5.6+ specialist: strict type safety, branded types, performance-first, modern build tooling | systematic-debugging, test-driven-development |
| `vercel-ops` | ops | base-ops | Vercel platform operations specialist for deployment, edge functions, environment management, and serverless architecture | systematic-debugging |
| `version-control` | version-control | base-ops | Git operations specialist. Manages branches, versioning, releases, and merge conflict resolution with clean history. | git-workflow |
| `web-qa` | qa | base-qa | Progressive 6-phase web testing with UAT mode for business intent verification, behavioral testing, and comprehensive acceptance validation alongside technical testing | systematic-debugging, test-driven-development, testing-anti-patterns |
| `web-ui-engineer` | engineer | base-engineer | Front-end web specialist with expertise in HTML5, CSS3, JavaScript, responsive design, accessibility, and user interface implementation | systematic-debugging, test-driven-development |
