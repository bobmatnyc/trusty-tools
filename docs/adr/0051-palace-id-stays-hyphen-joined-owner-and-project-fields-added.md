# 0051. Palace identifiers stay hyphen-joined; `palace.json` gains structured `owner`/`project` fields

- **Status:** Accepted
- **Date:** 2026-08-16
- **Scope:** crate `trusty-common` (`memory_core::palace::PalaceId`, `Palace`
  record, `palace_id::owner_repo_from_git_remote`), crate `trusty-memory`
  (`palace_ops::handle_palace_create`, `service::core::create_palace`,
  `tools::helpers::resolve_palace`)
- **Reversibility Cost:** Low — no storage token changes and no migration
  runs; the fields are additive and optional, so removing them later costs
  nothing already written to disk
- **Decision Drivers:** owner ruling, 2026-08-16 conversation; a proposed
  `<owner>:<project>` separator rejected on evidence that the shared slug
  sanitizer silently strips colons; the `#4088` yank precedent for a required
  new public field on a 0.y.z crate
- **Supersedes / Superseded by:** Amends [ADR-0012](0012-per-instance-guid-and-marker-file-identity.md) §1.

## Context

ADR-0012 §1 states that a trusty-memory palace slug derives from
`git_origin + committed_pin` and "is shared across all worktrees and branches
of the same repo." That decision fixed the derivation but left the join
character between the owner and repo segments implicit — the concrete slugs
it discusses (`bobmatnyc-trusty-tools`) simply use a hyphen, without ADR-0012
ever weighing a separator alternative. This ADR makes that choice explicit and
adds structured fields so consumers stop needing to infer owner/project from
the joined string at all.

A colon separator (`<owner>:<project>`) was proposed this session and rejected
on the following evidence:

- **The shared slug sanitizer strips colons rather than converting them.**
  `slugify_string` (`crates/trusty-common/src/slug.rs:36-64`) maps only `_`,
  `-`, space, and tab to `-`; every other character — colon included — is
  dropped, not substituted. `TRUSTY_MEMORY_PALACE=bobmatnyc:trusty-tools`
  would yield `bobmatnyctrusty-tools`: owner and project silently concatenated
  with no separator at all, which is worse than the ambiguity it would have
  tried to fix.
- **The id becomes a directory name verbatim.**
  `PalaceRegistry::create_palace` does `data_root.join(palace.id.as_str())`
  (`crates/trusty-common/src/memory_core/registry.rs:636`), and `PalaceId`
  (`crates/trusty-common/src/memory_core/palace.rs:26-28`) is a bare `String`
  wrapper with no sanitization of its own. Whatever character scheme the id
  uses reaches the filesystem unchanged.
- **The three creation paths disagree on legal characters today, and a colon
  would widen that gap.** MCP `palace_create`
  (`crates/trusty-memory/src/tools/palace_ops.rs:28-49,91`) enforces
  `[a-z0-9][a-z0-9-]{0,62}` and rejects a colon outright. HTTP
  `POST /api/v1/palaces` (`crates/trusty-memory/src/service/core.rs:196-249`)
  has no charset gate at all, so with `force=true` it would happily create a
  colon-bearing directory. The MCP read path
  (`crates/trusty-memory/src/tools/helpers.rs:404-416`) takes the caller's
  string verbatim, with no validation on the read side either.
- **Pin values are never re-slugified.** The pin-file read path in
  `palace_resolve.rs:87-88` (in `trusty-common`; not yet merged to `main` as
  of this writing — see "What could not be verified" below) passes a
  hand-written `palace:` value through to `data_root.join` with nothing in
  between to catch it.
- **On macOS, a colon is legal at the POSIX/APFS layer but Finder renders it
  as a path separator (`/`).** A user who opens the palace directory in
  Finder would see what looks like a nested directory that does not exist on
  disk — a purely cosmetic hazard, but one with no upside to offset it.

The hyphen has a real, acknowledged ambiguity of its own: a repo name can
itself contain a hyphen, so `<owner>-<project>` cannot always be split back
into its two parts by looking for the first or last `-`.
`repo_slug_from_git_remote`'s doc comment
(`crates/trusty-common/src/palace_id.rs:101-110`) names this directly, and its
test `repo_slug_https_with_owner`
(`crates/trusty-common/src/palace_id.rs:462-470`) demonstrates it concretely:
`gitlab.com/acme/team/cool-widget` cannot be recovered from a joined
`acme-cool-widget` string, because nothing marks where `acme` ends and
`cool-widget` begins.

That ambiguity is tolerable because no code anywhere attempts the split today.
Every consumer either re-derives the pieces from the git remote directly
(`owner_repo_from_git_remote`, `repo_slug_from_git_remote`) or treats the
joined id as an opaque token. The structured fields this ADR adds remove the
need for that split going forward, rather than fixing a live bug — the
ambiguity has not caused an incident, and this decision does not claim it has.

**Which document actually carries the palace-identity invariant.** A source
comment in `trusty-common` (currently in a branch not yet merged; see below)
cites "ADR-0050 §7" as authority for "a worktree and its main checkout
resolve to the same palace." That citation is wrong on inspection: ADR-0050's
scope line (`docs/adr/0050-colocated-path-tied-identity-with-delta-indexed-worktree-facets.md:5-7`)
is `crate: trusty-search`, index identity — a different subsystem — and the
file has no §7; its sections are Context, Decision, Consequences, Open
Questions, and Related Decisions. The invariant the comment is trying to cite
is ADR-0012 §1, restated by this ADR. A separate PR is fixing the miscited
comments; this ADR gives that fix something correct to point at.

**The invariant does not stop at "a worktree and its main checkout."**
ADR-0012 §1's derivation is `git_origin + committed_pin`, not physical path,
so it is checkout-location-independent by construction. A repo can have all
three of: the user's own clone (what this ADR calls the "main checkout"
above), the tm-managed checkout at `<repos_root>/<owner>/<repo>/` — a
separate clone that ADR-0030 explicitly distinguishes from the user's own
clone, not a git worktree of it — and any number of agent worktrees nested
beneath either. All three resolve to one palace as long as `git_origin` and
the committed pin agree between them, because none of the three changes what
the derivation reads.

## Decision

We will:

1. **Keep the hyphen as the join character for palace identifiers.** No
   change to how `owner_repo_from_git_remote` constructs
   `<owner>-<repo>` slugs, and no change to any existing palace's on-disk
   directory name.
2. **Add optional, structured `owner: Option<String>` and
   `project: Option<String>` fields to the `Palace` record persisted in
   `palace.json`.** Consumers that need the owner or project individually
   read these fields directly instead of parsing the joined `id`.
3. **Never backfill these fields by splitting the existing joined id.** That
   split is exactly the ambiguity this decision exists to route around — see
   the `cool-widget` example above. A palace created before this decision, or
   created without enough information to populate the fields, carries
   `owner: None, project: None`. A reader that wants them re-derives from the
   git remote (the same source `owner_repo_from_git_remote` already uses) or
   treats them as unknown. Never guess by splitting the string.

   `owner: None, project: None` carries two distinct meanings, and a reader
   must not conflate them. **Unknown** is the case above — a git-derived
   palace that predates these fields, or was created before enough remote
   information existed to populate them; `owner_repo_from_git_remote` can
   still populate it later, so re-deriving from the git remote is meaningful.
   **Inapplicable** is a different case: a palace is not necessarily a
   software project. `trusty-agents` creates one palace per ASSISTANT (owner
   ruling, 2026-08-16 conversation), and an assistant-scoped palace has no
   git remote, no owner, and no repo — nothing to derive. For an inapplicable
   palace, `None` is permanent by design, not a gap; no future migration
   should attempt to derive `owner`/`project` for it. Code reading these
   fields must not treat `None` as an instruction to go compute a value — it
   must first tell unknown from inapplicable (in practice: whether the
   palace's id came from `owner_repo_from_git_remote` at all) before deciding
   whether re-deriving even makes sense.
4. **Mark the `Palace` record `#[non_exhaustive]` and keep the two new
   fields `Option`.** `trusty-common` is published to crates.io; a required
   new public field on a 0.y.z crate is a MINOR-position SemVer break under
   Cargo's 0.x rule. `trusty-common` 0.22.5 shipped exactly that mistake on a
   patch bump and cost `trusty-analyze` 0.7.3 a yank (#4088). Optional fields
   plus `#[non_exhaustive]` on the struct keep this addition non-breaking by
   construction. `scripts/preflight-publish.sh` CHECK 5 is the gate that
   would have caught #4088's shape of break; it runs at release time via
   `cargo-semver-checks` against the crate's latest crates.io release and does
   not run on PRs — so passing CI on this ADR's implementation PR is not
   evidence the release-time gate has been satisfied. The two-reasons-for-
   absence split in item 3 above sharpens why `Option` is correct here, not
   just convenient: an assistant-scoped palace has no value to put in
   `owner`/`project` at all, so a required field would have no legitimate
   content to hold for a whole class of palace, independently of the #4088
   SemVer argument.
5. **Leave the storage token unchanged.** No existing palace directory moves,
   and no migration code runs as part of this decision.
   `update_palace_name` (`crates/trusty-memory/src/service/core.rs:348-379`)
   already establishes the pattern this decision follows: it rewrites only
   the display `name` field via `PalaceStore::save_palace`'s atomic
   tmp-file-then-rename, and explicitly leaves `id` and `data_dir` untouched.
   No palace-rename machinery exists to move a directory even if a future
   change wanted to — issue #98 narrowed `migrations.rs`
   (`crates/trusty-memory/src/commands/migrations.rs:1-30`) to a
   display-name-only rename specifically because "no public rename API, no
   HTTP endpoint, no MCP tool" exists.

This decision does not touch what ADR-0012 §1 already settled: a committed
pin always wins over derivation, and existing palaces are never orphaned by a
change to the derivation logic (#1224). This repo's own
`.trusty-tools/trusty-memory.yaml` demonstrates that ruling in production —
it pins the `trusty-tools` palace, with a 2026-05-30 note explaining the pin
deliberately overrides what git-path derivation would otherwise produce
(`bobmatnyc-trusty-tools`), specifically to avoid orphaning existing memories.

## Consequences

**Easier:**

- A consumer that wants "just the owner" or "just the project" reads a field
  instead of parsing a string that cannot always be parsed correctly.
- The `<owner>:<project>` colon proposal is closed out with a documented
  reason, so a future reader does not have to re-derive why it was rejected.

**Harder / unchanged:**

- Every palace created before this decision ships has `owner: None,
  project: None` until something re-derives and writes them — there is no
  bulk backfill step, by design (see Decision item 3).
- Two other identity surfaces are explicitly **out of scope** for this
  decision, not silently swept in:
  - `tm register`'s project alias
    (`crates/trusty-mpm/src/bin/tm/commands/register_args.rs:363-370`) is a
    separate namespace with its own hyphen constraint, derived through
    `owner_repo_from_git_remote` but not touched by this ADR's structured
    fields.
  - The basename-only slug parser in
    `crates/trusty-memory/src/project_root/validation.rs:38-65` (backed by
    `crates/trusty-memory/src/project_root/pin_file.rs:76-167`) has no git
    awareness and structurally cannot agree with the git-derived form.
    Unifying the two parsers is known follow-up work, not part of this
    decision.
- `slug.rs`'s silent-strip behavior is unchanged and remains a standing
  hazard. `slugify_string` drops any character it does not recognize instead
  of rejecting it — this is what made the colon proposal actively dangerous
  rather than merely inconvenient, and it will make the same class of mistake
  possible again for any future separator or format change that does not
  independently check the sanitizer's behavior first.
- **A palace is not necessarily a software project.** `owner: None,
  project: None` on an assistant-scoped palace (one `trusty-agents` palace
  per assistant) is not an artifact of the fields being unpopulated yet — the
  palace has no git remote to derive them from. A consumer or future
  migration that reads `None` and assumes "needs deriving" is wrong for this
  class of palace; see Decision item 3.

## What could not be verified

The source-comment miscitation of "ADR-0050 §7"
(`crates/trusty-common/src/palace_resolve.rs:27,355` and
`crates/trusty-common/src/palace_resolve_tests.rs:294,298`, per the task that
produced this ADR) lives in a `trusty-common` file
(`palace_resolve.rs`) that does not yet exist on `origin/main` — it is
present only in a concurrently in-flight worktree
(`.claude/worktrees/agent-ac95537dbaa58eaa4`, branch
`worktree-agent-ac95537dbaa58eaa4`). A read-only grep of that worktree's copy
of both files for the string `ADR-0050` found no matches at the time this ADR
was written, so the miscitation may already be corrected there, or the exact
line numbers may have shifted since the task describing it was written. This
ADR's claim about which document actually carries the invariant (ADR-0012 §1,
not ADR-0050) does not depend on the miscite's current line numbers or
correction status, and was verified independently against both ADR files.

## Related Decisions

Vetted against prior ADRs on 2026-08-16 (swept `docs/adr/INDEX.md` and every
ADR file in `docs/adr/`):

- **ADR-0012 (Per-instance GUID and marker-file canonical identity):**
  Extends — §1's `git_origin + committed_pin` derivation and its
  shared-across-worktrees invariant are unchanged. This ADR makes the
  previously implicit hyphen join explicit and adds structured fields
  alongside the joined id; it does not alter how the id itself is derived.
- **ADR-0050 (Colocated, path-tied index identity with delta-indexed
  worktree facets):** Consistent, not extended — ADR-0050's scope is
  `trusty-search` index identity, a different subsystem from trusty-memory
  palace identity. This ADR references ADR-0050 only to correct a miscitation
  that pointed to it in error; ADR-0050 itself is untouched.
- **ADR-0026 (Credential grants do not survive delegation):** Consistent —
  no interaction; this decision does not touch credential or delegation
  handling.
- **ADR-0044 / ADR-0048 / ADR-0049 (main-checkout write boundary and worktree
  grants):** Consistent — this ADR is authored from a dispatched agent's own
  granted worktree, per those decisions, and makes no main-checkout writes.
- All other ADRs in `docs/adr/INDEX.md`: no overlap with palace-identifier
  format or `trusty-memory`'s `Palace` record found.
