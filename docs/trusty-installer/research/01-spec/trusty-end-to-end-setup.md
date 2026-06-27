## Intro

`trusty-tools` is a monorepo of discrete tools — trusty-memory, trusty-search,
trusty-review, and others — that extend claude-mpm. claude-mpm runs without them
but is more capable when they are installed and integrated; trusty-tools is its
primary functionality provider.

Today each tool carries its own install, configuration, and lifecycle semantics,
and the user is responsible for setting up every tool on a per-project basis.
This document proposes **trusty-controller**: a single coordination tool that
manages install, upgrade, restart, configuration, doctor, and health across the
whole stack (claude-mpm + trusty-tools), at both the system and project level.

The goal is a stack the average user — with no knowledge of the underlying
setup requirements — can install, configure, and keep current with minimal
effort. trusty-controller stays a thin coordinator over a versioned, per-tool
contract; it never implements tool-specific logic itself.

---

trusty-tools provides a large number of tools. Each tool (e.g. trusty-memory, trusty-search, trusty-review) has it's own setup/usage semantics. 
Assumption is that trusty-tools is the primary provider of additional functionality for claude-mpm.
claude-mpm can operate without trusty-tools but less effectively when trusty-tools are installed and integrated.

We want to make claude-mpm + trusty-tools easy to install and setup by the average user who is not aware of installation and setup requirements.

---

## Definitions
 - trusty-tools -> this monorepo which provides multiple discrete tools
 - tools -> tools provided by trusty-tools, e.g: trusty-memory, trusty-search, trusty-review, etc. Installable via cargo
 - trusty-controller -> a tool responsible for inter-tool coordination of tasks.
 - claude-mpm -> claude-mpm orchestrator
 - claude-mpm stack -> claude-mpm and trusty-tools

## Primary core goals
 1. A new tool 'trusty-controller' which will coordinate setup of trusty-memory, trusty-search, trusty-review and other tools (as defined) at the system and project level.
 2. 'trusty-controller' should be:
   - part of trusty-tools monorepo
   - is installable via cargo
   - is responsible for dispatching various install/upgrade/restart/configuration/doctor tasks
 3. Follow the Unix philosophy of discreet and sharp CLI tools. Tool APIs are important as well.  

## Secondary usability goals

### trusty-controller UI
trusty-tools like trusty-memory and trusty-search provide a web ui. 
Visualization is important as it provides an overview and also a control plane.
trusty-controller should provide a similar UI out of the box
  - once installed and running, UI is available
  - show all installed tools and versions
  - provide tool upgrade indicators and means to upgrade from UI and CLI commands to do it from a terminal
  - show health of all installed and running tools
  - provide ability to run 'doctor' tasks for each tool and display results
  - provide a comprehensive 'doctor' task to determine overall claude-mpm + trusty-tools health.
trusty-controller UI must not reimplement UI functionality present in e.g. trusty-memory trusty-search UIs but rather link to these UIs where applicable. 

## Out of scope - Non-Goals
 - not an agent orchestrator (that's claude-mpm)
 - not a replacement for cargo/package management
 - not a tool-internal config editor
 - no uninstall

## Division of responsibility
 1. trusty-controller is a coordinator and never a direct implementor of tool specific operations. 
   - tools are responsible for providing a well defined control surface (API & CLI)
   - trusty-controller is aware of each tools control surface and provided functionality. 
 2. trusty tools (trusty-memory, trusty-search, trusty-review, etc) provide tool specific commands to perform actions like restart/configuration/doctor 

## Hard dependencies

1. Rust tool-chain is installed and functioning correctly. e.g. we need 'cargo' to bootstrap

## Contract

Interactions between trusty-controller and tools require a well defined contract.  
Conventions every tool implements:
- <tool> doctor --json → stable JSON schema (checks, status, remediation hints)
- <tool> health --json → running/degraded/down + version
- <tool> version --json
- <tool> restart / <tool> config
In case of claude-mpm (python based), the claude-mpm (cli or related cli commands like mpm-doctor) is to provide matching functionality. 
Controller must contain zero tool-specific logic.
The contract itself must be versioned to allow future modifications and ensure tools adhere to the correct contract semantics.
Each tool advertises the contract version it implements via <tool> version --json (e.g. contract_version), and the controller uses this to negotiate behavior and degrade gracefully against tools on an older contract rather than failing.

## Example trusty-controller CLI operations

 - install stack
 - show available updates for all stack tools. plus changelog headlines for each tool between current and newest available version. 
 - upgrade stack
 - restart all demonized tools and UI services. 
 - determine health of all tools contributing to the stack
 - stack doctor 

## Tool versioning and version/changelog advertisement
The stack needs a manifest/BOM/lockfile pinning known-good tool-version combinations and a notion of a "stack version".
The stack manifest/BOM doubles as the controller's tool registry: it enumerates each stack member, its binary name/path, and pinned version, so the controller discovers the available control surfaces from the manifest rather than probing or hard-coding them.
"Changelog headlines" requires structured, parseable changelogs per tool

## Scope Model: System vs. Project

Some trusty tools have **two layers**: a singleton **system** daemon plus per-project **state** it serves. Scope is a first-class axis of the contract.

| Layer | Cardinality | Examples |
  |---|---|---|
| **System** | one per machine | daemon, port, runtime/model, version |
| **Project** | one per repo/cwd | index, palace, `.mcp.json`, config overrides |

Status is composite: a daemon can be healthy while a project is unindexed.

### Verbs are scope-polymorphic
- `install` / `upgrade` / `restart` → **system** (affect all projects/sessions)
- `health` / `doctor` / `config` → **both** (daemon vs. project resource; project config overrides system)
- index / palace create → **project** (only valid once system exists)

### Consequences
1. **Readiness is layered:** `system: installed→running→healthy→version-ok` then `project: configured→exists→fresh→ready`. Unindexed = *system-ready, project-pending*, not broken.
2. **Idempotency differs:** `install` runs once; `ensure project` runs every launch and must no-op when set up.
3. **Blast radius:** system ops disrupt every project/session; project ops must never trigger them implicitly.
4. **Config precedence:** project overrides system defaults.

### Contract
- `--scope project|system|all` on verbs (default `all` in a project dir, else `system`).
- Each `doctor`/`health` check carries its own `scope`:
  ```json
  { "id": "daemon-running", "scope": "system",  "status": "ok" }
  { "id": "index-fresh",    "scope": "project", "status": "pending" }
- System-mutating ops tagged system so the controller can warn before acting.
- Shared project identity convention (e.g. git root → index-id) binds project ops to the right cwd.

Guarantees

- Ordering: ensure system → then project (no indexing against a dead daemon) — this is progressive readiness.
- Rollup: stack doctor renders a tools × scope matrix; system failures are global, project "pending" is local/in-progress.

## Anti-patterns

Anti-patterns related to usability of the claude-mpm stack:
 1) user needs to install, upgrade, configure and start each tool on a per project basis


## Usecases - User point of view

### UUC1

User of a claude-mpm stack wants to have a fully integrated stack utilizing trusty-memory, trusty-search, trusty-review.
When claude-mpm is launched in a project directory, all relevant tools are auto-configured and usable. 
This is a lowest possible effort config scenario. 
Initiation with progressive readiness, for example: 
  - trusty-search can be setup immediately but needs time to index a project. What does waiting for completion look like?
  - tools might need time to start/stop/upgrade. What does this process look like?

### UUC2

A new user of claude-mpm stack has zero knowledge related to how to setup the stack (claude-mpm + trusty-tools).
The user wants a simple method to install and configure the stack. 

### UUC3

claude-mpm stack (claude-mpm + trusty-tools) is a rapidly evolving tool set. 
User of the stack needs to be aware of updates to any of the tools and have an easy path to upgrade these tools.
Once upgraded, the new versions of tools must take effect. 

## Usecases - Maintainer point of view

### MUC1

Maintainer needs to be able to test the stack installation process in isolation, e.g. vanilla container or VM. 
Using maintainer's system and per project setup (maintainer is also the user of installed stack) is an anti-pattern.

### MUC2
Testing isolated MacOs installation is primary. Linux is secondary but also important.   

## Open Questions

1. Naming entities is critical. What name do we pick for the coordination tool. This document names it 'trusty-controller'
   - The monorepo is called 'trusty-tools' which is on point and accurate.
   - The effort here is to provide a coordination/control plane for all tools in trusty-tools and perhaps also for claude-mpm.
   - other naming choices might be:
     a) 'trusty-installer' -> too specific as we might want to use this functionality not only for installation but also as a control plane
     b) 'trusty-tools' -> good name but might cause nomenclature confusion between trusty-tools as monorepo and control plane tool. 
     c) 'trusty-ctl' -> shorter than trusty-controller while also informative 
