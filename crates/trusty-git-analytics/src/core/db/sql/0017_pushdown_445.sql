-- Migration v17: push-down columns for issue #445 batch A.
--
-- Additive only — no existing columns are modified, no data is removed.
-- All three tables gain optional columns that the Rust-side backfill
-- subcommands populate retroactively.

-- classifications: top-level work category derived from subcategory taxonomy.
-- Populated at classification write time (write_results_chunk) and via
-- `tga backfill top-level`.
ALTER TABLE classifications ADD COLUMN top_level_category TEXT;
CREATE INDEX IF NOT EXISTS idx_classifications_top_level ON classifications(top_level_category);

-- fact_commit_effort: numeric T-shirt size (1–5) alongside the text label.
-- XS=1, S=2, M=3, L=4, XL=5. Populated at effort persist time and via
-- `tga backfill effort-tshirt`.
ALTER TABLE fact_commit_effort ADD COLUMN effort_tshirt INTEGER;
CREATE INDEX IF NOT EXISTS idx_fact_commit_effort_tshirt ON fact_commit_effort(effort_tshirt);

-- commits: AI-tool co-authorship attribution.
-- is_ai_assisted is 0/1; ai_tool is a stable string identifier
-- ("claude", "copilot", "cursor") or NULL.
ALTER TABLE commits ADD COLUMN is_ai_assisted INTEGER NOT NULL DEFAULT 0;
ALTER TABLE commits ADD COLUMN ai_tool TEXT;
CREATE INDEX IF NOT EXISTS idx_commits_is_ai_assisted ON commits(is_ai_assisted);
