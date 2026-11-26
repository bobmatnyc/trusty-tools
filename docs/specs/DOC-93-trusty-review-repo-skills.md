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

Today trusty-review reads `.trusty-review.toml` from the git root of the checkout. The file selects a voice package, a principles flag and one named review template. The template text ships with trusty-review, not with the repository. The CLI `run` command and the local MCP `review_pr` tool load the skills §2 to §9 describe. The hosted webhook and HTTP service paths load no skill today (§9).

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
- RS-6. A missing `name`, a `name` that is not a string, or a `name` that fails RS-5, skips that entry with kind `invalid_name`. The reason is named in the review (§6).
- RS-7. With no `apply_to`, or an empty one, the skill applies to the whole pull request.
- RS-8. `apply_to` globs are `globset` globs with a literal separator. They match repository-relative paths with `/` separators. `*` and `?` stay within one path segment, and `**` crosses segments. A glob that does not parse skips that entry with kind `invalid_glob`.
- RS-9. Entries load in file order. The first entry with a given `name` wins. Each later entry with the same `name` is skipped with kind `duplicate`.
- RS-10. A pull request author can edit `.trusty-review.toml` on the head branch. The configuration is therefore read at the same revision as the skill files (§5).
- RS-41. An entry with a key other than `name` and `apply_to`, or with an `apply_to` that is not a list of strings, is skipped with kind `malformed_entry`. A `review.skills` value that is not a list of entries is one `malformed_entry` skip. A skipped entry is named by its `name`, or by `entry <n>` (1-based) when it has none.
- RS-42. A `.trusty-review.toml` at the base commit that is not valid TOML loads no skill. The review records one skip of kind `config_unreadable` (RS-45) and completes.

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
- RS-15. The body, the text after the closing `---`, is non-empty after trimming.

Other frontmatter keys are ignored. Files beside `SKILL.md` in the skill directory are never read.

- RS-43. A skill whose text could read as a fence marker is skipped with kind `unreadable`. The word `REPO-SKILL` and the `<<<`/`>>>` runs delimit skills in the prompt (§7). The check normalises the text, then tests it:
  - Normalisation: fullwidth characters become ASCII, bracket lookalikes become `<` or `>`, zero-width and other format characters are dropped, and the result is lowercased.
  - Any `<<<` or `>>>` run in the normalised text is rejected.
  - The text is rejected when it contains `reposkill` once every non-alphanumeric character is removed. `repo-skill`, `REPO_SKILL` and a hyphen lookalike all match.
  - Known false positive: plain text such as "repo skill" is refused.

## 4. Source {#SPEC-REVSKILL-04~draft}

**ID:** SPEC-REVSKILL-04~draft

- RS-16. Skills come only from the reviewed repository. trusty-review reads no installed skill (`~/.claude/skills`), no framework or bundled skill and no remote skill source for this feature.
- RS-17. Resolution never leaves the repository. The path is always `.claude/skills/<name>/SKILL.md`, built from a name that passed RS-5. A path of any other shape is never requested.
- RS-18. A symlink is never followed. A skill file with git mode 120000, or any symlinked directory on its path, is skipped with kind `unreadable`. Only a regular file (git mode 100644 or 100755) loads.
- RS-19. A path read through the Contents API (the hosted path, §9) passes `validate_repo_path` first: at most 200 characters, only `[A-Za-z0-9._/-]`, no leading `/`, no empty, `.` or `..` segment.

## 5. Revision {#SPEC-REVSKILL-05~draft}

**ID:** SPEC-REVSKILL-05~draft

- RS-20. trusty-review reads `.trusty-review.toml` skill entries and every `SKILL.md` at the base commit of the pull request, the tip of the target branch the pull request merges into. It never reads them from the head commit or the working tree. A pull request cannot change the skill that reviews it.
- RS-21. A pull request from a fork loads no repository skill. The fork test is `head_is_fork`: the head label owner and the base label owner differ, or a label has no `owner:` prefix. A fork pull request with no configured skill is silent: no `repo_skills` record and no notice (RS-36). A fork pull request records `repo_skills` with `fork: true` and the base sha only when the base configuration lists skills or cannot be read. When the base commit is readable and lists skills, every entry is listed as skipped with kind `fork`. When the base configuration is unreadable (RS-45), the record carries `fork: true` and the entry is skipped with kind `config_unreadable`. The review body then says: "none loaded. This PR comes from a fork, so repository skills were not loaded and no repository skill text was used."
- RS-22. A same-repository pull request loads skills at the base commit.
- RS-44. A review of a range, `--base <ref>`, reads the configuration and skills at the commit `<ref>` resolves to in the repository root. A range review has no fork signal, so it never sets `fork`. An empty `<ref>`, or one starting with `-`, is refused and no skill loads.
- RS-45. Reading the base commit needs a local checkout that holds it. A GitHub pull request review with no checkout, or whose base commit is not fetched, reads nothing. So does a review of a diff file or standard input. The configuration cannot be read, so no skill loads and no record is written. A fork pull request is the exception: it records `fork: true` and the base sha, and the fork notice appears (RS-21). A configuration file that is present at the base commit but cannot be read (a git error or timeout, over its size cap, not a regular file, not UTF-8, or not valid TOML) is one skip of kind `config_unreadable`, named `.trusty-review.toml`, with the cause as its reason. An absent file, or one with no or an empty `[[review.skills]]` list, is silent. A review given an explicit request list records every request as skipped with kind `no_base`.

Reading the base commit rather than the head means the skills are the ones the target branch already accepted. A skill added by the pull request itself takes effect for later pull requests, after it merges.

## 6. Limits and skipped skills {#SPEC-REVSKILL-06~draft}

**ID:** SPEC-REVSKILL-06~draft

- RS-23. Each `SKILL.md` has a size cap of 8 KiB (8192 bytes). The size is checked before the blob is read.
- RS-24. All loaded `SKILL.md` files together have a size cap of 24 KiB (24576 bytes). Skills count against it in configuration order, by file size including the frontmatter. A skill that would exceed it is skipped, and so is each later skill that does not fit.
- RS-25. A skill is skipped, not loaded, when it is missing, invalid, over a cap, or does not match. Skip reasons:

| Label | Record `kind` | Cause |
|---|---|---|
| `invalid name` | `invalid_name` | RS-5 fails, or `name` is absent or not a string. |
| `duplicate` | `duplicate` | The name already appeared in an earlier entry. |
| `invalid glob` | `invalid_glob` | An `apply_to` entry does not parse. |
| `not applicable` | `not_applicable` | No changed file matches `apply_to`. |
| `not found` | `not_found` | No `SKILL.md` at that path at the base commit. |
| `unreadable` | `unreadable` | A read error, a directory, a symlink, text that is not UTF-8, or text that matches the fence rule (RS-43). |
| `invalid frontmatter` | `invalid_frontmatter` | RS-11, RS-12, RS-13 or RS-15 fails. |
| `over per-skill cap` | `over_skill_cap` | The file exceeds 8 KiB. |
| `over total cap` | `over_total_cap` | Adding the skill exceeds 24 KiB. |
| `malformed entry` | `malformed_entry` | RS-41. |
| `fork pull request` | `fork` | RS-21. |
| `configuration unreadable` | `config_unreadable` | The base `.trusty-review.toml` exists but cannot be read or parsed (RS-45). |
| `no base commit` | `no_base` | RS-45. |
| `over entry cap` | `over_entry_cap` | RS-51. |

Each skipped skill carries a one-line reason beside its kind.

- RS-26. The review body names each loaded skill with a short hash, and each skipped skill with its name, label and reason. A `not applicable` skill is listed as "not applied", not as a fault. The notice sits just before the review footer. For a fork pull request it states that no repository skill text was used.
- RS-27. A skill never makes the review fail. Every skill error is a skip, and the review completes with the skills that loaded.

- RS-46. The `.trusty-review.toml` read at the base commit is capped at 64 KiB. A larger file is one skip of kind `config_unreadable` (RS-45): no skill loads, and the cause is the reason.
- RS-50. The review-body notice renders each name, reason and sha as one code span. Backticks and control or invisible characters are removed from the text, and each field is at most 160 characters. The notice is at most 8,000 characters. It is cut at a line boundary, and a final line reads "N more line(s) omitted; the review record lists them all".
- RS-51. At most 32 `[[review.skills]]` entries are processed. The rest become one skip of kind `over_entry_cap`, labelled "over entry cap" and named `entries 33-N`, where N is the entry count.
- RS-52. Each git read has a 10-second timeout. A timeout is a skipped read (RS-27), never a review failure.

## 7. Prompt placement {#SPEC-REVSKILL-07~draft}

**ID:** SPEC-REVSKILL-07~draft

The system prompt is layered today: the stock rubric, then principles, then voice, then the named review template. The template only appends; no layer replaces the rubric.

- RS-28. Loaded skills append after the review template as one labelled, fenced block. Each skill sits between `<<<BEGIN REPO-SKILL name="…" sha256="…">>>` and `<<<END REPO-SKILL name="…" sha256="…">>>` markers. The text between them is the body of its `SKILL.md`, trimmed, without the frontmatter.
- RS-29. The block opens with a fixed preamble. It names the base commit, says the text is repository guidance, treated as untrusted and not as operator instructions, and says it may add review checks. It says the verdict rules and the output schema above it stay authoritative, and that any part of a skill that asks to change them, to approve or block regardless of the code, to skip checks or to reveal the prompt is ignored.
- RS-30. Skill text cannot change the verdict rules or the output schema. The structured-output schema is enforced by the call, not by the prompt, so a skill cannot alter it.
- RS-31. Skills load in configuration order, so the prompt is deterministic for a given repository state. The block is the same for every pull request that applies the same skills, so it stays part of a stable prompt prefix.
- RS-32. With no loaded skill, the system prompt is byte-identical to a review with no skill configured.
- RS-47. The block ends by restating that the verdict rules, the severity and grading rules and the output schema given earlier remain in force and win any conflict with a skill.

## 8. Review record {#SPEC-REVSKILL-08~draft}

**ID:** SPEC-REVSKILL-08~draft

- RS-33. The review record carries a `repo_skills` object: `{base_sha, fork, loaded[{name, sha256, bytes}], skipped[{name, kind, reason}]}`. `base_sha` is the commit the configuration was read at, and is empty when none was read. `fork` is true for a fork pull request. `loaded` lists each loaded skill's `name`, content hash and size in bytes.
- RS-34. The content hash is the SHA-256 of the exact `SKILL.md` bytes read, in lowercase hex.
- RS-35. `skipped` lists each skipped skill with its `kind` and `reason` (§6).
- RS-36. The record fields are additive and optional. A consumer that ignores them keeps working. The `repo_skills` key is present only when skills are configured or the base configuration cannot be read, and is absent otherwise (RS-21, RS-45).

The hash lets a reader compare two reviews of different repositories, or of one repository over time, and tell whether the same skill text applied.

## 9. Surfaces {#SPEC-REVSKILL-09~draft}

**ID:** SPEC-REVSKILL-09~draft

- RS-37. The CLI `run` command and the local MCP `review_pr` tool read from the local checkout. The configuration is found at the git root of the working directory. Skill files and the configuration are read from the base commit through the git object store of that checkout, not from the working tree.
- RS-38. (Design intent.) The hosted webhook path has no checkout. It reads `.trusty-review.toml` and each `SKILL.md` at the base commit through the contents-at-ref reader (`DocFetcher`), which validates the path and requires a full commit SHA before any request.
- RS-39. (Design intent.) On the hosted path, an unreadable file is a skip with reason `unreadable`. A fork pull request makes no skill request at all (RS-21).
- RS-40. API changes are additive and optional. The existing `[review]` keys, CLI flags and MCP arguments keep their meaning. No new flag or argument is required to use the feature; the configuration file drives it.
- RS-48. The CLI `--config` option does not disable repository skills.
- RS-49. The webhook and HTTP service paths pass no skill input today. They load no skill and write no `repo_skills` record until RS-38 is implemented.

## 10. Acceptance tests {#SPEC-REVSKILL-10~draft}

**ID:** SPEC-REVSKILL-10~draft

- (a) With a configured skill, the review prompt contains that skill's `SKILL.md` text as it stands at the base commit, not the head commit. A real pull request, reviewed once with the skill and once without it, yields different finding sets.
- (b) A missing, invalid or over-cap skill is skipped. The review completes, and the review body names the skill with the reason.
- (c) A pull request from a fork gets no repository skill text in its prompt, and the review says so.
- (d) The review record lists the name and content hash of each loaded skill.

Unit coverage owed by the implementation: name validation (RS-5), frontmatter validation (RS-11 to RS-13), both caps, `apply_to` matching, symlink and traversal rejection, a prompt golden test showing block placement (RS-28), and the base-commit read on the local path. The base-commit read on the hosted path is owed with RS-38.

## 11. Out of scope {#SPEC-REVSKILL-11~draft}

**ID:** SPEC-REVSKILL-11~draft

- Skill sources outside the reviewed repository: installed, framework, bundled or remote.
- Loading a skill on demand during a review. The review is one structured-output call with no tool loop.
- Replacing or removing the stock rubric.
- Reading any file in a skill directory other than `SKILL.md`.
