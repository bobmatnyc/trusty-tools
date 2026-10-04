//! Unit tests for [`super`], the here-document body scanner.

use super::*;

/// The body of a `<<'PY'` script — the #5356 reproduction — is claimed.
#[test]
fn heredoc_bodies_cover_a_quoted_delimiter_body() {
    let command = "python3 <<'PY'\nprint(len(k) > 3)\nPY";
    let bodies = HeredocBodies::scan(command);
    let gt = command.find('>').expect("comparison operator");
    assert!(bodies.contains(gt), "the script's `>` must be body content");
}

/// A redirect on the operator line stays live syntax.
#[test]
fn heredoc_bodies_exclude_the_operator_line() {
    let command = "python3 <<'PY' > out.rs\nprint(1)\nPY";
    let bodies = HeredocBodies::scan(command);
    let gt = command.find('>').expect("redirect");
    assert!(!bodies.contains(gt), "the operator line must stay live");
}

/// `<<<` is a here-string; nothing follows it as a body.
#[test]
fn heredoc_bodies_ignore_a_here_string() {
    let command = "grep x <<< 'a > b'\necho done > f.rs";
    let bodies = HeredocBodies::scan(command);
    let redirect = command.rfind('>').expect("redirect");
    assert!(!bodies.contains(redirect));
}

/// An unterminated delimiter — and an arithmetic `<<`, which looks the
/// same — claims nothing, so the pre-#5356 over-deny is preserved.
#[test]
fn heredoc_bodies_claim_nothing_when_unterminated() {
    let unterminated = "python3 <<'PY'\nprint(1)\n";
    assert!(!HeredocBodies::scan(unterminated).contains(20));
    let shift = "echo $((1 << 3))\necho x > f.rs";
    let redirect = shift.rfind('>').expect("redirect");
    assert!(!HeredocBodies::scan(shift).contains(redirect));
}

/// #6946: the frame runs from the newline that opened the body through the
/// end of the terminator line, and stops there.
#[test]
fn heredoc_frames_cover_the_terminator_line() {
    let command = "cat <<'EOF' > f\nbody\nEOF\nnext";
    let bodies = HeredocBodies::scan(command);
    let opening_newline = command.find('\n').expect("operator line ends");
    assert!(bodies.suppresses_separator(opening_newline));
    assert!(!bodies.suppresses_separator(opening_newline - 1));
    let terminator = command.find("EOF\nnext").expect("terminator line");
    for idx in terminator..terminator + 3 {
        assert!(bodies.suppresses_separator(idx));
    }
    // The newline after the terminator is a live separator again.
    assert!(!bodies.suppresses_separator(terminator + 3));
}

/// #6946 fail-open check: a body handed to a shell is shell source, so it
/// gets no frame and its separators keep splitting.
#[test]
fn heredoc_frames_are_empty_for_a_shell_operator_line() {
    for command in [
        "bash <<'EOF'\nbody\nEOF",
        "sudo /bin/sh <<EOF\nbody\nEOF",
        "true && zsh <<EOF\nbody\nEOF",
    ] {
        let bodies = HeredocBodies::scan(command);
        let newline = command.find('\n').expect("operator line ends");
        assert!(
            !bodies.suppresses_separator(newline),
            "{command} should not be framed"
        );
        // The #5356 body span is still claimed — only framing differs.
        assert!(bodies.contains(newline + 1), "{command} body span");
    }
}

/// `<<-` allows a tab-indented terminator.
#[test]
fn heredoc_bodies_handle_tab_stripped_delimiter() {
    let command = "cat <<-EOF\n\ta > b\n\tEOF";
    let bodies = HeredocBodies::scan(command);
    let gt = command.find('>').expect("arrow");
    assert!(bodies.contains(gt));
}

/// A `<<` inside quotes is argument text, not an operator.
#[test]
fn heredoc_bodies_ignore_quoted_operator() {
    let command = "grep -n '<<EOF' f\necho x > g.rs";
    let bodies = HeredocBodies::scan(command);
    let redirect = command.rfind('>').expect("redirect");
    assert!(!bodies.contains(redirect));
}

/// #7266: the body leaves the argv text as spaces and comes back as text.
#[test]
fn splits_a_heredoc_body_out_of_the_argv_text() {
    let command = "cat >> verb.rs <<'RSEOF'\nstruct VerbStub {\n}\nRSEOF";
    let (argv_text, bodies) = split_heredoc_bodies(command);
    assert_eq!(argv_text.len(), command.len(), "byte offsets are preserved");
    assert!(argv_text.starts_with("cat >> verb.rs <<'RSEOF'"));
    assert!(argv_text.trim_end().ends_with("RSEOF"), "{argv_text:?}");
    assert!(!argv_text.contains('{'), "{argv_text:?}");
    assert_eq!(bodies, vec!["struct VerbStub {\n}\n".to_string()]);
}

/// #7266 fail-open check: a body the operator line hands to a shell IS
/// shell source, so it stays in place for the segment classifiers.
#[test]
fn leaves_a_shell_heredoc_body_in_the_argv_text() {
    let command = "bash <<'EOF'\nls .env\nEOF";
    let (argv_text, bodies) = split_heredoc_bodies(command);
    assert_eq!(argv_text, command);
    assert!(bodies.is_empty());
}

/// #7266: no here-document, and an unterminated one, both leave the
/// command byte-identical — the pre-fix scan runs unchanged.
#[test]
fn splits_nothing_without_a_heredoc() {
    for command in ["awk '{print}' f", "cat <<EOF\nno terminator"] {
        let (argv_text, bodies) = split_heredoc_bodies(command);
        assert_eq!(argv_text, command);
        assert!(bodies.is_empty(), "{command:?}");
    }
}

/// #8756: only an unquoted delimiter expands its body, and each data body
/// knows the line that opened it.
#[test]
fn data_bodies_record_whether_the_delimiter_was_quoted() {
    for (command, expands) in [
        ("cat <<EOF\n$(date)\nEOF", true),
        ("cat <<'EOF'\n$(date)\nEOF", false),
        ("cat <<\"EOF\"\n$(date)\nEOF", false),
        ("cat <<\\EOF\n$(date)\nEOF", false),
        ("cat <<-E'O'F\n$(date)\n\tEOF", false),
    ] {
        let bodies = data_bodies(command);
        assert_eq!(bodies.len(), 1, "{command:?}");
        assert_eq!(bodies[0].expands, expands, "{command:?}");
        let (start, end) = bodies[0].operator_line;
        assert!(command[start..end].starts_with("cat <<"), "{command:?}");
    }
}

/// #9155: every unquoted-delimiter body is recorded as expanding, a body a
/// shell runs included; a quoted one is not.
#[test]
fn heredoc_bodies_record_every_expanding_body_9155() {
    for (command, expanding) in [
        ("cat <<EOF\n$(date)\nEOF", 1),
        ("bash <<X\necho '$(date)'\nX", 1),
        ("python3 - <<PY\nprint('$(date)')\nPY", 1),
        ("bash <<'X'\necho '$(date)'\nX", 0),
        ("cat <<'EOF'\n$(date)\nEOF", 0),
    ] {
        let bodies = HeredocBodies::scan(command);
        assert_eq!(bodies.expanding().len(), expanding, "{command:?}");
    }
    let command = "bash <<X\necho '$(date)'\nX";
    let (start, end) = HeredocBodies::scan(command).expanding()[0];
    assert_eq!(&command[start..end], "echo '$(date)'\n");
}

/// #9150: a delimiter word outside the `[A-Za-z0-9_.-]` allowlist — a
/// quoted or escaped break byte, a substitution, a `\` the shell keeps, a
/// quote or `\` left open — makes the scan unscannable and claim nothing,
/// on both the balanced and the #8111 retry path; a plain delimiter does not.
#[test]
fn heredoc_bodies_refuse_a_delimiter_split_by_a_word_break() {
    for command in [
        "cat <<'A B'\nx\nA B\necho \"$(rm -rf /)\"\nA",
        "cat <<'A>B'\nx\nA>B\nA",
        "cat <<'A<B'\nx\nA<B\nA",
        "cat <<-'A B'\n\tx\n\tA B\n\tA",
        "cat <<'A\tB'\nx\nA\tB\nA",
        "cat <<\"A;B\"\nx\nA;B\nA",
        "cat <<A\\ B\nx\nA B\nA",
        "cat <<A\\\nB\n$(rm -rf /)\nA\nAB",
        "cat <<'A\nB'\nx\nA",
        // #8111 retry path: an apostrophe in the body unbalances the map.
        "cat <<'A B'\nit's\nA B\nA",
        // Retry path, `own` unbalanced and the word unreadable.
        "cat <<'A\nB'\nit's\nA",
        // Round 2: a substitution or glob group in the word.
        "cat <<'A'$(x)\nx\nA$(x)\necho \"$(rm -rf /)\"\nA$",
        "cat <<'A'$((1 + 2))\nA$((1 + 2))\necho \"$(rm -rf /)\"\nA$",
        "cat <<'A'$[1 + 2]\nx\nA$[1 + 2]\necho \"$(rm -rf /)\"\nA$[1",
        "cat <<'A'${x:- }\nA${x:- }\necho \"$(rm -rf /)\"\nA${x:-",
        "cat <<'A'`x y`\nx\nA`x y`\necho \"$(rm -rf /)\"\nA`x",
        "cat <<'A'<(x)\nx\nA<(x)\necho \"$(rm -rf /)\"\nA",
        "cat <<'A'>(x)\nx\nA>(x)\necho \"$(rm -rf /)\"\nA",
        "cat <<'A'(x y)\nx\nA(x y)\necho \"$(rm -rf /)\"\nA",
        // Round 2: a `\` or quote the shell keeps in the word.
        "cat <<'A\\B'\nx\nA\\B\necho \"$(rm -rf /)\"\nAB",
        "cat <<\"A\\B\"\nx\nA\\B\necho \"$(rm -rf /)\"\nAB",
        "cat <<A\\\\B\nx\nA\\B\necho \"$(rm -rf /)\"\nAB",
        "cat <<\"A'B\"\nx\nA'B\necho \"$(rm -rf /)\"\nAB\n: '",
        "cat <<A\\'B\nx\nA'B\necho \"$(rm -rf /)\"\nAB\n: '",
        "cat <<''\nx\n\necho \"$(rm -rf /)\"",
    ] {
        let bodies = HeredocBodies::scan(command);
        assert!(bodies.is_unscannable(), "{command:?}");
        assert!(bodies.spans.is_empty(), "{command:?} claims nothing");
        assert_eq!(
            super::super::unclassifiable_command(command),
            Some(HEREDOC_DELIMITER_REASON),
            "{command:?}"
        );
    }
    for command in [
        "cat <<'EOF'\n$(rm -rf /)\nEOF\necho benign",
        "cat <<\"EOF\" > out\nx\nEOF",
        "cat <<\\EOF\nx\nEOF",
        "cat <<-E'O'F\nx\n\tEOF",
        "cat <<'EOF';echo hi\nx\nEOF",
        "cat <<'EOF'\nit's\nEOF",
        "echo $((1 << 3))",
        "python3 <<'PY'\nprint(1)\n",
        "cat <<'END_OF-file.1'\nx\nEND_OF-file.1",
        "x=$(cat <<'EOF'\n$(rm -rf /)\nEOF\n)",
        "(cat <<'EOF')\nx\nEOF",
        "cat <<EOF\r\nx\r\nEOF\r",
    ] {
        assert!(
            !HeredocBodies::scan(command).is_unscannable(),
            "{command:?}"
        );
        assert_eq!(
            super::super::unclassifiable_command(command),
            None,
            "{command:?}"
        );
    }
}

/// #9150 item 4: a wrapper's inner delimiter is refused through the
/// recursion in `unclassifiable_at`, though the outer `<<` is quoted text.
#[test]
fn heredoc_delimiter_refusal_reaches_a_wrapped_command() {
    let command = "sh -c \"cat <<'A B'\nx\nA B\necho \\$(rm -rf /)\nA\"";
    assert!(!HeredocBodies::scan(command).is_unscannable());
    assert_eq!(
        super::super::unclassifiable_command(command),
        Some(HEREDOC_DELIMITER_REASON)
    );
}

/// #9150: the shared walk records a `<<` in code, an ambiguous one in
/// arithmetic or a same-line `$(…)`, and none in a comment, `${…}` text, a
/// quote or after a `\`.
#[test]
fn heredoc_operators_skip_comments_escapes_and_expansions() {
    use OperatorCtx::{Ambiguous, Code};
    for (command, want) in [
        ("cat <<'EOF'\nx\nEOF", vec![(4, Code)]),
        ("echo hi # <<'EOF'\nx\nEOF", vec![]),
        ("echo \\<<'EOF'\nx\nEOF", vec![]),
        ("echo ${x:-<<'EOF'}\nx\nEOF}", vec![]),
        ("echo '<<EOF'", vec![]),
        ("echo $((1<<\"2\"))\nx\n2", vec![(9, Ambiguous)]),
        ("(( x = 1 <<\\2 ))", vec![(9, Ambiguous)]),
        ("echo $[1<<2]", vec![(8, Ambiguous)]),
        ("x=$(cat <<'EOF')\nx\nEOF", vec![(8, Ambiguous)]),
        ("x=`cat <<'EOF' `\nx\nEOF", vec![(7, Ambiguous)]),
        ("x=$(cat <<'EOF'\nx\nEOF\n)", vec![(8, Code)]),
        ("echo ${x:-$(cat <<'EOF'\nx\nEOF\n)}", vec![(16, Code)]),
    ] {
        assert_eq!(heredoc_operators(command), want, "{command:?}");
    }
}

/// #9150: a `<<` the shell reads as text claims no body, so the next lines
/// stay live; an ambiguous one with a matching later line is refused, and
/// one with no such line opens nothing.
#[test]
fn heredoc_bodies_skip_an_operator_outside_code() {
    for command in [
        "echo hi # <<'EOF'\necho \"$(rm -rf /)\"\nEOF",
        "echo \\<<'EOF'\necho \"$(rm -rf /)\"\nEOF",
        "echo ${x:-<<'EOF'}\necho \"$(rm -rf /)\"\nEOF}",
        "echo $((1 << 3))\necho \"$(rm -rf /)\"",
    ] {
        let bodies = HeredocBodies::scan(command);
        assert!(!bodies.is_unscannable(), "{command:?}");
        assert!(bodies.spans.is_empty(), "{command:?} claims nothing");
    }
    for command in [
        "echo $((1<<\"2\"))\necho \"$(rm -rf /)\"\n2",
        "(( x = 1 <<\\2 ))\necho \"$(rm -rf /)\"\n2",
        "echo $[1<<2]\necho \"$(rm -rf /)\"\n2",
        "x=$(cat <<'EOF')\necho \"$(rm -rf /)\"\nEOF",
        "x=`cat <<'EOF' `\necho \"$(rm -rf /)\"\nEOF",
    ] {
        assert!(HeredocBodies::scan(command).is_unscannable(), "{command:?}");
        assert_eq!(
            super::super::unclassifiable_command(command),
            Some(HEREDOC_DELIMITER_REASON),
            "{command:?}"
        );
    }
}

/// Two here-documents opened on one line consume their bodies in order.
#[test]
fn heredoc_bodies_span_two_heredocs_on_one_line() {
    let command = "diff <<A <<B\na > b\nA\nc > d\nB";
    let bodies = HeredocBodies::scan(command);
    let first = command.find('>').expect("first arrow");
    let second = command.rfind('>').expect("second arrow");
    assert!(bodies.contains(first), "first body claimed");
    assert!(bodies.contains(second), "second body claimed");
}

/// #9150: a CRLF here-document reads like the LF form. Bash 3.2 and zsh 5.9
/// keep the `\r` in the delimiter word and in the terminator line, so the
/// body is claimed, a live line after it is not, and an `A\r` line never
/// ends an `A` body.
#[test]
fn heredoc_bodies_read_a_crlf_heredoc_as_the_shell_does() {
    let plain = "cat <<'EOF'\r\na > b\r\nEOF\r\necho done\r\n";
    let bodies = HeredocBodies::scan(plain);
    assert!(!bodies.is_unscannable());
    assert!(bodies.contains(plain.find('>').expect("arrow")), "body");
    assert!(!bodies.contains(plain.find("echo").expect("echo")), "live");
    // A segment's `trim()` cuts the final `EOF\r` to `EOF`; nothing follows.
    let trimmed = "cat <<'EOF'\r\na > b\r\nEOF";
    assert!(HeredocBodies::scan(trimmed).contains(trimmed.find('>').expect(">")));
    let live = "cat <<'EOF'\r\nx\r\nEOF\r\necho \"$(rm -rf /)\"\r\n";
    assert!(!HeredocBodies::scan(live).contains(live.find("$(").expect("$(")));
    assert_eq!(super::super::unclassifiable_command(live), None);
    // The shell's `A` body runs past `A\r` to the bare `A`; the line after
    // that is live, though a `\r`-trimming scan read it as an `X` body.
    let decoy = "cat <<'A'\nx\nA\r\ncat <<'X'\nA\necho \"$(rm -rf /)\"\nX";
    let bodies = HeredocBodies::scan(decoy);
    assert!(bodies.contains(decoy.find("cat <<'X'").expect("inner")));
    assert!(!bodies.contains(decoy.find("$(").expect("$(")), "live");
}
