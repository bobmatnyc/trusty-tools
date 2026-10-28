# Agent Roster Reference

Generated from content/agents (the agent roster) + `agent_metadata::agent_metadata_from_str` — the same frontmatter parser `agent_builder` uses at compose time — with the **Category** and **Deploys When** columns read from the bundled `framework-manifest.toml`, the same file the deployer consults — this table is that manifest's rendered view, never a second copy of it. Regenerate with `tm generate capabilities`.

**Category** is the DEPLOYMENT gate (#4760) and **Deploys When** is the condition it resolves to (#4765): `universal` deploys to every project with no detection; `language`, `framework`, and `platform` deploy only when one of the listed markers is present at the project root or at a declared workspace member; `deprecated` never deploys. A marker written `path::needle` is a bounded content probe of that file. This axis is distinct from ADR-0025's four-category agent model, which classifies by who authored an agent and where it lives — every row below is an ADR-0025 category-1 or category-2 bundled agent.

Every agent below transitively `extends: base-agent` (directly, or via `base-engineer`/`base-qa`/`base-ops`/`base-research`) — see `agents/BASE-AGENT.md` and its four role-specific bases for the shared foundation, not repeated per row here.

39 concrete agents.

| Agent | Category | Deploys When | Role | Extends | Description | Declared Skills |
|---|---|---|---|---|---|---|
| `api-qa` | universal | always | qa | base-qa | Specialized API and backend testing for REST, GraphQL, and server-side functionality | systematic-debugging, test-driven-development, testing-anti-patterns |
| `code-analyzer` | universal | always | code-analyzer | base-research | Code analysis specialist: static review of correctness, quality, security, architecture | code-review-standards |
| `code-critic` | universal | always | qa | base-qa | Adversarial code reviewer: rubric-based APPROVE/WARN/BLOCK verdict with line-level citations | code-review-standards, contract-driven-testing |
| `dart-engineer` | language | `pubspec.yaml` | engineer | base-engineer | Dart/Flutter specialist: cross-platform mobile, web and desktop apps, null safety, state | systematic-debugging, test-driven-development |
| `data-designer` | universal | always | data-designer | base-agent | Data designer: chart form, encoding, colour and label specs; reviews dashboards for readability | data-design |
| `data-engineer` | universal | always | data-engineer | base-engineer | Data specialist: ETL pipelines, database migrations, large-dataset processing | systematic-debugging, test-driven-development |
| `documentation` | universal | always | documentation | base-agent | Documentation specialist: writes, reorganises and maintains consistent technical docs | documentation-style |
| `dotnet-engineer` | language | `*.sln` or `*.csproj` or `*.vbproj` or `global.json` or `Directory.Build.props` | engineer | base-engineer | C#/.NET 8+ specialist: ASP.NET Core, EF Core, xUnit, minimal APIs, async; legacy VB.NET aware |  |
| `elixir-engineer` | language | `mix.exs` | engineer | base-engineer | Elixir/OTP specialist: supervision trees, GenServer/Task processes, fault tolerance, Ecto | systematic-debugging, test-driven-development |
| `engineer` | universal | always | engineer | base-engineer | General-purpose software engineer. Implements features, fixes bugs, refactors code. | systematic-debugging, test-driven-development |
| `gcp-ops` | platform | `app.yaml` or `cloudbuild.yaml` or `cloudbuild.yml` or `.gcloudignore` | ops | base-ops | Specialized agent for Google Cloud Platform operations, authentication, and resource management | systematic-debugging |
| `golang-engineer` | language | `go.mod` | engineer | base-engineer | Go 1.23+ specialist: concurrent systems, goroutine patterns, interface design, idiomatic Go | systematic-debugging, test-driven-development |
| `java-engineer` | language | `pom.xml` or `build.gradle` or `build.gradle.kts` or `settings.gradle` | engineer | base-engineer | Java 21+ specialist: Spring Boot apps, virtual threads, pattern matching, JUnit 5 testing | systematic-debugging, test-driven-development |
| `javascript-engineer` | language | `package.json` | engineer | base-engineer | Vanilla JavaScript specialist: Node.js (Express, Fastify, Koa), browser extensions, ESM | systematic-debugging, test-driven-development |
| `local-ops` | universal | always | ops | base-ops | Local dev environment specialist: process supervision, Docker, database lifecycle, quality gates | systematic-debugging |
| `memory-manager` | universal | always | memory-manager | base-agent | Manages project memory in trusty-memory: store, recall, tag and prune facts by domain |  |
| `mpm-agent-manager` | universal | always | mpm-agent-manager | base-agent | Manages trusty-mpm agents: discovery, validation, bundled-agent deployment, catalog work | tm-capabilities |
| `mpm-skills-manager` | universal | always | mpm-skills-manager | base-agent | Manages trusty-mpm skills: discovery, deployment, stack-based recommendations, catalog work | tm-capabilities |
| `nextjs-engineer` | framework | `next.config.js` or `next.config.mjs` or `next.config.ts` or `package.json::"next"` | engineer | base-engineer | Next.js 15+ specialist: App Router, Server Components, Partial Prerendering | systematic-debugging, test-driven-development |
| `phoenix-engineer` | framework | `mix.exs::{:phoenix,` | engineer | base-engineer | Phoenix specialist: web apps, JSON APIs and LiveView on Elixir/OTP | systematic-debugging, test-driven-development |
| `php-engineer` | language | `composer.json` | engineer | base-engineer | PHP 8.4+ and Laravel 11-12 specialist: strict types, passkey/WebAuthn security, performance | systematic-debugging, test-driven-development |
| `prompt-engineer` | universal | always | engineer | base-engineer | LLM prompt specialist: reviews and refactors system prompts for model fit, tool use, output, cost |  |
| `python-engineer` | language | `pyproject.toml` or `setup.py` or `setup.cfg` or `requirements.txt` or `Pipfile` | engineer | base-engineer | Python 3.12+ specialist: type-safe, async-first production code with SOA and DI patterns | systematic-debugging, test-driven-development |
| `qa` | universal | always | qa | base-qa | QA engineer: designs test strategy, builds test automation, validates software quality | systematic-debugging, test-driven-development, testing-anti-patterns |
| `react-engineer` | framework | `package.json::"react"` | engineer | base-engineer | React specialist: modern React patterns, component architecture, performance tuning | systematic-debugging, test-driven-development |
| `refactoring-engineer` | universal | always | engineer | base-engineer | Refactoring specialist: safe, incremental, behavior-preserving changes backed by tests | systematic-debugging, test-driven-development |
| `research` | universal | always | research | base-research | Research analyst: investigates codebases, maps architecture, assesses tech stacks |  |
| `ruby-engineer` | language | `Gemfile` or `Gemfile.lock` or `.ruby-version` | engineer | base-engineer | Ruby 3.4 + Rails 8 specialist: YJIT, service objects, Kamal deployment, production Rails | systematic-debugging, test-driven-development |
| `rust-engineer` | language | `Cargo.toml` | engineer | base-engineer | Rust 2024 specialist: ownership/borrowing, async tokio, zero-cost abstractions, memory-safe systems | systematic-debugging, test-driven-development, rust-build-performance, rust-delivery-workflow |
| `secrets-manager` | universal | always | secrets-manager | base-agent | Secrets specialist: handles credentials by vault and key name or `secret://` ref, never a value |  |
| `security` | universal | always | security | base-agent | Security specialist: vulnerability assessment, attack vectors, secret scanning, compliance | security-scanning |
| `svelte-engineer` | framework | `svelte.config.js` or `svelte.config.ts` or `package.json::"svelte"` | engineer | base-engineer | Svelte 5 and SvelteKit specialist: Runes ($state, $derived, $effect, $props), TypeScript | systematic-debugging, test-driven-development |
| `tauri-engineer` | framework | `src-tauri/tauri.conf.json` or `tauri.conf.json` | engineer | base-engineer | Tauri desktop app specialist: web UI + Rust backend, IPC, state, cross-platform builds | systematic-debugging, test-driven-development, rust-build-performance |
| `ticketing` | universal | always | ticketing | base-agent | Ticket specialist: creates, updates, links and tracks issues with scope and workflow-state checks |  |
| `typescript-engineer` | language | `tsconfig.json` or `package.json` | engineer | base-engineer | TypeScript 5.6+ specialist: strict type safety, branded types, modern build tooling | systematic-debugging, test-driven-development |
| `vercel-ops` | platform | `vercel.json` or `.vercelignore` or `.vercel/project.json` | ops | base-ops | Vercel operations specialist: deployments, edge functions, environments, serverless | systematic-debugging |
| `version-control` | universal | always | version-control | base-ops | Git specialist: branches, versioning, releases, merge-conflict resolution, clean history | git-workflow |
| `web-qa` | universal | always | qa | base-qa | Web QA: 6-phase browser testing plus a UAT mode that verifies business intent and acceptance | systematic-debugging, test-driven-development, testing-anti-patterns |
| `web-ui-engineer` | universal | always | engineer | base-engineer | Front-end specialist: HTML5, CSS3, JavaScript, responsive design, accessibility | systematic-debugging, test-driven-development |
