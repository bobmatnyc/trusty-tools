# Institutional mandates

A mandate is a rule an organization sets for every project it owns, or for a
named set of them: ticketing style, commit and PR style, where cloud logs
drain, and similar. The user supplies them. The Architect checks them during
provisioning, in architectural reviews and in fleet passes (`architect.md`,
"Architect duties").

Mandates are instance data. Each one is a TOML file in `local/mandates/`,
named `<id>.toml`. This directory holds only the schema and one fake example
(`example.toml`); it holds no real mandate.

## Schema

| Field | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | string | yes | Unique, kebab-case. Matches the file name. |
| `title` | string | yes | One line. |
| `category` | string | no | Grouping, e.g. `ticketing`, `commits-prs`, `logging`, `security`. |
| `scope` | `"all"` or list of strings | yes | `"all"`, or project names as used in the project's palace record. |
| `severity` | `"block"`, `"warn"` or `"info"` | yes | `block`: the PM fixes it before its next merge or release. `warn`: the PM fixes it in normal work. `info`: report only. |
| `rule` | string | yes | The rule in plain words: what must be true. |
| `check.kind` | `"command"` or `"review"` | yes | How compliance is checked. |
| `check.command` | string | if `kind = "command"` | A read-only shell command run in the project root. Exit 0 means compliant; any other exit means a violation. |
| `check.question` | string | if `kind = "review"` | A yes/no question a reviewer answers; "yes" means compliant. |
| `source.owner` | string | yes | The person or team that owns the mandate and rules on questions about it. |
| `source.reference` | string | no | Where the policy is written (URL or document). |
| `source.added` | date string | yes | When the user supplied it (`YYYY-MM-DD`). |
| `exceptions` | array of tables | no | `{ project, reason, ruling }`: a project the user exempted, with the ruling's date and source. |

## How the Architect applies them

- **When:** at provisioning (`architect-project-provision`), in every
  architectural review (`architect-review`), and on full-poll passes for
  mandates with a `command` check.
- **Commands stay read-only.** A check never writes to the project repo. A
  command that would need to write is a `review` check instead.
- **Violations** go to the owning PM as plain relays: the mandate id, the
  rule, the evidence and the severity.
- **Conflicts** between a mandate and a project's own rules (CLAUDE.md, ADRs,
  TICKETING.md) go to the user as a multiple-choice question. The Architect
  never picks one alone. An exemption the user grants is recorded under
  `exceptions`.
- **Changes** to a mandate come from the user or its owner. Record each change
  in `local/actions.md` and commit `local/`.
