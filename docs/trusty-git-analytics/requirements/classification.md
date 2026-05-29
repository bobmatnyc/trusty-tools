# Classification Engine

The classification engine assigns a `change_type` (and optional remapped `work_type`) to
every commit. It runs as a four-tier cascade: the first tier to produce a result wins, with
a rule-based fallback ensuring every commit receives a classification.

## Four-Tier Cascade

### Tier 0: Manual Overrides (confidence: 1.0)

- Source: `classification_overrides` table
- Key: `(commit_hash, repo_path)`
- Set via `tga override --commit <HASH> --repo <PATH> --change-type <TYPE> --reason <...>`
- Always wins when present

### Tier 1.5: Issue Type Classifier (confidence: 0.90)

Applies when the commit has at least one `ticket_references` entry that resolves to an
`issue_cache` row. The cached `issue_type` is mapped via `ISSUETYPE_CHANGE_TYPE_MAP`:

| Issue Type (case-insensitive) | change_type |
|-------------------------------|-------------|
| `bug` / `defect` / `error` | `bugfix` |
| `story` / `feature` / `new feature` / `epic` / `improvement` / `enhancement` | `feature` |
| `task` / `sub-task` / `subtask` | `None` → check labels |
| `technical task` | `maintenance` |
| `tech debt` / `infrastructure` / `platform` | `platform` |
| `spike` | `research` |
| `documentation` | `documentation` |
| `test` | `test` |

**Task label disambiguation** (only when issue_type ∈ {task, sub-task, subtask}):

| Label substring | change_type |
|------------------|-------------|
| `platform` / `infra` / `tooling` | `platform` |
| `refactor` / `tech-debt` | `refactor` |
| `maintenance` / `chore` | `maintenance` |
| (default) | `maintenance` |

If the commit has multiple ticket references mapped to different change_types, the highest
confidence (alphabetical tiebreak) wins.

### Tier 3: JIRA Project Key Mapping (confidence: 0.95)

Applies when `jira_project_mappings` is set in config and the commit has ticket references
matching the JIRA pattern `[A-Z]+-\d+`.

- Project key extracted from regex, normalized uppercase
- Looked up in `jira_project_mappings: dict<string,string>`
- Multiple matches → first config-order match wins

### LLM Classification (confidence threshold: 0.7 configurable)

- **Providers**: `openrouter` (default), `bedrock`, `auto`
- **Default model**: `mistralai/mistral-7b-instruct`
- **Batch size**: 50 commits per request
- **Parameters**: `max_tokens=50`, `temperature=0.1`, `timeout=30s`
- **Circuit breaker**: after 3 consecutive batch failures, LLM disabled for run
- **Response cache**: 90-day TTL keyed by commit hash + model name
- Result accepted only when `confidence >= confidence_threshold`

### Rule-Based Fallback (always available)

Ordered first-match list. Patterns compiled into a single `aho-corasick` automaton at
startup for O(message_length) matching.

| Priority | change_type | Patterns (case-insensitive substrings / regex) |
|----------|-------------|------------------------------------------------|
| 1 | `maintenance` | merge commit patterns, `chore:`, `update deps`, `bump version` |
| 2 | `bugfix` | `revert`, `fix:`, `bug:`, `resolve`, `repair`, `correct` |
| 3 | `platform` | `platform:`, `infra:`, `devops:`, `tooling:`, `architect` |
| 4 | `feature` | `feat:`, `add feature`, `implement`, `introduce` |
| 5 | `refactor` | `refactor:`, `restructure`, `optimize`, `improve`, `clean up` |
| 6 | `documentation` | `docs:`, `documentation:`, `readme` |
| 7 | `test` | `test:`, `spec:`, `add test` |
| 8 | `style` | `style:`, `format:`, `lint`, `prettier`, `whitespace` |

Default: `maintenance`.

---

## Change Type Taxonomy (19 values)

| Value | Description |
|-------|-------------|
| `feature` | New user-facing functionality |
| `bugfix` | Defect repair |
| `platform` | Infrastructure / platform engineering |
| `refactor` | Code restructuring without behavior change |
| `documentation` | Docs-only changes |
| `test` | Test additions or fixes |
| `maintenance` | Routine upkeep (deps, config, chores) |
| `style` | Formatting, lint fixes |
| `build` | Build system changes |
| `security` | Security patches |
| `hotfix` | Urgent production fix |
| `revert` | Code reversion |
| `integration` | Third-party integration work |
| `content` | Content updates (non-code) |
| `localization` | i18n / translation |
| `research` | Spike / investigation |
| `ktlo` | Keep-the-lights-on operational work |
| `other` | Uncategorized |
| `unknown` | Classifier could not decide |

### Fallthrough Categories

For coverage metrics, these are treated as "unclassified":

```
{maintenance, ktlo, other, unknown}
```

---

## Work Type Taxonomy Mapping

When `taxonomy_mapping` is configured, a post-classification SQL UPDATE pass remaps
`change_type` → `work_type`:

```sql
UPDATE qualitative_commits
SET work_type = COALESCE(
    (SELECT mapped_value FROM taxonomy_mapping WHERE source_change_type = change_type),
    change_type
);
```

When no mapping exists for a given `change_type`, `work_type` falls back to `change_type`.

---

## Coverage Metrics

```
coverage_pct = 100 * (commits NOT IN fallthrough) / total_commits
```

- Computed per-repo, stored in `repository_analysis_status.classification_coverage_pct`
- Warning emitted at `coverage < 20%` (configurable via `--coverage-threshold`)
- `--validate-coverage` flag causes `tga classify` to exit non-zero when below threshold

---

## Rust Implementation Notes

| Concern | Approach |
|---------|----------|
| Tier 0 lookup | `SELECT FROM classification_overrides WHERE commit_hash = ? AND repo_path = ?` (rusqlite prepared statement, single connection per worker) |
| Tier 1.5 map | `HashMap<String, Option<ChangeType>>` compiled once at startup from `ISSUETYPE_CHANGE_TYPE_MAP` constant |
| Tier 3 map | `HashMap<String, WorkType>` deserialized from config |
| Rule patterns | Single `aho_corasick::AhoCorasick` automaton per tier; lookup tier → change_type via match index |
| LLM dispatch | `async fn` returning `Result<Vec<LlmResult>>`; `tokio::select!` for concurrent batches with timeout |
| Cascade dispatcher | `rayon::par_iter` over commit batches; each batch invokes async LLM via `tokio::runtime::Handle::block_on` from a dedicated executor pool |
| Result type | `struct ClassificationResult { change_type: ChangeType, work_type: WorkType, confidence: f32, tier: ClassificationTier }` |
| Tier enum | `enum ClassificationTier { Override, IssueType, JiraMapping, Llm, RuleBased }` |
| ChangeType enum | 19 variants, `#[derive(Serialize, Deserialize, sqlx::Type)]` (or rusqlite `ToSql`/`FromSql`) |

---

## Diagnostics

`tga classify --show-jira-signals` emits per-commit diagnostics:

```
abc1234 → ticket_refs=[API-123, PLAT-456] | issue_types=[bug, task] | tier=issue_type | result=bugfix(0.90)
```

This is the canonical diagnostic for understanding why a given commit landed in a given
tier and helps validate `jira_project_mappings` / `ISSUETYPE_CHANGE_TYPE_MAP` correctness.
