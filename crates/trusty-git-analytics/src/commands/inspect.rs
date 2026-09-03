//! `tga inspect` — show the live database schema, or attest what it holds.
//!
//! Why: #5218 — a reviewer asked what tga retained has, until now, had to read
//! 27 migration files and trust that the operator's database matches them.
//! What: two subcommands over one read-only connection — `schema` prints every
//! table, view, and column with the free-text ones marked, and `attest` prints
//! the data-handling claim together with the live evidence for it.
//! Test: `core::inspect::tests`, plus `tests::inspect_errors_on_a_missing_database`
//! below for the CLI's own fail-open arm.

use std::path::Path;

use clap::{Args, Subcommand};

use tga::core::inspect::{attest, render, schema};

/// Arguments for `tga inspect`.
#[derive(Args, Debug)]
pub struct InspectArgs {
    /// What to inspect.
    #[command(subcommand)]
    pub what: InspectSubcommand,
}

/// The two things worth inspecting.
#[derive(Subcommand, Debug)]
pub enum InspectSubcommand {
    /// Every table, view, and column the database actually holds.
    Schema(InspectFormatArgs),
    /// The data-handling attestation, with the live evidence behind it.
    Attest(InspectFormatArgs),
}

/// Output-format flag shared by both subcommands.
#[derive(Args, Debug, Default)]
pub struct InspectFormatArgs {
    /// Emit JSON instead of the human-readable report.
    #[arg(long)]
    pub json: bool,
}

/// Run `tga inspect`.
///
/// Why: dispatched from `main` BEFORE the shared `Database::open`, because that
/// call creates and migrates a missing file — an inspection built on it would
/// print a complete, empty, freshly-minted schema and exit 0 for a database the
/// caller cannot actually read (#5218).
/// What: opens `db_path` read-only through
/// [`tga::core::inspect::open_read_only`], which names the cause for a missing
/// path, a directory, or a non-SQLite file, then renders the requested report to
/// stdout.
/// Test: `tests::inspect_errors_on_a_missing_database`,
/// `tests::attest_exits_non_zero_on_findings`,
/// `core::inspect::tests::open_read_only_names_a_missing_database`.
///
/// # Errors
///
/// Propagates the open failure, or any SQLite error raised while reading the
/// schema. Never returns `Ok` having printed an empty report for a database it
/// could not read. `attest` additionally errors, after printing its report, when
/// the verdict is [`attest::Verdict::Findings`].
pub fn run(db_path: &Path, args: InspectArgs) -> anyhow::Result<()> {
    let conn = tga::core::inspect::open_read_only(db_path)?;
    let snapshot = schema::snapshot(&conn)?;

    match args.what {
        InspectSubcommand::Schema(fmt) => {
            if fmt.json {
                println!("{}", serde_json::to_string_pretty(&snapshot)?);
            } else {
                print!("{}", render::schema_report(&snapshot));
            }
        }
        InspectSubcommand::Attest(fmt) => {
            let attestation = attest::attest(&conn, &snapshot)?;
            if fmt.json {
                println!("{}", serde_json::to_string_pretty(&attestation)?);
            } else {
                print!("{}", render::attestation_report(&attestation));
            }
            // #5218: the report is on stdout either way; a findings verdict also
            // exits non-zero so a release or hand-over script can gate on it
            // instead of grepping the text it just printed.
            if attestation.verdict == attest::Verdict::Findings {
                let flagged: i64 = attestation
                    .scanned_columns
                    .iter()
                    .map(|s| s.diff_shaped_rows)
                    .sum();
                anyhow::bail!(
                    "attestation findings: {} content-bearing column(s), {flagged} row(s) \
                     carrying diff-shaped text — the claim above does not hold unreviewed",
                    attestation.content_columns.len()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the command layer is where the fail-open would actually reach a
    /// user, so the arm is pinned here as well as in the library.
    /// What: runs both subcommands against a path that does not exist and
    /// asserts each errors, names the path, and creates nothing.
    /// Test: this test itself.
    #[test]
    fn inspect_errors_on_a_missing_database() {
        let mut path = std::env::temp_dir();
        path.push(format!("tga-inspect-cmd-{}-absent.db", std::process::id()));
        let _ = std::fs::remove_file(&path);

        for what in [
            InspectSubcommand::Schema(InspectFormatArgs::default()),
            InspectSubcommand::Attest(InspectFormatArgs::default()),
        ] {
            let err = run(&path, InspectArgs { what }).expect_err("must not succeed");
            let text = err.to_string();
            assert!(
                text.contains("no database at"),
                "the error must name the cause: {text}"
            );
            assert!(
                text.contains("absent.db"),
                "the error must name the path: {text}"
            );
        }
        assert!(!path.exists(), "inspection must not create a database");
    }

    /// Why: an attestation that exits 0 on findings is a gate nothing can hang
    /// a release script on, and a report a reader skims to the wrong verdict.
    /// What: seeds a diff into `commits.message`, runs `inspect attest`, and
    /// asserts the command errors while the clean database beside it succeeds.
    /// Test: this test itself.
    #[test]
    fn attest_exits_non_zero_on_findings() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("tga-inspect-verdict-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("tga.db");

        {
            let db = tga::core::db::Database::open(&path).expect("migrate");
            db.connection()
                .execute(
                    "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
                     VALUES ('c1', 'Ada', 'ada@example.com', '2026-01-01T00:00:00Z', 'ok', 'r')",
                    [],
                )
                .expect("clean commit");
        }
        run(
            &path,
            InspectArgs {
                what: InspectSubcommand::Attest(InspectFormatArgs::default()),
            },
        )
        .expect("a clean database must attest cleanly");

        {
            let db = tga::core::db::Database::open(&path).expect("reopen");
            db.connection()
                .execute(
                    "UPDATE commits SET message = ?1 WHERE sha = 'c1'",
                    ["fix\n\ndiff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\n"],
                )
                .expect("paste a diff");
        }
        let err = run(
            &path,
            InspectArgs {
                what: InspectSubcommand::Attest(InspectFormatArgs::default()),
            },
        )
        .expect_err("a diff in a free-text column must fail the gate");
        assert!(
            err.to_string().contains("diff-shaped text"),
            "the error must name what was found: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
