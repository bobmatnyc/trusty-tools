# DOC-2 — Stack Manifest/BOM + Version & Changelog Advertisement (FOUNDATIONAL)

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Define the manifest/BOM/lockfile that pins known-good tool-version combinations
and a "stack version", doubling as the controller's **tool registry**: the
single static source from which `trusty-controller` (`tctl`, DOC-0) learns which
members exist, what binary to invoke for each, the version it expects, and the
`contract_version` floor it must satisfy. The manifest is the mechanism that lets
the controller honour the spec's hard rule — *"Controller must contain zero
tool-specific logic"* (spec §83): the controller **enumerates members and their
control surfaces from the manifest, never by probing or hard-coding** (spec §98).

It also defines a **structured, parseable per-tool changelog format** so the
controller can render "changelog headlines for each tool between current and
newest available version" (spec §90, §99) during the upgrade flow (DOC-9).

---

## DESIGN

### 1. Manifest purpose & dual role

The manifest serves **two roles from one document**:

1. **Pinned known-good BOM / lockfile.** A named, tested tuple of member versions
   — the set of versions that have been verified to work *together*. This is the
   "stack version" the spec calls for (§97). It is the upgrade-flow target
   (DOC-9) and the readiness yardstick (`version-ok` rung, DOC-3 §2).

2. **The controller's tool registry.** For every stack member it records the
   stable member id, the binary to invoke, the install source, and the expected
   `contract_version`. The controller reads this registry to know *what to run*
   and *how to talk to it*; it never hard-codes a tool list or a binary name.

These two roles are intentionally fused: the same record that pins a version also
tells the controller how to reach that member's control surface. There is no
second registry.

**Division of labour (the load-bearing rule).** The manifest is the **static
registry**; each tool's `version --json` (DOC-1 D3b) is the **runtime capability
probe**. See §6 for the precise split — in short: *the manifest says which members
exist and where their binaries are; `version --json` says what each member can
actually do right now*.

### 2. Format & location

**Recommended format: TOML.** Rationale: the repo is a Cargo workspace and every
config the team authors by hand is TOML (`Cargo.toml`, `.mcp.json` being the lone
JSON exception). TOML has first-class comments (so the shipped default BOM can be
self-documenting), is diff-friendly for the lockfile use case, and `serde` +
`toml` are already workspace dependencies. JSON is reserved for the wire/contract
layer (DOC-1 envelopes); the manifest is config, not wire, so TOML fits the repo
convention. (Owner-approved: TOML; see Resolved Decision Q1.)

**Location strategy: embedded default + optional SYSTEM-level override file
(owner-approved; see Resolved Decision Q2).** v1 ships exactly two manifest
tiers — a compiled-in default and an optional system-scope override. There is
**no per-project manifest override** in v1. Concretely:

- **Embedded default BOM (compiled-in).** `trusty-controller` ships a canonical
  default manifest **compiled into the `tctl` binary** (via `include_str!` of a
  committed `manifest.toml`, the same pattern trusty-search uses for its embedded
  UI bundle). This guarantees a fresh `cargo install trusty-controller` is
  immediately usable with **zero network fetch** and a known-good BOM that
  matches the controller's release — satisfying the zero-knowledge install goal
  (spec §25, DOC-8). The embedded manifest's stack version is, by construction,
  the one this `tctl` release was tested against.

- **Optional system-level override file.** A user-writable manifest at the
  OS config dir — `~/.config/trusty-controller/manifest.toml` (Linux/XDG) /
  `~/Library/Application Support/trusty-controller/manifest.toml` (macOS),
  resolved via the existing `trusty_common` `resolve_data_dir`/`config` helpers.
  When present it **overrides** the embedded default. This lets a user pin to a
  newer (or older) stack version, or add a member, without rebuilding `tctl`.
  The manifest is **system-scope only** — there is no per-project manifest
  override. (Project-SCOPED *operations* and per-project state/identity still
  exist, but they are owned by DOC-3, not the manifest; see the clarification
  below.)

- **The manifest is system-scope; project state is DOC-3's.** Removing the
  per-project *manifest* override does **not** remove project-scoped behaviour:
  per DOC-3, the manifest describes only *system-layer* members, while
  per-project readiness, state, and identity are discovered at runtime and owned
  by DOC-3 — never stored in the BOM. The two concepts are distinct: there is no
  per-project manifest, but project-scoped operations remain per DOC-3.

- **Fetched/remote channel: explicitly deferred.** Fetching a manifest from a
  release URL (a "stable / beta channel") is a natural extension but is **not in
  v1**. The embedded default already gives a known-good BOM per release; remote
  channels add a fetch/trust/signing surface that DOC-9 (upgrade flow) should own
  if/when it lands. (Owner-approved deferral; see Resolved Decision Q3.)

**Precedence (highest → lowest):**

```
system override file (~/.config/trusty-controller/manifest.toml)
  > embedded default BOM (compiled into tctl)
```

There are only two manifest tiers in v1 (no project tier). This is the
manifest-scope subset of DOC-3 §7's config precedence
(`project > system > built-in default`): the manifest simply has no project
tier, so its rule reduces to `system override > embedded default`.

### 3. Manifest entry schema

Each stack member is one `[[member]]` entry. Fields:

| Field | Req? | Meaning |
|---|---|---|
| `id` | yes | Stable member id; the **manifest key** that matches DOC-1's envelope `tool` field and the DOC-3 project-identity keys. snake/kebab tool name, e.g. `trusty-search`. |
| `display_name` | yes | Human label for UI/CLI, e.g. `"Trusty Search"`. |
| `binary` | yes | Binary name on PATH the controller invokes, e.g. `trusty-search`, `tctl`. (May differ from `id` and from the crate name — cf. `tga`.) |
| `kind` | yes | Member kind: `daemon` (two-layer, has system daemon) \| `cli` (CLI-only / system-only, no project layer) \| `orchestrator` (the pluggable orchestrator slot). Ties to DOC-3 §1 (single-layer vs two-layer) and §10 (orchestrator). |
| `install` | yes | Install source descriptor (a sub-table — see below). Drives DOC-8 install + DOC-9 upgrade. |
| `version` | yes | The **pinned** version for this BOM (the lockfile pin). semver string, e.g. `"0.24.1"`. |
| `min_contract_version` | yes | Lowest `contract_version` (DOC-1 D2) the controller will accept from this member. Integer ≥ 1. A member advertising below this is contract-incompatible (DOC-1 floor rule). |
| `expected_contract_version` | no | The `contract_version` this BOM was tested against (informational; defaults to `min_contract_version`). |
| `ui` | no | UI-availability + discovery hint sub-table (see below). Drives DOC-7 (the controller UI links out to member UIs, never reimplements them — spec §56). Omit for members with no UI. |
| `changelog` | yes | Changelog source descriptor (a sub-table — see §5). Drives DOC-9 headline extraction. |
| `depends_on` | no | Array of member `id`s this member requires at runtime (e.g. `trusty-analyze` → `["trusty-search"]`). Informs DOC-4 rollup / ordering; does not duplicate DOC-1 health `deps`. |
| `enabled` | no | `true` (default) \| `false`. A system override file can disable a member without removing its registry entry. |

**`install` sub-table** (tagged by `source`):

```toml
# cargo source — the common case (crates.io)
install = { source = "cargo", crate = "trusty-search" }   # → cargo install trusty-search --locked
# python source — claude-mpm today (DOC-0 forward-compat; DOC-6 Resolved Decision 5)
install = { source = "python", tool = "uv", package = "claude-mpm" } # → uv tool install claude-mpm
```

- `source = "cargo"` → the controller composes `cargo install <crate> --locked`
  (the exact command DOC-9 reuses via `trusty_common::update::perform_upgrade`,
  which already shells out to `cargo install <name> --locked`).
- `source = "python"` → for the Python orchestrator; `tool` selects `uv`
  (DOC-6 Resolved Decision 5: `uv tool install claude-mpm`). This is the *only*
  non-cargo install path.
- **Sidecars are NOT members.** Per the single-install convention (CLAUDE.md;
  verified: `crates/trusty-search/Cargo.toml` declares `trusty-embedderd` as a
  bundled second binary so `cargo install trusty-search` installs *both*), the
  sidecars `trusty-embedderd` and `trusty-bm25-daemon` ship inside their parent's
  single install and get **no separate `[[member]]` entry**. They surface (if at
  all) through their parent's `doctor`/`health` (DOC-1), never the manifest.

**`ui` sub-table:**

```toml
ui = { available = true, path = "/ui", port_source = "port_json" }
```

- `available` — whether this member serves a UI at all.
- `path` — the UI route (verified: trusty-search/trusty-memory serve at `/ui`).
- `port_source` — how the controller *discovers* the live port (it is **not**
  pinned in the manifest, because the daemon auto-binds): `port_json` = call the
  member's `<binary> port --json` (verified to exist on trusty-search /
  trusty-memory) and build `http://<addr><path>`. This keeps DOC-7 link-out
  dynamic; the manifest only says "this member has a UI and here's how to find
  its URL," never a hard-coded port. The controller's own UI URL is discovered
  the same way (`tctl port --json`).

**`changelog` sub-table** — see §5.

#### Worked example (real current tools, 2026-06-08 versions)

> Versions below are the **actual** current crate versions read from the tree
> (`trusty-search` 0.24.1, `trusty-memory` 0.15.0, `trusty-analyze` 0.5.1,
> `trusty-review` 0.3.6). `trusty-controller` is shown at a placeholder `0.1.0`
> (crate not yet created — DOC-0). `claude-mpm` version is a placeholder pending
> the orchestrator-adapter design (DOC-6).

```toml
# trusty-controller stack manifest / BOM
# Pinned, known-good tuple of member versions = "stack version".
# NO SECRETS — install sources and URLs only (see Security, §8).

stack_version = "2026.06-1"          # see §4 for the naming scheme
contract_floor = 1                    # global controller contract floor (DOC-1)

[[member]]
id = "trusty-controller"
display_name = "Trusty Controller"
binary = "tctl"
kind = "cli"                          # the controller itself: system-only
install = { source = "cargo", crate = "trusty-controller" }
version = "0.1.0"
min_contract_version = 1
ui = { available = true, path = "/ui", port_source = "port_json" }
changelog = { source = "git_tag", crate = "trusty-controller", path = "CHANGELOG.md", format = "keepachangelog" }

[[member]]
id = "trusty-search"
display_name = "Trusty Search"
binary = "trusty-search"
kind = "daemon"                       # two-layer: machine daemon + per-project indexes
install = { source = "cargo", crate = "trusty-search" }
version = "0.24.1"
min_contract_version = 1
ui = { available = true, path = "/ui", port_source = "port_json" }
changelog = { source = "git_tag", crate = "trusty-search", path = "CHANGELOG.md", format = "keepachangelog" }
# NOTE: bundles trusty-embedderd sidecar via single-install — NOT a separate member.

[[member]]
id = "trusty-memory"
display_name = "Trusty Memory"
binary = "trusty-memory"
kind = "daemon"
install = { source = "cargo", crate = "trusty-memory" }
version = "0.15.0"
min_contract_version = 1
ui = { available = true, path = "/ui", port_source = "port_json" }
changelog = { source = "git_tag", crate = "trusty-memory", path = "CHANGELOG.md", format = "keepachangelog" }
# NOTE: bundles trusty-bm25-daemon sidecar via single-install — NOT a separate member.

[[member]]
id = "trusty-analyze"
display_name = "Trusty Analyze"
binary = "trusty-analyze"
kind = "daemon"
install = { source = "cargo", crate = "trusty-analyze" }
version = "0.5.1"
min_contract_version = 1
depends_on = ["trusty-search"]        # hard runtime dep (DOC-1 grounding)
changelog = { source = "git_tag", crate = "trusty-analyze", path = "CHANGELOG.md", format = "keepachangelog" }

[[member]]
id = "trusty-review"
display_name = "Trusty Review"
binary = "trusty-review"
kind = "cli"                          # system-only (DOC-3 Q2): CLI today is serve-only
install = { source = "cargo", crate = "trusty-review" }
version = "0.3.6"
min_contract_version = 1
depends_on = ["trusty-search", "trusty-analyze"]
changelog = { source = "git_tag", crate = "trusty-review", path = "CHANGELOG.md", format = "keepachangelog" }

[[member]]
id = "claude-mpm"
display_name = "Claude MPM (orchestrator)"
binary = "claude-mpm"
kind = "orchestrator"                 # the pluggable orchestrator slot (DOC-0 A4 / DOC-3 §10)
install = { source = "python", tool = "uv", package = "claude-mpm" }
version = "0.0.0"                     # placeholder — pinned by DOC-10 discover-and-freeze mechanism
min_contract_version = 1              # satisfied via the Python contract adapter (DOC-6)
changelog = { source = "url", url = "https://raw.githubusercontent.com/bobmatnyc/claude-mpm/main/CHANGELOG.md", format = "keepachangelog" }
```

### 4. "Stack version" definition

A **stack version is a named, tested lockfile of member versions** — i.e. the
top-level `stack_version` string plus the set of `[[member]].version` pins it
labels. It is net-new: today every crate versions itself independently and the
workspace has **no shared version field** (verified: root `Cargo.toml`
`[workspace.package]` removed its `version` per #343; `version = ... — REMOVED`).
The stack version layers a *coordinating* concept **on top of** that independence
without taking it away:

- **What it IS:** an immutable, human-named tuple — "this exact combination of
  member versions was tested together and is known-good." Analogous to a
  `Cargo.lock` for the whole stack, but curated and released, not auto-generated.
- **Relationship to per-crate versions (#343):** crates keep versioning and
  publishing independently (the existing per-crate `<crate>-v<version>` tag
  convention, CLAUDE.md release section, is unchanged). The stack version does not
  replace or constrain a crate's own version; it merely **records a snapshot** of
  which independent versions are pinned together. A crate can ship `0.24.2`
  to crates.io without any stack version existing for it yet; the stack version is
  cut later, when the combination is tested.
- **Naming scheme (owner-approved):** `YYYY.MM-N` (e.g. `2026.06-1`), a
  date-anchored, monotonically-increasing label decoupled from any single crate's
  semver — because no single crate's version can name the whole tuple. (See
  Resolved Decision Q4.)
- **How a user pins / moves between stack versions:**
  - *Pin:* the embedded default BOM already pins the stack version this `tctl`
    release shipped with. To move, the user either (a) upgrades `tctl` itself
    (the new binary carries a newer embedded BOM), or (b) drops a system override
    `manifest.toml` declaring a different `stack_version` + pins.
  - *Move:* DOC-9's `upgrade stack` walks the **current** manifest to the
    **target** manifest, upgrading each member to its target `version` via the
    `install` descriptor (reusing `trusty_common::update`). "Available updates"
    (spec §90) = the diff between installed member versions and the target BOM's
    pins, annotated with changelog headlines (§5).
  - The stack version is therefore the unit a user reasons about ("I'm on
    `2026.06-1`; `2026.07-1` is available") even though the underlying crates
    moved independently.

### 5. Structured changelog format

The spec requires **"changelog headlines for each tool between current and newest
available version"** (§90) and notes this **"requires structured, parseable
changelogs per tool"** (§99). Grounding: every crate already has a `CHANGELOG.md`,
and they already declare **Keep a Changelog** format (verified:
`crates/trusty-search/CHANGELOG.md` header — *"Format follows Keep a Changelog…
Versions correspond to Cargo.toml patch releases"*). So the format is **not
fully net-new** — it is *already keepachangelog-shaped*; what is net-new is making
it **reliably machine-parseable** for headline extraction.

**Decision: standardize on Keep a Changelog (1.0.0) as the parse-target
contract, parsed best-effort with graceful degradation — no CI enforcement
gate** (owner-approved; see Resolved Decision Q5). The team already writes this
format, so a new format would mean rewriting 10 changelogs for no gain. The
parse target follows these conventions:

1. **Version headers are H2 in the form** `## [<semver>] — <YYYY-MM-DD>`
   (em-dash or hyphen accepted). This is the parse anchor — the controller splits
   the file on `## [` headers, reads the bracketed semver, and selects the slice
   of entries with `installed_version < v <= target_version`.
2. **Each entry is a Markdown list item** under a category heading
   (`### Added/Changed/Fixed/Removed/Deprecated/Security`).
3. **The first line of each list item is the headline** (the "headline" = the
   leading bolded summary; trusty-search already writes
   `- **#868 — short summary** — long detail…`). Headline extraction takes the
   list item up to the first sentence / first `—` continuation, so the rendered
   "headlines between A and B" are the bolded leaders, not the full prose.

**Conformance is a convention, not a CI-enforced gate.** Headline extraction is
**best-effort**: when a tool's changelog is missing, unreachable, or does not
conform to the H2 anchor above, the controller **degrades gracefully** — it
omits/skips headlines for that one tool and surfaces a soft note ("changelog
headlines unavailable for `<tool>`") rather than failing the upgrade flow. There
is **no lint or CI gate** enforcing the changelog format; the existing
changelogs already conform, and any drift simply costs headlines for the
affected tool, never a build failure.

**`changelog` sub-table schema:**

```toml
# In-tree / crates.io tools (the Rust crates):
changelog = { source = "git_tag", crate = "trusty-search", path = "CHANGELOG.md", format = "keepachangelog" }
# External tool (claude-mpm) — fetched over HTTP:
changelog = { source = "url", url = "https://…/CHANGELOG.md", format = "keepachangelog" }
```

- `source = "git_tag"` → the changelog is published alongside the crate; the
  controller resolves it from the crate's published artifact / repo
  (the per-crate `<crate>-v<version>` tag convention gives a stable anchor).
- `source = "url"` → fetch a raw `CHANGELOG.md` (claude-mpm, external repo).
- `format = "keepachangelog"` → the only format defined in v1; the field exists so
  a future tool could declare a different parser without a contract change.
- **Where each tool publishes it:** each crate's repo-root / crate-root
  `CHANGELOG.md` (already present for all members). claude-mpm publishes its own
  upstream `CHANGELOG.md`, reached via `source = "url"`.

This block **feeds DOC-9** (the upgrade flow renders the extracted headlines in
both CLI and UI).

### 6. Discovery rule (manifest vs. `version --json`)

The controller's knowledge of the stack is split cleanly between the **static
manifest** and each member's **runtime `version --json`** (DOC-1 D3b):

| Question | Answered by | Why |
|---|---|---|
| Which members exist? | **manifest** (`[[member]]` enumeration) | zero hard-coded tool list (spec §98) |
| What binary do I invoke? | **manifest** (`binary`) | controller never guesses a binary name |
| How do I install/upgrade it? | **manifest** (`install`) | DOC-8 / DOC-9 |
| What version is pinned/known-good? | **manifest** (`version`, `stack_version`) | BOM / `version-ok` rung (DOC-3) |
| What `contract_version` must it meet? | **manifest** (`min_contract_version`) | DOC-1 floor negotiation |
| Does it have a UI / where? | **manifest** (`ui`) + member `port --json` | DOC-7 link-out |
| **What can it actually do right now?** | **`<binary> version --json` `verbs[]`** (DOC-1) | runtime capability — never stored statically |
| Is it healthy / fresh? | **`<binary> health`/`doctor`** (DOC-1) | runtime state — never stored statically |

**Rule:** *the manifest is the registry of which members exist and how to reach
them; `version --json` is the runtime probe of what each member can do.* The
controller enumerates members from the manifest, then for each runs
`<binary> version --json` to learn its advertised `verbs[]` and live
`contract_version`, and dispatches only advertised verbs (DOC-1 D3c generic
passthrough). The manifest is **never** a substitute for the runtime probe (a
member's capabilities can change between releases without a manifest edit), and
the runtime probe is **never** used to *discover members* (you must already know
the binary to probe it — that comes from the manifest). This is exactly the
"zero tool-specific logic" guarantee: nothing about a tool is compiled into the
controller except the ability to read the manifest and parse the contract.

### 7. Orchestrator forward-compatibility

The manifest models the orchestrator as **one swappable member** (DOC-0 A4,
DOC-3 §10), via the `kind = "orchestrator"` slot:

- **Today:** the `claude-mpm` entry uses `install = { source = "python", tool =
  "uv", package = "claude-mpm" }` and a `source = "url"` changelog. Its contract
  surface is **synthesized by the Python adapter** (DOC-6); from the manifest's
  point of view it is just another `[[member]]` with a `min_contract_version`.
- **Later:** when `trusty-mpm` (the in-house Rust replacement, not yet ready —
  DOC-0) ships, swapping the orchestrator is a **single manifest edit**: replace
  the `claude-mpm` entry with a `trusty-mpm` entry using
  `install = { source = "cargo", crate = "trusty-mpm" }` and a `git_tag`
  changelog. **No controller code changes** — the controller already discovers
  the orchestrator from the manifest and talks to it over the contract. The
  `id`/`kind = "orchestrator"` continuity means DOC-3's system-layer model and
  DOC-4's rollup treat the new entry identically.

The forward-compat requirement (DOC-0) is thus satisfied structurally: the only
orchestrator-specific knowledge in the whole system lives in (a) the manifest
entry and (b) the DOC-6 adapter — never in the controller.

### 8. Security (cross-cutting)

- **No secrets in the manifest, ever.** The manifest is committed/embedded and
  may ship inside the public `tctl` binary; it carries only member ids, binary
  names, install sources (crate names / package names / URLs), version pins, and
  UI paths — **never** API keys, tokens, AWS credentials, or connection strings.
  This is the manifest-side counterpart to DOC-1 D8 (which redacts secrets in
  *contract output*); together they ensure no secret transits either the static
  registry or the runtime wire.
- **Only a trusted system override exists.** Per §2, v1 has **no per-project
  manifest override** — the only override tier is the system-level file, which is
  trusted by virtue of being user-owned at the OS config dir. Because no
  checked-out repo can contribute manifest entries or install sources, a hostile
  repo cannot make the controller run an arbitrary install command.
- **No code execution from the manifest.** The manifest is declarative data; the
  controller composes install/upgrade commands from a fixed set of `source`
  templates (`cargo install …`, `uv tool install …`) — it never executes a free-form
  command string from the manifest.

---

## Dependencies

### Consumes (inputs)
- **DOC-0** — the chosen `<name>` (`trusty-controller` / binary `tctl`), the
  crates.io publishing convention (the manifest's `cargo` install source), and the
  orchestrator-swap forward-compat requirement (A4 → §7).
- **DOC-1** — the per-member `contract_version` (D2) that `min_contract_version`
  gates against, and `version --json` `verbs[]` (D3b) that the discovery rule (§6)
  pairs with the static registry.

### Produces (consumed by)
- **DOC-5** — the CLI dispatcher reads the registry to resolve `binary` per member.
- **DOC-6** — the conformance matrix + claude-mpm adapter consume the member list
  and `min_contract_version` pins; the orchestrator entry (§7) is the adapter's
  manifest anchor.
- **DOC-7** — the web UI reads `ui` hints to link out to member UIs and renders
  the installed-vs-pinned version table.
- **DOC-8** — install/bootstrap reads `install` descriptors (incl. the Python
  path) to install each member.
- **DOC-9** — upgrade flow reads `version` pins (to compute "available updates")
  and `changelog` descriptors (to render headlines), reusing
  `trusty_common::update` (`check_crates_io`, `perform_upgrade`,
  `upgrade_and_restart`, `is_launchd_supervised`).

> These edges match the README dependency graph (DOC-2 consumes DOC-0 + DOC-1;
> produces into DOC-5, DOC-6, DOC-7, DOC-8, DOC-9).

## Grounding (exists vs. net-new)

Source-first audit, 2026-06-08.

| Area | Reality today | Manifest implication |
|---|---|---|
| **Per-crate versioning** | Each crate owns its `version` in its own `Cargo.toml`; `[workspace.package]` has **no** version field (`version … — REMOVED`, #343). Current pins read from tree: search 0.24.1, memory 0.15.0, analyze 0.5.1, review 0.3.6, common 0.14.1, mpm 0.6.2. | "Stack version" is **net-new** — a coordinating tuple layered over independent crate versions (§4). |
| **Install mechanism** | `cargo install <crate>`; UI-embedding crates need `SKIP_UI_BUILD=1` at publish; binaries installed `--locked`; per-crate git tag `<crate>-v<version>` (CLAUDE.md release section). | `install = { source = "cargo", crate = … }` composes `cargo install <crate> --locked`. |
| **Update machinery** | `trusty_common::update` already exposes `check_crates_io`, `perform_upgrade` (shells `cargo install <name> --locked`), `upgrade_and_restart`, `is_launchd_supervised`, `UpdateInfo`, `notice`. | The manifest **feeds** this existing machinery (DOC-9); no new upgrade primitive needed for cargo members. |
| **Single-install / sidecars** | Verified: `crates/trusty-search/Cargo.toml` bundles `trusty-embedderd` as a second `[[bin]]` so `cargo install trusty-search` installs both; trusty-memory similarly parents `trusty-bm25-daemon`. | Sidecars get **no** `[[member]]` entry; they ride their parent's single install (§3). |
| **Changelogs** | Every member crate has a `CHANGELOG.md`; they **already declare Keep a Changelog** format (search header confirms it) with `## [x.y.z] — DATE` headers and bolded headline list items. | Changelog format is **mostly grounded** — keepachangelog is standardized as the parse contract; net-new work is only making H2 headers reliably machine-parseable (§5). |
| **UI discovery** | trusty-search/trusty-memory serve a UI at `/ui` and expose `port --json` / `port.lock`. | `ui = { available, path = "/ui", port_source = "port_json" }`; the live port is discovered at runtime, never pinned (§3). |
| **claude-mpm install** | External Python tool; DOC-6 Resolved Decision 5 confirms the install path is `uv tool install claude-mpm` — "the one path not [cargo]". | `install = { source = "python", tool = "uv", package = "claude-mpm" }` (§3, §7). |
| **The manifest itself** | **Fully net-new.** No manifest/BOM, no `stack_version`, no tool registry exists today. | This whole document. |

## Cross-cutting notes

- **Security / secrets:** no secrets in the manifest — install sources, URLs, and
  version pins only (§8; counterpart to DOC-1 D8).
- **Single precedence rule:** manifest override precedence is
  `system override > embedded default` — the manifest-scope subset of DOC-3 §7's
  config precedence (which has no project tier for the manifest in v1). One
  mental model across config and manifest.
- **Manifest is system-scope; project state is not in the manifest:** v1 has
  **no per-project manifest override**. Per DOC-3, the manifest describes
  **system-layer** members only; per-project readiness, state, and identity
  (configured/exists/fresh) are discovered at runtime via the contract and owned
  by DOC-3, never stored in the BOM. Project-scoped *operations* still exist
  (DOC-3); only a per-project *manifest* override is excluded.

## Remaining work

- [x] Pick format & location strategy (TOML; embedded default + **system-level**
      override only — no per-project manifest override in v1)
- [x] Define the `[[member]]` entry schema (incl. `install`, `ui`, `changelog`,
      `kind`, contract pins) with annotated example listing the real current tools
- [x] Define "stack version" + the lockfile-of-version-tuples model (#343-aware)
- [x] Choose stack-version naming scheme (date-anchored `YYYY.MM-N`)
- [x] Choose a structured, parseable changelog format (keepachangelog as the
      parse target) and the headline-extraction rule — **best-effort, no CI gate**
- [x] Fix the discovery rule (manifest = static registry; `version --json` =
      runtime capability) and document the division of labour
- [x] Orchestrator forward-compat (claude-mpm now → trusty-mpm later) via the
      swappable `kind = "orchestrator"` entry
- [x] Security note (no secrets; system-only override; no code execution)
- [x] **Owner: resolve the open questions** (see Resolved Decisions below)
- [ ] *(implementation-time)* wire best-effort changelog parsing + graceful
      degradation into DOC-9's upgrade flow
- [x] *(DOC-6-owned)* resolve the canonical `claude-mpm` install path and package
      name (RESOLVED in DOC-6 Resolved Decision 5: `uv tool install claude-mpm`,
      package `claude-mpm`, changelog URL
      `https://raw.githubusercontent.com/bobmatnyc/claude-mpm/main/CHANGELOG.md`)
- [x] *(DOC-10-owned)* discover-and-freeze the concrete `claude-mpm` version pin
      (RESOLVED in DOC-10 Resolved Decision 3: harness captures version on first
      run and flows it into DOC-2's manifest `version` field)

---

## Resolved Decisions

All six questions were resolved by the owner (owner-approved, 2026-06-08). Two
decisions (Q2, Q5) diverged from the draft and the body above has been revised
accordingly.

1. **Manifest serialization format — TOML.** *Confirmed as drafted.* The
   manifest is authored/serialized as **TOML**, matching the repo's
   hand-authored config convention, supporting comments for the self-documenting
   embedded BOM, and reusing the already-present `toml` workspace dep. JSON
   remains reserved for the DOC-1 contract wire layer.

2. **Scope of the per-project override — SYSTEM-level override ONLY (CHANGED).**
   *Diverges from the draft.* v1 ships with **no per-project manifest override**.
   The only override tier is the **SYSTEM-level** file
   (`~/.config/trusty-controller/manifest.toml`) layered over the embedded
   default; precedence reduces to `system override > embedded default`. The
   per-project `trusty-controller.toml` / pins-only override described in the
   original draft has been removed from the design (§2, §8, Cross-cutting).
   *Clarification:* this removes only a per-project *manifest* override —
   project-SCOPED operations and per-project state/identity still exist and are
   owned by DOC-3; the manifest is system-scope only.

3. **Remote/fetched manifest channel — deferred.** *Confirmed as drafted.* v1 =
   embedded default BOM + optional system override file only. A fetched
   "stable/beta channel" manifest (with its trust/signing surface) is a future
   extension, left to DOC-9 if/when it lands.

4. **Stack-version naming scheme — date-anchored `YYYY.MM-N`.** *Confirmed as
   drafted.* `stack_version` uses a date-anchored, monotonically-increasing
   label (e.g. `2026.06-1`), decoupled from any single crate's semver.

5. **Changelog conformance enforcement — best-effort parsing, NO CI gate
   (CHANGED).** *Diverges from the draft.* Keep a Changelog remains the
   expected/parse-target format, but headline extraction is **best-effort**: on a
   non-conforming or missing changelog the controller **degrades gracefully**
   (omits/skips headlines for that tool and surfaces a soft note) rather than
   failing. There is **no CI lint gate**; conformance is a convention, not an
   enforced gate (§5).

6. **`claude-mpm` package name, install mechanism, changelog URL, and version pin
   — resolved across DOC-6 and DOC-10.** *Confirmed.* The orchestrator entry
   uses `install = { source = "python", tool = "uv", package = "claude-mpm" }`
   (resolved in DOC-6 Resolved Decision 5) with changelog URL
   `https://raw.githubusercontent.com/bobmatnyc/claude-mpm/main/CHANGELOG.md`.
   The version placeholder (`version = "0.0.0"`) is replaced by DOC-10's
   discover-and-freeze mechanism (DOC-10 Resolved Decision 3): the harness
   installs claude-mpm unpinned, captures the resolved version, and freezes it
   as the BOM pin, flowing back into this field.
