# RFC: `.trusty-tools/` Per-Project State Convention

**Date:** 2026-05-29
**Status:** Draft — decisions needed from maintainer (see final section)
**Author:** Research analysis + user request
**Scope:** trusty-memory (urgent), trusty-search, tga, trusty-analyze, trusty-mpm

---

## 1. Summary

Introduce a `.trusty-tools/` directory at the project root that holds one
auto-generated YAML file per trusty ecosystem tool. Each tool reads and writes
its own `.trusty-tools/<tool>.yaml` file to store project-scoped linkage
state. Because this directory lives *inside* the project tree (alongside
`.git`), the project↔tool association survives any filesystem move or rename.

The immediate driver is **drive reorganisation relocation survival**: when a
user moves a project directory (e.g. `~/Projects/foo` → `~/Kemono/Projects/foo`),
the link between that project and its memory palace should not break.

---

## 2. Motivation

### 2.1 Drive Reorg Scenario

The user is reorganising their drive. After the move:

- Any global state that references the old *absolute* path silently orphans the
  project's memory, search index, analytics database, and related state.
- The user must manually reconnect tools — or, more commonly, unknowingly
  starts accumulating a second, orphaned set of state.

A per-project dotfile convention stores the linkage *inside* the project tree.
Moving the tree (as a unit, e.g. with `mv` or a GUI file manager) carries the
dotfiles along. The next time any trusty tool runs, it finds `.trusty-tools/<tool>.yaml`
at the project root and restores the association immediately, before any user
intervention.

### 2.2 Secondary Benefits

- **Multi-machine sharing via git commit** (opt-in): teammates and CI immediately
  get the correct tool configuration for a cloned repo.
- **Worktree isolation**: each git worktree is a distinct filesystem path and
  would naturally get its own `.trusty-tools/` state — avoiding the current
  ambiguity where two worktrees of the same repo share the same index or palace.
- **Single discovery contract**: all trusty tools use the same root-detection
  walk (`find_project_root` or equivalent), so they all agree on which directory
  is "the project" and where to look for the dotfile.

---

## 3. Current State Per Tool

### 3.1 trusty-memory (MOST IMPORTANT — the relocation gap)

**Code references:**
- `crates/trusty-memory/src/project_root.rs:9–17` — module documentation.
- `crates/trusty-memory/src/project_root.rs:95–103` — `project_slug_at()`:
  derives palace name from the **basename** of the project root found by
  walking up to `.git`, `Cargo.toml`, `pyproject.toml`, `package.json`,
  `go.mod`, or `.project-root`.
- `crates/trusty-memory/src/project_root.rs:63–83` — `find_project_root()`:
  the directory-walk implementation (canonicalises symlinks before walking).
- `crates/trusty-memory/src/lib.rs:136–142` — `resolve_palace_registry_dir()`:
  returns `<data_dir>/palaces/` when it exists, else `<data_dir>` itself,
  where `data_dir = resolve_data_dir("trusty-memory")` → macOS:
  `~/Library/Application Support/trusty-memory/` or XDG equivalent.
- `crates/trusty-common/src/memory_core/registry.rs:143,162–165` —
  `PalaceRegistry::open()` / `PalaceRegistry::create()`: palace data lives at
  `<data_root>/<palace_id>/palace.json` — i.e., globally, keyed purely by
  palace name string.

**How project↔palace association works today:**

1. When any MCP tool or CLI command runs, `project_slug()` walks up from CWD
   to the nearest project-root marker and slugifies the directory basename.
2. That slug becomes the palace *name*. Example: `/Users/masa/Projects/trusty-tools` →
   slug `trusty-tools` → palace stored at
   `~/Library/Application Support/trusty-memory/palaces/trusty-tools/palace.json`.
3. When creating a new palace, `validate_palace_name()` (`project_root.rs:139–166`)
   enforces that the requested name matches the derived slug — preventing accidental
   cross-project palace creation.

**What breaks on a directory move:**

The palace association is computed **entirely from CWD basename** at call time.
Palace data itself lives in a *global* OS data directory and is never aware of
the project's filesystem path. Therefore:

| Before move | After move |
|---|---|
| CWD = `/Volumes/X/Projects/trusty-tools` | CWD = `/Volumes/Y/Projects/trusty-tools` |
| slug = `trusty-tools` | slug = `trusty-tools` (same!) |
| palace = `trusty-tools` ✓ | palace = `trusty-tools` ✓ |

**When the move also renames the directory:**

| Before move | After move |
|---|---|
| CWD = `/Volumes/X/Projects/trusty-tools` | CWD = `/Volumes/Y/Repos/tt` |
| slug = `trusty-tools` | slug = `tt` (different!) |
| palace = `trusty-tools` | palace `tt` does not exist — empty! |

So the critical failure mode is **any rename of the project directory's basename**,
not just a path prefix change. The old palace (`trusty-tools`) is orphaned at
`~/Library/Application Support/trusty-memory/palaces/trusty-tools/` and never
automatically found again.

**No existing escape hatch for this case.** The only recovery is manually
passing `--palace trusty-tools` or renaming the palace via the API.

### 3.2 trusty-search (`.trusty-search/` colocated storage — #403)

**Code references:**
- `crates/trusty-search/src/detect.rs:43–76` — `detect_project()`: walks up
  looking for `.git` or `.trusty-search` marker file; falls back to CWD
  basename. `index_id` is derived from **directory basename** (same slug
  problem as trusty-memory).
- `crates/trusty-search/src/service/persistence.rs:117–132` — `PersistedIndex::colocated`:
  when `true`, all index data (`hnsw.usearch`, `index.redb`, etc.) lives at
  `<root_path>/.trusty-search/` instead of the global
  `<data_dir>/indexes/<id>/`.
- `crates/trusty-search/src/service/roots_registry.rs:7–17` —
  `roots.toml` at `<data_dir>/roots.toml`: the daemon's global registry of
  which project roots to scan at startup for colocated `.trusty-search/`
  directories.
- `crates/trusty-search/src/core/project_config.rs:31` — `PROJECT_CONFIG_FILENAME =
  ".trusty-search.yaml"`: user-authored per-project config for index name,
  path, and exclude patterns (committed to the repo).

**What already works:**
Issue #403 introduced `colocated = true` mode. When colocated:
- The heavy index data (HNSW graph, redb corpus) moves into `<root_path>/.trusty-search/`.
- Moving the project tree moves the data with it.
- The daemon re-discovers the index by scanning roots listed in `roots.toml`.

**What still breaks on move:**
- `roots.toml` stores **absolute paths**. After a move, the old root path in
  `roots.toml` no longer exists; the daemon does not scan the new path until
  it is registered with `trusty-search index <new-path>`.
- `PersistedIndex::root_path` in `indexes.toml` is also an absolute path —
  it must be updated after a move.
- The `index_id` (derived from directory basename by `detect_project()`) may
  change if the directory is renamed — the same slug problem as trusty-memory.

**Existing per-project markers:**
- `.trusty-search` (marker file / directory) — detected by `detect.rs`.
- `.trusty-search.yaml` — user-authored project config (committed).
- `.trusty-search/` (directory) — colocated heavy storage (gitignored).

### 3.3 trusty-git-analytics (tga)

**Code references:**
- `crates/trusty-git-analytics/src/main.rs:43–44` — `--config` CLI flag:
  `default_value = "config.yaml"`, resolved relative to CWD.
- `crates/trusty-git-analytics/src/main.rs:684–691` — `db_path` resolved via
  CLI; defaults to a path derived from config or CWD.

**Current state:** tga is entirely CWD-driven. The user runs `tga` from the
project root; `config.yaml` (in CWD) and the SQLite database (path in config
or CWD-relative default) are the project-scoped state. There is no global
registry. Moving the project tree along with `config.yaml` and the SQLite
file preserves all state — **tga is already relocatable by default**, assuming
`config.yaml` uses relative paths. The only risk is an absolute `db_path` in
`config.yaml`.

**What `.trusty-tools/tga.yaml` would add:** a standard location for tga to
advertise which `config.yaml` it uses and where its database lives — useful
for tooling that needs to discover tga state without reading the custom config.

### 3.4 trusty-analyze

**Code references:**
- `crates/trusty-analyze/src/main.rs:254` — wired as an MCP server receiving
  an `index_id` (trusty-search index) as a parameter.
- `crates/trusty-analyze/src/lang/detection.rs:226` — `detect_frameworks()`
  takes a `project_root: &Path` argument.

**Current state:** trusty-analyze has no independent per-project persistent
state. Its analysis results are keyed by `index_id`, stored in its own daemon
(HTTP), and serve the current trusty-search index. It does not maintain a
separate database that would break on a project move.

**What `.trusty-tools/trusty-analyze.yaml` might add:** the preferred
`index_id` to use when running analyze from this project root — avoiding
the need to pass `--index myproject` every time.

### 3.5 trusty-mpm

**Code references:**
- `crates/trusty-mpm/src/daemon/discover.rs:58–77` — daemon address discovery
  from `{data_dir}/http_addr`.
- `crates/trusty-mpm/src/core/session.rs:110` — `project_path` on sessions.

**Current state:** trusty-mpm's per-project state is minimal. The daemon is
machine-wide; sessions carry a `project_path` field but sessions are ephemeral.
No long-lived per-project state breaks on a directory move.

**What `.trusty-tools/trusty-mpm.yaml` might add:** project-specific agent
configuration defaults, preferred session templates — low urgency.

### 3.6 Existing Per-Project Conventions Already in Use

| Marker | Tool | Committed? | Relocatable? |
|---|---|---|---|
| `.git/` | git | yes | yes (marker, not state) |
| `.trusty-search` | trusty-search | optional | yes (marker file) |
| `.trusty-search.yaml` | trusty-search | yes | yes |
| `.trusty-search/` | trusty-search | no (gitignored) | yes (colocated data) |
| `config.yaml` | tga | yes | yes (relative paths) |
| `CLAUDE.md` | Claude Code | yes | yes |
| `.project-root` | trusty-memory | yes | yes (marker) |

`crates/trusty-memory/src/project_root.rs:44–51` — `PROJECT_MARKERS` already
includes `.project-root` as an escape hatch for project-root detection. This
is the natural extension point.

---

## 4. Proposed `.trusty-tools/` Convention

### 4.1 Directory Location and Discovery

```
<project-root>/
├── .git/
├── .trusty-tools/          ← new directory
│   ├── trusty-memory.yaml  ← written by trusty-memory
│   ├── trusty-search.yaml  ← written by trusty-search
│   ├── tga.yaml            ← written by tga (optional)
│   └── trusty-analyze.yaml ← written by trusty-analyze (optional)
└── ... (project files)
```

**Project root** is found by walking up from CWD until a directory containing
one of `PROJECT_MARKERS` (`.git`, `Cargo.toml`, `pyproject.toml`, `package.json`,
`go.mod`, `.project-root`) is found — reusing the exact logic in
`crates/trusty-memory/src/project_root.rs:63–83`. No new root-detection
mechanism is introduced.

The `.trusty-tools/` directory acts as its own marker: `PROJECT_MARKERS` should
be extended to include `.trusty-tools` so that a project with no other markers
but an established trusty linkage is still detectable. (This is a backward-compatible
addition since the list is checked in order and `.git` remains first.)

### 4.2 Schema Per Tool

All files follow the same envelope:

```yaml
# .trusty-tools/trusty-memory.yaml
# Auto-generated by trusty-memory. Do not edit palace_id by hand.
schema_version: 1
tool: trusty-memory
written_at: "2026-05-29T12:34:56Z"
palace_id: "trusty-tools"        # stable UUID or slug (see §4.2.1)
palace_name: "trusty-tools"      # human-readable name (current slug)
```

```yaml
# .trusty-tools/trusty-search.yaml
schema_version: 1
tool: trusty-search
written_at: "2026-05-29T12:34:56Z"
index_id: "trusty-tools"         # the index name in the search daemon
colocated: true                  # whether .trusty-search/ holds the data
```

```yaml
# .trusty-tools/tga.yaml
schema_version: 1
tool: tga
written_at: "2026-05-29T12:34:56Z"
config_file: "config.yaml"       # relative path to tga config (default: config.yaml)
db_file: "tga.db"                # relative path to analytics SQLite DB
```

```yaml
# .trusty-tools/trusty-analyze.yaml
schema_version: 1
tool: trusty-analyze
written_at: "2026-05-29T12:34:56Z"
index_id: "trusty-tools"         # trusty-search index used for analysis
```

#### 4.2.1 Palace ID Stability (Critical Design Point)

The current palace linkage is slug-only: `palace_id = slugify(dir_basename)`.
This is the root of the relocation problem when the directory is renamed.

Two options for a stable identifier in `.trusty-tools/trusty-memory.yaml`:

**Option A — Stable UUID stored in file.**
trusty-memory generates a UUID on first palace creation and writes it to
`.trusty-tools/trusty-memory.yaml`. On subsequent runs, it reads the UUID
from the file instead of re-deriving from the directory basename. The palace
is stored globally under its UUID, not its slug. The `palace_name` field holds
the human-readable slug for display.

Pros: fully relocation-stable including renames.
Cons: palace data must be keyed by UUID in `PalaceRegistry`, not by slug —
a schema migration for existing palaces.

**Option B — Slug stored as explicit declaration.**
trusty-memory reads the slug from `.trusty-tools/trusty-memory.yaml` instead
of re-deriving it from the current directory basename. The palace is still
stored globally under its slug. The file makes the slug explicit and portable.

Pros: no schema migration; existing palace data is immediately found after a
rename because the slug is read from the file, not computed from the new basename.
Cons: two machines that independently compute the slug will write the same value —
no collision risk. But if someone manually edits the slug in the file, the
linking breaks. Also does not help if the file itself is lost (new clone).

**Recommendation: Option B for v1 (fast ship), Option A as a follow-up.**
Option B is a 1–2 day implementation with no breaking changes. Option A is
the correct long-term answer but requires a storage migration.

### 4.3 Who Writes the File and When

Each tool is responsible for writing its own YAML file. The file is written
**lazily on first use** (not at install time):

- **trusty-memory**: writes `.trusty-tools/trusty-memory.yaml` when a palace
  is first created for the project (i.e., when `palace_create` succeeds). Also
  writes it when `memory_remember` is called and no file exists yet. Reads it
  on every `project_slug()` call if it exists (Option B: returns stored slug
  instead of re-deriving).
- **trusty-search**: writes `.trusty-tools/trusty-search.yaml` when
  `trusty-search index` runs successfully. Reads it when auto-detecting the
  index from CWD.
- **tga**: writes `.trusty-tools/tga.yaml` when `tga run` first executes in
  a project directory. Optional — low urgency.
- **trusty-analyze**: writes `.trusty-tools/trusty-analyze.yaml` when first
  invoked with a project root. Optional — low urgency.

### 4.4 Interplay with the Existing `.trusty-search/` Data Directory

trusty-search already has `.trusty-search/` for heavy index data (issue #403).
The relationship between this directory and the proposed `.trusty-tools/trusty-search.yaml`
must be clarified.

**Option 1 — Keep them separate (lightweight pointer):**

```
<project-root>/
├── .trusty-search/          # heavy data: hnsw.usearch, index.redb (gitignored)
├── .trusty-tools/
│   └── trusty-search.yaml  # lightweight: index_id + colocated flag (committable)
```

`.trusty-tools/trusty-search.yaml` holds only the *linkage* metadata (index ID,
colocated flag, written_at). The heavy data stays in `.trusty-search/`.
`roots.toml` would no longer be strictly needed for relocation — the tool reads
`.trusty-tools/trusty-search.yaml` to get the index_id, then finds the data in
`.trusty-search/` relative to that same root.

Pros: clear separation between committable linkage and gitignored data; consistent
with the convention used by other tools (trusty-memory's palace data is global,
but the linkage file is per-project).
Cons: an additional file to parse on startup; two dotfile directories to document.

**Option 2 — Consolidate into `.trusty-tools/trusty-search/`:**

```
<project-root>/
├── .trusty-tools/
│   ├── trusty-search.yaml           # linkage (committable)
│   └── trusty-search/               # heavy data (gitignored)
│       ├── hnsw.usearch
│       └── index.redb
└── trusty-memory.yaml               # no sub-directory needed for memory
```

All trusty-tools-related files live under `.trusty-tools/`. The gitignore entry
becomes `.trusty-tools/*/` or per-tool entries.

Pros: single top-level dotfile directory for all trusty ecosystem state.
Cons: breaking change for the existing `.trusty-search/` storage path (requires
`migrate-storage` equivalent); more complex gitignore rules.

**Recommendation: Option 1 for now.** The `.trusty-search/` colocated storage
was just shipped in #403 and asking users to migrate again immediately is
disruptive. Option 2 is the cleaner end-state and should be the target for a
future consolidation pass (e.g. v2 of this convention).

---

## 5. gitignore vs. Commit Decision

This is the central fork in the design. The two options have different
properties that matter for the "survive a drive reorg" goal.

### Option A — Commit `.trusty-tools/` to the repo

`.gitignore` does *not* include `.trusty-tools/`. All YAML files are committed
to the repository.

**Characteristics:**
- Project↔tool linkage survives a local move: yes (file is in the tree).
- Project↔tool linkage survives cloning to a new machine: yes (checked out
  from git).
- Teammates get the correct palace name / index_id automatically: yes.
- CI gets the correct configuration: yes.
- Risk: palace_id / index_id is embedded in git history. If the palace is
  deleted and recreated (Option A UUID scheme), the old UUID is stale but
  the file still references it.
- Noise: every developer's `git status` shows `.trusty-tools/` changes if
  tools update `written_at` on every run. Mitigated by only writing on
  creation, not on every use.

**Best for:** teams, CI, any project stored in a shared git repo.

### Option B — Gitignore `.trusty-tools/`

`.gitignore` includes `.trusty-tools/` (local-only).

**Characteristics:**
- Project↔tool linkage survives a local move: yes (file is in the tree, moves
  with the directory regardless of git tracking).
- Project↔tool linkage survives cloning to a new machine: no (file is not
  committed; each machine re-derives from scratch on first tool use).
- Teammates get the correct palace name: no (each developer gets their own
  local `.trusty-tools/` with potentially different values — but for palace
  linkage, that is often *correct*: each developer has their own palace).
- CI does not see the file: typically fine.

**Best for:** solo developers, local-only tools, cases where each developer
should have an independent memory palace.

### Hybrid — Commit only read-only metadata, gitignore writable state

A `.trusty-tools/.gitignore` (a gitignore file *inside* the directory) can
selectively commit some files and ignore others:

```gitignore
# .trusty-tools/.gitignore
# Generated per-tool linkage — commit these to share tool associations
# with teammates and across machines via git.
!trusty-memory.yaml
!trusty-search.yaml
!tga.yaml
!trusty-analyze.yaml
# Tool-managed local-only state (if any) — do not commit.
*.local.yaml
```

This approach lets the project maintainer opt in to committing per-tool YAML
files without any workspace-level gitignore changes.

**Verdict for "survive a drive reorg":** All three options survive a local
filesystem move because the files live inside the project tree. The decision
is about whether to additionally share the linkage across machines. This RFC
recommends the **commit** option (Option A) as the default, with a note that
solo users can gitignore the directory if they prefer local-only state.
The hybrid approach is a good middle ground if the maintainer wants per-tool
granularity.

---

## 6. Discovery and Loading

Each tool's startup sequence adds one step:

1. Call `find_project_root(cwd)` (existing function,
   `crates/trusty-memory/src/project_root.rs:63–83`).
2. If a project root is found, check for `<root>/.trusty-tools/<tool>.yaml`.
3. If the file exists and is valid, read the linkage from it (palace_id,
   index_id, etc.) rather than re-deriving from the directory basename.
4. If the file does not exist (first use), proceed with the legacy slug
   derivation and write the file after the first successful operation.

Parsing uses `serde_yml` (already used in
`crates/trusty-search/src/core/project_config.rs:80`). A missing file is
`Ok(None)`, never an error. A malformed file is `Err` with the path included
for context — the tool should warn and fall back to slug derivation, not abort.

---

## 7. Migration from Current Global Mappings

### 7.1 trusty-memory

No data migration is required for Option B (slug stored in file). The migration
is purely additive:

1. For every existing palace that can be matched to a project root (by scanning
   `DEFAULT_SEARCH_DIRS` in `crates/trusty-common/src/project_discovery.rs:31`),
   trusty-memory writes `.trusty-tools/trusty-memory.yaml` with
   `palace_id = <slug>`.
2. A one-time `trusty-memory setup --write-project-files` command (or equivalent)
   can backfill all discovered projects.
3. Tools read the file opportunistically; if absent, they fall back to the
   current slug derivation — no user-visible breakage.

### 7.2 trusty-search

For the `.trusty-tools/trusty-search.yaml` linkage file:

1. The existing `trusty-search migrate-storage` command (`main.rs:586`) already
   moves data into `.trusty-search/`. It should also write
   `.trusty-tools/trusty-search.yaml` as part of the migration.
2. `trusty-search index <path>` (new indexes) writes the file on first index.
3. `roots.toml` continues to function as the authoritative discovery registry;
   `.trusty-tools/trusty-search.yaml` is a supplementary linkage hint that
   lets tools find the index_id without querying the daemon.

### 7.3 tga

No migration needed; tga is already CWD-relative. The tool writes
`.trusty-tools/tga.yaml` on first run if absent.

---

## 8. Phasing

### Phase 1 — trusty-memory (urgent, for the imminent drive reorg)

**Target:** implement before the user's drive reorganisation.

1. Add a `read_project_file()` helper in
   `crates/trusty-memory/src/project_root.rs` that reads
   `.trusty-tools/trusty-memory.yaml` and returns the stored `palace_id`.
2. Modify `project_slug_at()` to call `read_project_file()` first; fall back
   to the current basename slugification if the file is absent.
3. Modify `palace_create` (and the first `memory_remember` call for a new
   palace) to write `.trusty-tools/trusty-memory.yaml` after success.
4. Add `PROJECT_MARKERS` entry for `.trusty-tools` (order: after `.git`, before
   `Cargo.toml`).
5. Unit tests: reading existing file, writing on create, fallback when absent,
   fallback when malformed, roundtrip through a move-rename scenario.

**Estimated scope:** 1 file new (YAML schema), ~50–80 lines in `project_root.rs`,
~20 lines in `tools.rs` / `service.rs`.

**gitignore decision required** (see §9).

### Phase 2 — trusty-search

**Target:** next sprint after Phase 1.

1. Extend `trusty-search index <path>` to write `.trusty-tools/trusty-search.yaml`.
2. Extend `detect_project()` to read the file if present (returns stored `index_id`
   rather than re-deriving from basename).
3. Extend `trusty-search migrate-storage` to write the file as part of the move.
4. Update `roots.toml` update logic: when a project is re-registered at a new
   path, scan for `.trusty-tools/trusty-search.yaml` to confirm the new root
   before registering.

### Phase 3 — Generalise (tga, trusty-analyze, trusty-mpm)

**Target:** future sprint. Lower urgency since these tools are already relocatable
or have no long-lived project state.

Add write/read hooks in each tool for their respective YAML files. Implement
a `trusty-tools doctor` scan that reports which projects have stale or missing
`.trusty-tools/` files.

---

## 9. Decisions Needed From Maintainer

**D1. gitignore vs. commit** (§5)
Should `.trusty-tools/` be committed to the repository (shared linkage) or
gitignored (local-only)? Recommended default: **commit**. Alternatives:
gitignore globally, or use a `.trusty-tools/.gitignore` hybrid for per-file
granularity. The drive-reorg goal is satisfied by either choice; committing
adds cross-machine and cross-teammate benefits.

**D2. Palace ID stability strategy** (§4.2.1)
Should Phase 1 use Option B (slug stored explicitly in file — no storage
migration, immediate fix) or Option A (stable UUID — better long-term,
requires PalaceRegistry schema migration)? Recommended: **Option B for v1,
plan Option A as a follow-up issue**.

**D3. `.trusty-search/` consolidation** (§4.4)
Should the new `.trusty-tools/trusty-search.yaml` coexist with the existing
`.trusty-search/` data directory (Option 1 — two dotfile dirs, separation of
concerns), or should a future version consolidate everything under
`.trusty-tools/trusty-search/` (Option 2 — single dotfile dir, requires a
second migration)? Recommended: **Option 1 for now**, targeting Option 2 in a
future consolidation pass.

**D4. Migration tooling for Phase 1**
Should trusty-memory get a one-shot `trusty-memory setup --write-project-files`
subcommand that backfills `.trusty-tools/trusty-memory.yaml` for all discovered
projects, or should the file be written lazily on first use only? Recommended:
**lazy write on first use** (simpler) plus an explicit opt-in backfill command
for users who want to pre-populate before the drive reorg.

**D5. Extend `PROJECT_MARKERS`**
Should `.trusty-tools` be added to `PROJECT_MARKERS` in
`crates/trusty-memory/src/project_root.rs:44–51` so that a directory containing
only `.trusty-tools/` (no `.git`, `Cargo.toml`, etc.) is still detected as a
project root? Recommended: **yes** — this makes the convention self-referential
and removes the dependency on other ecosystem markers being present.

**D6. `roots.toml` update strategy for trusty-search**
After a project move, `roots.toml` in the global data dir still references the
old path. Should trusty-search auto-heal `roots.toml` by scanning
`.trusty-tools/trusty-search.yaml` in any directory the user `cd`s into (daemon
side), or should it require an explicit `trusty-search index <new-path>` to
re-register? Recommended: **require explicit re-registration** for v1
(simpler, safer); add auto-heal in Phase 2.

---

## Appendix: File and Code Reference Summary

| Citation | Location | Relevance |
|---|---|---|
| `project_root.rs:9–17` | `crates/trusty-memory/src/` | Module overview of palace slug derivation |
| `project_root.rs:44–51` | `crates/trusty-memory/src/` | `PROJECT_MARKERS` constant — extension point |
| `project_root.rs:63–83` | `crates/trusty-memory/src/` | `find_project_root()` walk — reuse for `.trusty-tools` discovery |
| `project_root.rs:95–103` | `crates/trusty-memory/src/` | `project_slug_at()` — currently reads only from basename |
| `project_root.rs:139–166` | `crates/trusty-memory/src/` | `validate_palace_name()` — enforcement gate |
| `lib.rs:136–142` | `crates/trusty-memory/src/` | `resolve_palace_registry_dir()` — global palace storage |
| `registry.rs:143,162–165` | `crates/trusty-common/src/memory_core/` | Palace data keyed by `PalaceId` string in global dir |
| `detect.rs:43–76` | `crates/trusty-search/src/` | `detect_project()` — basename-derived `index_id` |
| `persistence.rs:117–132` | `crates/trusty-search/src/service/` | `colocated` flag — issue #403 colocated storage |
| `roots_registry.rs:7–17` | `crates/trusty-search/src/service/` | `roots.toml` — global list of project roots |
| `project_config.rs:31` | `crates/trusty-search/src/core/` | `.trusty-search.yaml` — existing per-project config |
| `main.rs:43–44` | `crates/trusty-git-analytics/src/` | tga `config.yaml` — CWD-relative, already relocatable |
| `lib.rs:307–346` | `crates/trusty-common/src/` | `resolve_data_dir()` — global OS data dir for all tools |
| `project_discovery.rs:31` | `crates/trusty-common/src/` | `DEFAULT_SEARCH_DIRS` — home-dir project scan roots |
