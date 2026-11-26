# DOC-93 — trusty-review repo-named review skills

**Status:** Draft
**Subsystem:** trusty-review — `.trusty-review.toml` handling (`config/`), system-prompt assembly (`pipeline/prompt.rs`), the contents-at-ref reader (`integrations/context/contents_at_ref.rs`), the CLI `run` command, the local MCP `review_pr` tool, and the webhook drain
**Owner:** owner
**Last-updated:** 2026-11-26
**Spec ID:** `SPEC-REVSKILL-01~draft` … `SPEC-REVSKILL-11~draft` (DOC-93)
**Builds on:** [DOC-38](./spec-linked-documentation.md#SPEC-SLD-01~draft) (spec grammar). Extends the per-project review-template mechanism of #2995 (`[review] template`).

Requirement ids are `RS-n`. "MUST" marks a testable requirement. "Design intent" marks a statement no code in trusty-review implements or fixes yet. Every other statement describes code that exists today.

## 1. Purpose {#SPEC-REVSKILL-01~draft}

**ID:** SPEC-REVSKILL-01~draft

- RS-1. A repository names the review skills that trusty-review applies when it reviews that repository's pull requests.
- RS-2. Each skill is a file in the reviewed repository. The repository owns its text and its history.
- RS-3. A skill adds checks to a review. It never changes the verdict rules or the output schema.
- RS-4. A skill never makes a review fail.

Today trusty-review reads `.trusty-review.toml` from the git root of the checkout. The file selects a voice package, a principles flag and one named review template. The template text ships with trusty-review, not with the repository. No code in the crate loads a skill.

## 2. Configuration {#SPEC-REVSKILL-02~draft}

**ID:** SPEC-REVSKILL-02~draft

A skill is one `[[review.skills]]` entry in `.trusty-review.toml`. The entries sit beside the existing `[review] template` key.

| Key | Type | Required | Meaning |
|---|---|---|---|
| `name` | string | yes | Names the skill. Resolves to `.claude/skills/<name>/SKILL.md`. |
| `apply_to` | list of strings | no | Path globs. The skill applies when any changed file of the pull request matches one glob. |

```toml
[review]
template = "strict-security"

[[review.skills]]
name = "rust-error-handling"
apply_to = ["crates/**/*.rs"]

[[review.skills]]
name = "release-notes"
# No apply_to: the skill applies to the whole pull request.
```

Rules:

- RS-5. `name` MUST be a bare identifier. The check is `is_valid_identifier` in `crate::identifier`: the string is non-empty, and every character is an ASCII letter, an ASCII digit, `-` or `_`. A `/`, `\`, `.`, space or any other character fails. The rule guarantees that `name` is exactly one path component, so it cannot traverse or become absolute.
- RS-6. A missing `name`, or a `name` that fails RS-5, skips that entry. The reason is named in the review (§6).
- RS-7. With no `apply_to`, or an empty one, the skill applies to the whole pull request. (Design intent for the empty list.)
- RS-8. `apply_to` globs match repository-relative paths with `/` separators. `*` matches within one path segment, `**` matches across segments, and `?` matches one character. A glob that does not parse skips that entry. (Design intent: the glob dialect.)
- RS-9. Entries load in file order. A second entry with an already-seen `name` is skipped as a duplicate. (Design intent.)
- RS-10. A pull request author can edit `.trusty-review.toml` on the head branch. The configuration is therefore read at the same revision as the skill files (§5).

## 3. Skill file {#SPEC-REVSKILL-03~draft}

**ID:** SPEC-REVSKILL-03~draft

A skill is `.claude/skills/<name>/SKILL.md` in the reviewed repository. The file follows the [Agent Skills format](https://agentskills.io): YAML frontmatter, then a Markdown body.

```markdown
---
name: rust-error-handling
description: Flags new unwrap and expect calls in non-test Rust code during review.
---

# Rust error handling

When a changed file under `crates/` adds `unwrap()` or `expect()` outside a test
module, report one finding at the call site. Suggest `?` with a typed error.
Do not report calls inside `#[cfg(test)]` modules or `tests/` directories.
```

Validation. Each failure skips the skill:

- RS-11. The file starts with a frontmatter block delimited by `---` lines, and the block parses as YAML.
- RS-12. The frontmatter has a non-empty `name` and a non-empty `description`.
- RS-13. The frontmatter `name` equals the configured `<name>` exactly. This is the strict reading. The Agent Skills format requires a skill name to match its directory name. This spec enforces that match for the exact configured string, and it does not apply the Agent Skills character set (lowercase letters, digits, hyphens) on top of RS-5.
- RS-14. The file is UTF-8 text.
- RS-15. The body, the text after the closing `---`, is non-empty. (Design intent.)

Other frontmatter keys are ignored. Files beside `SKILL.md` in the skill directory are never read.

## 4. Source {#SPEC-REVSKILL-04~draft}

**ID:** SPEC-REVSKILL-04~draft

- RS-16. Skills come only from the reviewed repository. trusty-review reads no installed skill (`~/.claude/skills`), no framework or bundled skill and no remote skill source for this feature.
- RS-17. Resolution never leaves the repository. The path is always `.claude/skills/<name>/SKILL.md`, built from a name that passed RS-5. A path of any other shape is never requested.
- RS-18. A symlink is never followed. A skill file, or any directory on its path, that is a symlink is skipped. The Contents API returns no inline content for a symlink, and the reader reports `no inline content`; the local reader rejects a symlink explicitly. (Design intent for the local reader.)
- RS-19. A path read through the Contents API passes `validate_repo_path` first: at most 200 characters, only `[A-Za-z0-9._/-]`, no leading `/`, no empty, `.` or `..` segment.

## 5. Revision {#SPEC-REVSKILL-05~draft}

**ID:** SPEC-REVSKILL-05~draft

- RS-20. trusty-review reads `.trusty-review.toml` skill entries and every `SKILL.md` at the base commit of the pull request, the tip of the target branch the pull request merges into. It never reads them from the head commit or the working tree. A pull request cannot change the skill that reviews it.
- RS-21. A pull request from a fork loads no repository skill. The fork test is `head_is_fork`: the head label owner and the base label owner differ, or a label has no `owner:` prefix. The review states that repository skills were not loaded and why.
- RS-22. A same-repository pull request loads skills at the base commit.

Reading the base commit rather than the head means the skills are the ones the target branch already accepted. A skill added by the pull request itself takes effect for later pull requests, after it merges.

## 6. Limits and skipped skills {#SPEC-REVSKILL-06~draft}

**ID:** SPEC-REVSKILL-06~draft

- RS-23. Each skill file has a size cap of `<per-skill cap>` bytes.
- RS-24. All loaded skills together have a size cap of `<total cap>` bytes. Skills count against it in configuration order. A skill that would exceed it is skipped, and so are all later skills that do not fit.
- RS-25. A skill is skipped, not loaded, when it is missing, invalid, over a cap, or does not match. Skip reasons:

| Reason | Cause |
|---|---|
| `invalid name` | RS-5 fails, or `name` is absent. |
| `duplicate` | The name already appeared. |
| `invalid glob` | An `apply_to` entry does not parse. |
| `not applicable` | No changed file matches `apply_to`. |
| `not found` | No `SKILL.md` at that path at the base commit. |
| `unreadable` | A read error, a directory, a symlink, an oversized API file, or text that is not UTF-8. |
| `invalid frontmatter` | RS-11, RS-12, RS-13 or RS-15 fails. |
| `over per-skill cap` | The file exceeds `<per-skill cap>`. |
| `over total cap` | Adding the skill exceeds `<total cap>`. |

- RS-26. The review body names each skipped skill with its name and reason. A `not applicable` skill is listed as not applicable, not as a fault.
- RS-27. A skill never makes the review fail. Every skill error is a skip, and the review completes with the skills that loaded.

The caps are not fixed by code today. The values are design intent and are set in the implementation, not in this spec.

## 7. Prompt placement {#SPEC-REVSKILL-07~draft}

**ID:** SPEC-REVSKILL-07~draft

The system prompt is layered today: the stock rubric, then principles, then voice, then the named review template. The template only appends; no layer replaces the rubric.

- RS-28. Loaded skills append after the review template as one labelled, fenced block. Each skill is a labelled sub-block carrying its name and the body of its `SKILL.md`, without the frontmatter.
- RS-29. The block opens with a fixed line that says the text is repository guidance, treated as untrusted, that it may add review checks, and that the verdict rules and the output schema above it stay authoritative.
- RS-30. Skill text cannot change the verdict rules or the output schema. The structured-output schema is enforced by the call, not by the prompt, so a skill cannot alter it.
- RS-31. Skills load in configuration order, so the prompt is deterministic for a given repository state. The block is the same for every pull request that applies the same skills, so it stays part of a stable prompt prefix.
- RS-32. With no loaded skill, the system prompt is byte-identical to today's.

## 8. Review record {#SPEC-REVSKILL-08~draft}

**ID:** SPEC-REVSKILL-08~draft

- RS-33. The review record lists each loaded skill's `name` and content hash.
- RS-34. The content hash is the SHA-256 of the exact `SKILL.md` bytes read, in lowercase hex.
- RS-35. The record also lists each skipped skill with its reason (§6), and whether repository skills were withheld for a fork pull request.
- RS-36. The record fields are additive and optional. A consumer that ignores them keeps working. A review with no configured skill omits them.

The hash lets a reader compare two reviews of different repositories, or of one repository over time, and tell whether the same skill text applied.

## 9. Surfaces {#SPEC-REVSKILL-09~draft}

**ID:** SPEC-REVSKILL-09~draft

- RS-37. The CLI `run` command and the local MCP `review_pr` tool read from the local checkout. The configuration is found at the git root of the working directory. Skill files and the configuration are read from the base commit through git, not from the working tree. (Design intent for the git read.)
- RS-38. The hosted webhook path has no checkout. It reads `.trusty-review.toml` and each `SKILL.md` at the base commit through the contents-at-ref reader (`DocFetcher`), which validates the path and requires a full commit SHA before any request.
- RS-39. On the hosted path, an unreadable file is a skip with reason `unreadable`. A fork pull request makes no skill request at all (RS-21).
- RS-40. API changes are additive and optional. The existing `[review]` keys, CLI flags and MCP arguments keep their meaning. No new flag or argument is required to use the feature; the configuration file drives it.

## 10. Acceptance tests {#SPEC-REVSKILL-10~draft}

**ID:** SPEC-REVSKILL-10~draft

- (a) With a configured skill, the review prompt contains that skill's `SKILL.md` text as it stands at the base commit, not the head commit. A real pull request, reviewed once with the skill and once without it, yields different finding sets.
- (b) A missing, invalid or over-cap skill is skipped. The review completes, and the review body names the skill with the reason.
- (c) A pull request from a fork gets no repository skill text in its prompt, and the review says so.
- (d) The review record lists the name and content hash of each loaded skill.

Unit coverage owed by the implementation: name validation (RS-5), frontmatter validation (RS-11 to RS-13), both caps, `apply_to` matching, symlink and traversal rejection, a prompt golden test showing block placement (RS-28), and the base-commit read on both the local and the hosted path.

## 11. Out of scope {#SPEC-REVSKILL-11~draft}

**ID:** SPEC-REVSKILL-11~draft

- Skill sources outside the reviewed repository: installed, framework, bundled or remote.
- Loading a skill on demand during a review. The review is one structured-output call with no tool loop.
- Replacing or removing the stock rubric.
- Reading any file in a skill directory other than `SKILL.md`.
