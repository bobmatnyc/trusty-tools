//! Migration v20 tests: `pr_reviewers.review_state` and `submitted_at` columns.
//!
//! Why: `reviewer_store` unit tests cover the ORM layer but do not verify the
//! migration SQL itself; this module confirms that the ALTER TABLE statements
//! actually land in a fresh in-memory DB. Keeping the test here mirrors the
//! v17 pattern (pre-flight column checks live in dedicated `v<N>.rs` modules).
//! What: opens a full-migration in-memory DB, inserts GitHub and ADO reviewer
//! rows, and asserts that the new columns behave correctly.
//! Test: `migration_v20_adds_review_state_columns`.

#[cfg(test)]
pub(super) mod tests {
    use crate::core::db::Database;
    use rusqlite::params;

    /// Why: regression guard for issue #742. Migration v20 must add
    /// `review_state` and `submitted_at` TEXT columns to `pr_reviewers` so
    /// GitHub review data can be stored alongside ADO reviewer votes.
    /// What: opens an in-memory DB (runs all migrations), inserts a GitHub
    /// reviewer row with the new columns, reads it back, confirms both
    /// columns round-trip and that existing NULL patterns work for ADO rows.
    /// Test: this test.
    #[test]
    pub(crate) fn migration_v20_adds_review_state_columns() {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();

        // Seed a PR row to satisfy the FK.
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, commit_shas) \
             VALUES ('github', 'acme/widget', 1, 'T', 'u', 'open', '2024-01-01T00:00:00Z', '[]')",
            [],
        )
        .expect("seed pr");
        let pr_id: i64 = conn
            .query_row(
                "SELECT id FROM pull_requests WHERE provider='github' AND repository='acme/widget'",
                [],
                |r| r.get(0),
            )
            .expect("pr id");

        // Insert a reviewer row using the new columns.
        // vote=0: schema has INTEGER NOT NULL DEFAULT 0; GitHub rows have no
        // numeric vote so we store 0 and use review_state to identify them.
        conn.execute(
            "INSERT INTO pr_reviewers \
             (pr_id, provider, reviewer_id, display_name, vote, is_required, is_container, \
              review_state, submitted_at) \
             VALUES (?1, 'github', 'octocat', NULL, 0, 0, 0, 'APPROVED', '2024-06-01T12:00:00Z')",
            params![pr_id],
        )
        .expect("insert github reviewer");

        let (review_state, submitted_at, vote): (String, String, i64) = conn
            .query_row(
                "SELECT review_state, submitted_at, vote FROM pr_reviewers WHERE pr_id = ?1",
                params![pr_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("read back");

        assert_eq!(review_state, "APPROVED");
        assert_eq!(submitted_at, "2024-06-01T12:00:00Z");
        assert_eq!(
            vote, 0,
            "GitHub reviewer vote is stored as 0 (schema default)"
        );

        // Verify existing ADO rows can still use NULL for the new columns.
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, commit_shas) \
             VALUES ('azdo', 'proj/repo', 99, 'ADO PR', 'user', 'merged', '2024-01-01T00:00:00Z', '[]')",
            [],
        )
        .expect("seed ado pr");
        let ado_pr_id: i64 = conn
            .query_row(
                "SELECT id FROM pull_requests WHERE provider='azdo'",
                [],
                |r| r.get(0),
            )
            .expect("ado pr id");
        conn.execute(
            "INSERT INTO pr_reviewers \
             (pr_id, provider, reviewer_id, display_name, vote, is_required, is_container) \
             VALUES (?1, 'azdo', 'user1', 'User One', 10, 0, 0)",
            params![ado_pr_id],
        )
        .expect("insert ado reviewer");

        let (ado_review_state, ado_submitted): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT review_state, submitted_at FROM pr_reviewers WHERE provider='azdo'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("ado read back");
        assert!(
            ado_review_state.is_none(),
            "ADO row review_state must be NULL after v20"
        );
        assert!(
            ado_submitted.is_none(),
            "ADO row submitted_at must be NULL after v20"
        );
    }
}
