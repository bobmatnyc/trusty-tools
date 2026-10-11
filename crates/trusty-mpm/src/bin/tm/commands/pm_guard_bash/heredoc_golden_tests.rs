//! Golden verdicts for the here-document scanners, recorded before #9720.
//!
//! Why: #9720 rewrote four scan loops for linear time — the walk's pending
//! low-water marks, the operator lookup, the line-end search and the span
//! queries. None of them may change an answer.
//! What: renders, for a fixed corpus, every output those loops feed — the
//! operator list, each [`HeredocBodies`] field, the per-byte answers of
//! `contains`, `suppresses_separator` and `shell_body_starting_at`, and the
//! pm-guard, credential-print and egress verdicts (reasons hashed) — and
//! compares each line with `heredoc_golden_9720.txt`, recorded on f7d0c6f54c.
//! The corpus is the here-document cases of the existing suites plus 500
//! commands drawn by a fixed generator from shell tokens and lines.
//! Test: `heredoc_scan_matches_the_pre_9720_golden_table`.

use super::super::credential_print::{evaluate_credential_print_command, heredoc_operators};
use super::super::evaluate_bash_command;
use super::HeredocBodies;
use crate::commands::egress_guard::{Verdict, decide};
use crate::commands::egress_guard_families::FamilyTable;

/// The table recorded on the base commit, one `<index>\t<render>` per case.
const GOLDEN: &str = include_str!("heredoc_golden_9720.txt");

/// Here-document cases from the existing suites (#5356, #6946, #8111, #8756,
/// #9150, #9155, #9180).
const KNOWN: [&str; 52] = [
    "python3 <<'PY'\nprint(len(k) > 3)\nPY",
    "python3 <<'PY' > out.rs\nprint(1)\nPY",
    "grep x <<< 'a > b'\necho done > f.rs",
    "python3 <<'PY'\nprint(1)\n",
    "echo $((1 << 3))\necho x > f.rs",
    "cat <<'EOF' > f\nbody\nEOF\nnext",
    "cat <<-EOF\n\ta > b\n\tEOF",
    "grep -n '<<EOF' f\necho x > g.rs",
    "cat >> verb.rs <<'RSEOF'\nstruct VerbStub {\n}\nRSEOF",
    "bash <<'EOF'\nls .env\nEOF",
    "bash <<X\necho '$(date)'\nX",
    "sh -c \"cat <<'A B'\nx\nA B\necho \\$(rm -rf /)\nA\"",
    "diff <<A <<B\na > b\nA\nc > d\nB",
    "cat <<'EOF'\r\na > b\r\nEOF\r\necho done\r\n",
    "cat <<'EOF'\r\na > b\r\nEOF",
    "cat <<'EOF'\r\nx\r\nEOF\r\necho \"$(rm -rf /)\"\r\n",
    "cat <<'A'\nx\nA\r\ncat <<'X'\nA\necho \"$(rm -rf /)\"\nX",
    "bash<<'O'\nbody\nO",
    "cat <<'X'\na\\\nX\necho done",
    "bash <<'O'\ncat <<X\nx\nX",
    "bash <<'O'\nx\nO\necho \"open",
    "cat <<'X'\nx\nX",
    "cat <<'A B'\nx\nA B\necho \"$(rm -rf /)\"\nA",
    "cat <<'A>B'\nx\nA>B\necho \"$(rm -rf /)\"\nA",
    "cat <<-'A B'\n\tx\n\tA B\necho \"$(rm -rf /)\"\n\tA",
    "cat <<'A\tB'\nx\nA\tB\necho \"$(rm -rf /)\"\nA",
    "cat <<\"A B\"\nx\nA B\necho \"$(rm -rf /)\"\nA",
    "cat <<A\\ B\nx\nA B\necho \"$(rm -rf /)\"\nA",
    "cat <<A\\\nB\n$(rm -rf /)\nA\nAB",
    "cat <<'A'$(x)\nx\nA$(x)\necho \"$(rm -rf /)\"\nA$",
    "cat <<'A'$((1 + 2))\nA$((1 + 2))\necho \"$(rm -rf /)\"\nA$",
    "cat <<'A'$[1 + 2]\nx\nA$[1 + 2]\necho \"$(rm -rf /)\"\nA$[1",
    "cat <<'A'(x y)\nx\nA(x y)\necho \"$(rm -rf /)\"\nA",
    "cat <<'A'<(x)\nx\nA<(x)\necho \"$(rm -rf /)\"\nA",
    "cat <<'A\\B'\nx\nA\\B\necho \"$(rm -rf /)\"\nAB",
    "cat <<\"A'B\"\nx\nA'B\necho \"$(rm -rf /)\"\nAB\n: '",
    "cat <<A\\'B\nx\nA'B\necho \"$(rm -rf /)\"\nAB\n: '",
    "echo hi # <<'EOF'\necho \"$(rm -rf /)\"\nEOF",
    "echo $((1<<\"2\"))\necho \"$(rm -rf /)\"\n2",
    "(( x = 1 <<\\2 ))\necho \"$(rm -rf /)\"\n2",
    "echo ${x:-<<'EOF'}\necho \"$(rm -rf /)\"\nEOF}",
    "echo \\<<'EOF'\necho \"$(rm -rf /)\"\nEOF",
    "x=$(cat <<'EOF')\necho \"$(rm -rf /)\"\nEOF",
    "x=$(cat <<'EOF'\nbody\nEOF\n)\necho done > f.rs",
    "echo `cat <<X`\nX\necho done",
    "cat <<'EOF' | gh issue comment 1 --body-file -\nrun `gh auth token` here\nEOF",
    "cat <<EOF > notes.md\n$(git log -1)\nEOF\ngit commit -m wip",
    "cat <<'EOF' > notes.md\ngit commit -m wip\nEOF\ngit status",
    "bash <<'A' <<'B'\necho a\nA\necho b > f.rs\nB",
    "true # <<'X'\necho hidden\nX",
    "cat <<X; echo $((1 << 2))\nbody\nX\necho tail > g.rs",
    "cat <<EOF\na\\\nEOF\nEOF",
];

/// Shell tokens the generated cases are drawn from.
const TOKENS: [&str; 30] = [
    "<<", "<<-", "<<<", "'", "\"", "$(", ")", "(", "`", "$((", "))", "${", "}", "#", "\n", "\n",
    "x", "X", "EOF", "'EOF'", " ", " ", "\\", "\t", "cat ", "bash ", ">", ";", "$[", "]",
];

/// Operator lines for the line-structured generated cases.
const OPERATOR_LINES: [&str; 14] = [
    "cat <<X",
    "cat <<'EOF' > f",
    "bash <<X",
    "cat <<-X",
    "x=$(cat <<X)",
    "echo $((1<<X))",
    "cat <<X <<EOF",
    "echo `cat <<X`",
    "cat <<X # <<EOF",
    "cat <<X; bash <<'EOF'",
    "diff <<X <<'EOF' > f",
    "cat <<X \\",
    "echo \"$(cat <<X)\" <<EOF",
    "(( 1 << 2 )); cat <<\"X\"",
];

/// Body and terminator lines for the line-structured generated cases.
const BODY_LINES: [&str; 12] = [
    "a",
    "a > b",
    "$(x)",
    "it's",
    "X",
    "X",
    "EOF",
    "\tX",
    "a\\",
    "echo \"$(rm -rf /)\" > g",
    "",
    "X\r",
];

/// A fixed linear congruential generator.
fn lcg(seed: u64) -> impl FnMut() -> usize {
    let mut state = seed;
    move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as usize
    }
}

/// 300 strings of 3 to 24 shell tokens, then 200 commands of one to three
/// operator lines each followed by up to four body lines.
fn generated() -> Vec<String> {
    let mut next = lcg(0x9720);
    let tokens = (0..300).map(|_| {
        let len = 3 + next() % 22;
        (0..len).map(|_| TOKENS[next() % TOKENS.len()]).collect()
    });
    let mut tokens: Vec<String> = tokens.collect();
    let mut next = lcg(0x9721);
    for _ in 0..200 {
        let mut lines = Vec::new();
        for _ in 0..1 + next() % 3 {
            lines.push(OPERATOR_LINES[next() % OPERATOR_LINES.len()]);
            for _ in 0..next() % 5 {
                lines.push(BODY_LINES[next() % BODY_LINES.len()]);
            }
        }
        tokens.push(lines.join("\n"));
    }
    tokens
}

/// FNV-1a over `text`, so a long deny reason renders short.
fn digest(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// One byte per character: `1` where `answer` holds at that byte.
fn bits(len: usize, answer: impl Fn(usize) -> bool) -> String {
    (0..len)
        .map(|i| if answer(i) { '1' } else { '0' })
        .collect()
}

/// Every scanner output and verdict #9720 must not change, on one line.
fn render(command: &str) -> String {
    let ops = heredoc_operators(command);
    let scan = HeredocBodies::scan(command);
    let data: Vec<_> = scan
        .data
        .iter()
        .map(|d| (d.span, d.operator_line, d.expands))
        .collect();
    let bodies: Vec<_> = scan
        .bodies
        .iter()
        .map(|d| (d.span, d.operator_line, d.expands))
        .collect();
    let starts: Vec<_> = (0..=command.len())
        .filter_map(|i| scan.shell_body_starting_at(i).map(|end| (i, end)))
        .collect();
    let egress = match decide(
        &Ok(serde_json::json!({ "tool_name": "Bash", "tool_input": { "command": command } })),
        &FamilyTable::builtin(),
    ) {
        Verdict::Allow => "allow".to_string(),
        Verdict::Deny { check, reason } => format!("{check}:{}", digest(&reason)),
    };
    format!(
        "ops={ops:?} spans={:?} frames={:?} data={data:?} expanding={:?} bodies={bodies:?} \
         unscannable={} lost={} in={} sep={} shell={starts:?} pm={:?} cp={:?} egress={egress}",
        scan.spans,
        scan.frames,
        scan.expanding,
        scan.unscannable,
        scan.lost,
        bits(command.len(), |i| scan.contains(i)),
        bits(command.len(), |i| scan.suppresses_separator(i)),
        evaluate_bash_command(command).map(digest),
        evaluate_credential_print_command(command).map(|r| digest(&r)),
    )
}

/// 🔴 #9720 equivalence: every case renders exactly as on the base commit.
#[test]
fn heredoc_scan_matches_the_pre_9720_golden_table() {
    let cases: Vec<String> = KNOWN
        .iter()
        .map(|c| c.to_string())
        .chain(generated())
        .collect();
    let actual: Vec<String> = cases
        .iter()
        .enumerate()
        .map(|(i, case)| format!("{i}\t{}", render(case)))
        .collect();
    let expected: Vec<&str> = GOLDEN.lines().collect();
    let differ: Vec<String> = actual
        .iter()
        .enumerate()
        .filter(|(i, line)| expected.get(*i) != Some(&line.as_str()))
        .map(|(i, line)| format!("case {i} {:?}\n  now: {line}", cases[i]))
        .collect();
    assert!(
        differ.is_empty() && expected.len() == actual.len(),
        "{} of {} cases differ from the golden table ({} lines):\n{}\n--- full table ---\n{}",
        differ.len(),
        actual.len(),
        expected.len(),
        differ
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
        actual.join("\n"),
    );
}
