//! #9001 cases 2-4: the secret rules' false positives, each beside the deny
//! that bounds its fix. Every command goes through the unified secret entry
//! point, so the #7266 read rule, the #8596 credential-print rule and the
//! #8875 delete rule all judge it.

use crate::commands::pm_guard_secret_read::evaluate_secret_file_read;

/// The unified secret verdict for a Bash `command`.
fn secret(command: &str) -> Option<String> {
    let input = serde_json::json!({ "command": command });
    evaluate_secret_file_read("Bash", Some(&input))
}

fn allowed(commands: &[&str]) {
    for command in commands {
        assert_eq!(secret(command), None, "expected ALLOW: {command}");
    }
}

fn denied(commands: &[&str]) {
    for command in commands {
        assert!(secret(command).is_some(), "expected DENY: {command}");
    }
}

/// Case 2: `` `db`.* `` in a `mysql -e` statement is a SQL wildcard.
#[test]
fn a_sql_wildcard_in_a_mysql_statement_is_no_path_9001() {
    allowed(&[
        "mysql -h db -u admin -e \"GRANT ALL PRIVILEGES ON \\`appdb\\`.* TO 'app'@'%'\"",
        "mariadb --execute \"GRANT SELECT ON \\`appdb\\`.* TO 'ro'@'%'\" -h db",
    ]);
}

/// Case 2 bound: a secret named in the statement or as an operand still denies.
#[test]
fn a_secret_named_in_a_mysql_statement_still_denies_9001() {
    denied(&[
        "mysql -e \"LOAD DATA LOCAL INFILE '.env' INTO TABLE t\"",
        "mysql -e \"SELECT LOAD_FILE('/srv/app/.env.production')\"",
        "mysql --defaults-extra-file=.env -e 'SELECT 1'",
        "mysql -e 'SELECT 1' < .env",
        "cat .env.*",
    ]);
}

/// Case 3: a gh issue/pr search string reads no file.
#[test]
fn a_gh_search_string_names_no_file_9001() {
    allowed(&[
        "gh issue list -R o/r --search \".env.*\" --state all",
        "gh pr list --search=.env.* --limit 5",
    ]);
}

/// Case 3 bound: a file flag beside the search, another subcommand, or a
/// substitution in the search value still denies.
#[test]
fn a_gh_file_flag_beside_a_search_still_denies_9001() {
    denied(&[
        "gh issue create --title x --body-file .env",
        "gh api repos/o/r --search .env.local --input .env",
        "gh issue list --search \"$(cat .env)\"",
        "gh myext --search .env",
    ]);
}

/// Case 3: the reported loop, whose words reach only echo and a gh search.
#[test]
fn a_for_loop_feeding_only_a_gh_search_names_no_file_9001() {
    allowed(&[
        "for k in \"pm-guard false positive\" \"8902\" \".env.*\" \"mysql\"; do echo \"== $k\"; \
         gh issue list -R bobmatnyc/trusty-tools --search \"$k\" --state all --limit 15 \
         --json number,title,state --jq '.[]|\"#\\(.number) [\\(.state)] \\(.title)\"'; done",
        "for k in .env.* id_rsa; do gh pr list --search \"$k\"; done",
    ]);
}

/// Case 3 bound: the loop's words reaching anything but echo and the search
/// value keeps the deny.
#[test]
fn a_for_loop_whose_words_reach_anything_else_still_denies_9001() {
    denied(&[
        "for k in .env.*; do cat \"$k\"; done",
        "for k in .env.*; do gh issue list --search \"$k\"; cat \"$k\"; done",
        "for k in .env.*; do gh issue list --search \"$k\"; done; cat \"$k\"",
        "for k in .env.*; do echo \"$k\" | xargs cat; done",
        "for k in .env.*; do echo \"$k\" > names; done",
        "for k in .env.*; do gh issue list --search \"$k\" --repo \"$k\"; done",
        "for k in .env.*; do gh issue list --search $k; done & cat .env",
        "for k in .env.*; do echo \"$kx\"; done",
        "gh() { cat \"$5\"; }; for k in .env.*; do gh issue list --search \"$k\"; done",
        "for k in .env.*; do eval \"cat $k\"; done",
        "for k in .env.*; do gh issue list --search \"${k}\"; done",
        "for k in .env.*; do gh issue list --search \"$k\"; done; for j in .env; do :; done",
    ]);
}

/// Case 4: a Python here-document that edits a local file, with an
/// apostrophe line and bracketed words, is no `gh api` secret DELETE.
#[test]
fn a_python_heredoc_with_no_gh_or_curl_is_no_secret_delete_9001() {
    allowed(&[
        "python3 - <<'EOF'\n\
               p = 'docs/notes.md'\n\
               s = open(p).read()\n\
               # don't merge d[k] x[0] here\n\
               s = s.replace('old', 'new')\n\
               open(p, 'w').write(s)\n\
               EOF",
        "python3 - <<'EOF'\n\
         p = 'src/x_tests.rs'\n\
         # don't drop the `cd` in d[k] x[0]\n\
         s = open(p).read().replace('\"cd $WT && x\"', '\"cd $WT && y\"')\n\
         open(p, 'w').write(s)\n\
         EOF",
    ]);
}

/// Case 4 bound: a real secret DELETE, and a body that names gh or curl or
/// runs a substitution, still deny; so does a heredoc fed to a shell.
#[test]
fn a_heredoc_body_that_names_gh_or_runs_a_substitution_still_denies_9001() {
    denied(&[
        "gh api -X DELETE repos/o/r/actions/secrets/NAME",
        "python3 - <<'EOF'\n# don't d[k] x[0]\nimport os; os.system('gh api -X DELETE repos/o/r/actions/secrets/X')\nEOF",
        "python3 - <<EOF\n# don't d[k] x[0]\nx = \"$(gh api -X DELETE repos/o/r/actions/secrets/X)\"\nEOF",
        "G=gh A=api python3 - <<'EOF'\n# don't d[k] x[0]\n\
         import os; os.system(\"$G $A -X DELETE repos/o/r/actions/secrets/X\")\nEOF",
        "bash <<'EOF'\ngh api -X DELETE repos/o/r/actions/secrets/X\nEOF",
        "cat <<'EOF' | sh\ngh api -X DELETE repos/o/r/actions/secrets/X\nEOF",
        "gh auth token",
    ]);
}
