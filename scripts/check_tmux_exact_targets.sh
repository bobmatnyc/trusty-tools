#!/usr/bin/env bash
#
# check_tmux_exact_targets.sh — every tmux `-t` target in the workspace is
# exact (issue #8443).
#
# Why: tmux resolves a bare `-t <name>` by exact name, then by PREFIX, then by
#   fnmatch. On 2026-09-23 `tm sessions resume tm-cto`, with `tm-cto` gone,
#   matched `tm-cto-reports` and killed it. #8443 moved every target into
#   `trusty_common::tmux`; this gate keeps a hand-built bare target from coming
#   back.
#
# What: scans every tracked Rust, shell, Swift, TS/JS and Svelte file, plus the
#   bundled Markdown assets under `crates/*/src/assets/` (skills and agents that
#   tell an agent which tmux command to run). Comment lines are ignored, and a
#   line ending in `\` is joined to the next before matching.
#     A. an argv token `"-t"` (or `'-t'`) in a non-shell file that mentions
#        tmux, or within 3 lines of a tmux verb literal. The NEXT argument must
#        be a string literal starting with `=`, an immutable id literal (`$N`
#        `@N` `%N`), a call to an approved helper, or a variable whose `let`
#        binding (within 30 lines) starts with such a call or builds its value
#        only from such calls — a binding that also calls `to_string`,
#        `to_owned`, `clone`, `into`, `String::from` or `format!` is bare. A
#        `"-t"` compared with `==`/`!=` or used as a match arm is skipped.
#     B. any `-t <tok>`, `-t"<tok>"` or `-t{<tok>}` on a line that mentions tmux
#        or a tmux verb — and, in a shell file that mentions tmux, on EVERY
#        line, because a wrapper (`"${TM[@]}" … -t "$S"`) hides the word.
#        `<tok>`, less any leading quote, must start with `=` or be a `%N`/`@N`
#        id. In a shell file `$N` is a positional parameter, never a session
#        id, so it is a finding; elsewhere `$N` is accepted. Outside shell a
#        `<placeholder>` is prose, and a `{…}` format placeholder is accepted
#        only when its statement calls an approved helper.
#   Approved helpers: `exact_session_target(`, `exact_window_target(`,
#   `exact_pane_target(`, `shell_exact_session_target(`,
#   `shell_attach_command(`, `.as_target()`.
#   A finding is excused only by a row in scripts/tmux-exact-targets-allowlist.tsv
#   (`path<TAB>line-regex<TAB>reason`). A row that excuses nothing is itself a
#   failure, so the allowlist cannot rot.
#
#   Scan floor: zero files enumerated, or zero tmux-mentioning files read, is a
#   failure (#4618), never a pass.
#
# Known limits — textual, not a parser:
#   - A target built in one function and passed to another that spawns tmux is
#     invisible unless the spawning call site holds a `-t` next to it.
#   - A conditional binding whose bare branch uses none of the listed builders
#     (e.g. `let t = if c { exact_session_target(n) } else { n };`) passes:
#     only the builder list above is recognised as "bare".
#   - A bare value that reaches tmux through a mutation other than `x = …`
#     (`t.clear(); t.push_str(n)`, `std::mem::swap`) passes.
#   - A verb alias is recognised only as a quoted argv literal within 3 lines
#     of the `-t`; an alias built at runtime, or in a file that never mentions
#     tmux and names no verb, is invisible.
#   - Combined short flags (`-st`) are checked only on a line that names tmux
#     or a verb, because `-it`/`-rt` belong to other tools; a combined flag on
#     a wrapper line (`"${TM[@]}" list-panes -st "$S"` names the verb, so it is
#     caught, but `"${TM[@]}" lsp -st "$S"` is not).
#   - A `let` bound more than 30 lines above its `-t`, and a `{…}` placeholder
#     whose value is computed outside its own statement, read as findings —
#     the safe direction; restructure or add an allowlist row.
#   - Markdown outside `crates/*/src/assets/` (ADRs, research notes) is not
#     scanned: it records history, and nothing executes it.
#
# Usage: bash scripts/check_tmux_exact_targets.sh
# Exit:  0 clean; 1 on findings, a stale allowlist row, or a scan-floor breach;
#        2 on a tool error.
# Test:  scripts/check_tmux_exact_targets_selftest.sh.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI); git and perl.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
ALLOWLIST="$SCRIPT_DIR/tmux-exact-targets-allowlist.tsv"

cd "$REPO_ROOT"

if ! files="$(git ls-files -- \
  '*.rs' '*.sh' '*.swift' '*.ts' '*.tsx' '*.js' '*.mjs' '*.svelte' \
  'crates/*/src/assets/**/*.md' \
  ':!**/node_modules/**' ':!scripts/check_tmux_exact_targets.sh' \
  ':!scripts/check_tmux_exact_targets_selftest.sh')"; then
  echo "check_tmux_exact_targets: TOOL ERROR: git ls-files failed" >&2
  exit 2
fi

if [ -z "$files" ]; then
  echo "check_tmux_exact_targets: SCAN FLOOR: no tracked source file to scan;" >&2
  echo "  a gate that scanned nothing has proven nothing (#4618)." >&2
  exit 1
fi

[ -f "$ALLOWLIST" ] || { echo "check_tmux_exact_targets: TOOL ERROR: missing $ALLOWLIST" >&2; exit 2; }

# The file list goes through a temp file: as one environment string it
# exceeds Linux's 128 KiB per-argument limit (`Argument list too long`).
FILE_LIST="$(mktemp)"
trap 'rm -f "$FILE_LIST"' EXIT
printf '%s\n' "$files" > "$FILE_LIST"

TMUX_GATE_FILE_LIST="$FILE_LIST" perl - "$ALLOWLIST" <<'PERL'
use strict;
use warnings;

my $allow_path = shift @ARGV;
my @allow;
open(my $af, "<", $allow_path) or die "open $allow_path: $!";
while (my $l = <$af>) {
    chomp $l;
    next if $l =~ /^\s*(#|$)/;
    my ($p, $re, $why) = split /\t/, $l, 3;
    die "allowlist row without a reason: $l\n" unless defined $why && $why =~ /\S/;
    push @allow, { path => $p, re => qr/$re/, why => $why, used => 0 };
}

my $helper = qr/\b(?:shell_)?exact_(?:session|window|pane)_target\s*\(|\bshell_attach_command\s*\(|\.as_target\s*\(\s*\)/;
my $bare_builder = qr/\.(?:to_string|to_owned|clone|into)\s*\(|\bString::from\b|\bformat!/;
# tmux verb ALIASES as quoted argv literals (`["has", "-t", n]`): too short to
# match as bare words, so only a quoted literal counts.
my $verb_literal = qr/[\x27"`](?:has|killp|killw|send|capturep|display|lsp|lsw|lsc|rename|renamew|neww|splitw|selectw|selectp|respawnp|attach|switchc|setenv|showenv|pipep)[\x27"`]/;
my $verb = qr/\b(?:has-session|kill-session|rename-session|list-windows|list-panes|list-clients|send-keys|capture-pane|display-message|split-window|new-window|select-window|select-pane|kill-window|kill-pane|respawn-pane|attach-session|switch-client|set-environment|show-environment|pipe-pane)\b/;
my (@findings, $scanned, $tmux_files);

sub excused {
    my ($path, $text) = @_;
    for my $a (@allow) {
        if ($a->{path} eq $path && $text =~ $a->{re}) { $a->{used}++; return 1; }
    }
    return 0;
}

sub token_ok {
    my ($tok, $shell, $stmt) = @_;
    $tok =~ s/^[\x27"`]+//;
    return 1 if $tok =~ /^=/;
    return 1 if $tok =~ /^[@%][0-9]+/;
    # The unresolvable sentinels an empty session name renders (#8443).
    return 1 if $tok =~ /^[\$%](?:[\x27"`;),]|$)/;
    return 1 if !$shell && $tok =~ /^\$[0-9]+/;
    return 1 if !$shell && $tok =~ /^</;
    return 1 if !$shell && $tok =~ /^\{/ && $stmt =~ $helper;
    return 0;
}

sub whole_helper_call {
    my ($e) = @_;
    $e =~ s/^&\s*//;
    return 1 if $e =~ /\.as_target\s*\(\s*\)$/ && $e !~ $bare_builder;
    return 0 unless $e =~ /^(?:[A-Za-z_][A-Za-z0-9_]*::)*(?:shell_)?exact_(?:session|window|pane)_target\s*\(/;
    my $open = index($e, "(");
    my $depth = 0;
    for my $k ($open .. length($e) - 1) {
        my $c = substr($e, $k, 1);
        $depth++ if $c eq "(";
        $depth-- if $c eq ")";
        return (substr($e, $k + 1) =~ /^\s*$/) ? 1 : 0 if $depth == 0;
    }
    return 0;
}

# A binding RHS is exact when it is one whole helper call, or when it builds
# only from helper calls (`pane.map_or_else(|| exact_window_target(s), …)`):
# no bare builder, and nothing indexed or sliced off a helper's result.
sub rhs_ok {
    my ($rhs) = @_;
    $rhs =~ s/^\s+|\s+$//g;
    return 1 if whole_helper_call($rhs);
    return $rhs =~ $helper && $rhs !~ $bare_builder && $rhs !~ /\)\s*\[/ ? 1 : 0;
}

sub next_arg {
    my ($rest) = @_;
    for (1 .. 8) {
        my $before = $rest;
        $rest =~ s/^\s+//;
        $rest =~ s/^\.(?:to_string|into|to_owned)\(\)//;
        $rest =~ s/^[)\];,]+//;
        $rest =~ s/^(?:[A-Za-z_][A-Za-z0-9_]*\.)?(?:push|arg)\(//;
        last if $rest eq $before;
    }
    my ($depth, $out, $q) = (0, "", "");
    for my $c (split //, substr($rest, 0, 400)) {
        if ($q) { $out .= $c; $q = "" if $c eq $q; next; }
        if ($c eq "\"" || $c eq "\x27") { $q = $c; $out .= $c; next; }
        if ($c =~ /[(\[{]/) { $depth++; }
        elsif ($c =~ /[)\]}]/) { last if $depth == 0; $depth--; }
        elsif (($c eq "," || $c eq ";") && $depth == 0) { last; }
        $out .= $c;
    }
    $out =~ s/^\s+|\s+$//g;
    return $out;
}

# The statement a line belongs to: this line through the next `;` (max 6 lines).
sub statement_from {
    my ($code, $i) = @_;
    my $s = "";
    for my $j ($i .. ($i + 5 > $#$code ? $#$code : $i + 5)) {
        $s .= $code->[$j];
        last if $code->[$j] =~ /;\s*$/;
    }
    return $s;
}

open(my $lf, "<", $ENV{TMUX_GATE_FILE_LIST}) or die "open file list: $!";
my @paths = map { chomp; $_ } <$lf>;
close $lf;
for my $path (@paths) {
    next unless length $path;
    open(my $fh, "<", $path) or die "open $path: $!";
    my @lines = <$fh>;
    close $fh;
    $scanned++;
    my $all = join "", @lines;
    my $mentions = $all =~ /tmux/i;
    $tmux_files++ if $mentions;
    my $shell = $path =~ /\.sh$/;
    my $md = $path =~ /\.md$/;
    next if $md && !$mentions;

    my @code = map {
        my $l = $_;
        ($l =~ m{^\s*(?://|/\*|\*|<!--)} || ($shell && $l =~ /^\s*#/)) ? "\n" : $l
    } @lines;

    # Shape B on logical lines (a trailing `\` joins the next physical line).
    my $i = 0;
    while ($i <= $#code) {
        my $start = $i;
        my $l = $code[$i];
        while ($l =~ /\\\s*\n\z/ && $i < $#code) {
            $l =~ s/\\\s*\n\z/ /;
            $i++;
            $l .= $code[$i];
        }
        $i++;
        # An attached `-t…` opening a string literal (`format!("-t{n}")`) in a
        # file that mentions tmux applies even without the word on the line.
        my $named = $l =~ /\btmux\b/ || $l =~ $verb;
        my $applies = ($shell && $mentions) || $named
            || ($mentions && $l =~ /[\x27"`]-t[^\s\x27"`]/);
        next unless $applies;
        my $stmt = $md ? $l : statement_from(\@code, $start);
        # `-t`, or combined short flags ending in t (`-st`) on a line that
        # names tmux or a verb — elsewhere `-it`/`-rt` belong to other tools.
        while ($l =~ /(?:^|[\s\x27"`(\[,])-([A-Za-z]*)t(?:\s+|(?=[\x27"{\$=%@]))(\S+)/g) {
            my ($flags, $tok) = ($1, $2);
            next if length($flags) && !$named;
            next if token_ok($tok, $shell, $stmt);
            # A lone `"-t"` argv token is Shape A`s job.
            next if !$shell && !$md && $tok =~ /^[\x27"](?:[,.)\]]|$)/;
            my $src = $lines[$start]; chomp $src;
            next if excused($path, $src);
            push @findings, sprintf("%s:%d: bare tmux target %s\n    %s", $path, $start + 1, $tok, $src);
        }
    }

    next if $shell || $md;
    my $text = join "", @code;
    # Backticks quote a string only in the JS family (`"-t"` in Rust is prose
    # when written `-t` inside a message).
    my $q = $path =~ /\.(?:ts|tsx|js|mjs|svelte)$/ ? qr/[\x27"`]/ : qr/[\x27"]/;
    while ($text =~ /($q)-([A-Za-z]*)t\1/g) {
        my $pos = pos($text);
        my $start = $pos - 4 - length($2);
        my $pre = substr($text, $start > 40 ? $start - 40 : 0, $start > 40 ? 40 : $start);
        my $post = substr($text, $pos, 40);
        next if $pre =~ /[=!]=\s*$/ || $post =~ /^\s*(?:=>|==|!=)/;
        my $line_no = (substr($text, 0, $pos) =~ tr/\n//) + 1;
        my $lo3 = $line_no - 4 < 0 ? 0 : $line_no - 4;
        my $near = join("", @code[$lo3 .. $line_no - 1]);
        next unless $mentions || $near =~ $verb || $near =~ $verb_literal;
        my $arg = next_arg(substr($text, $pos));
        my $ok = 0;
        $ok = 1 if whole_helper_call($arg);
        $ok = 1 if $arg =~ /^&?\s*[\x27"]=/ || $arg =~ /^&?\s*[\x27"][\$@%][0-9]+[\x27"]/;
        if (!$ok && $arg =~ /^&?\s*([A-Za-z_][A-Za-z0-9_]*)$/) {
            my $var = $1;
            my $lo = $line_no - 31 < 0 ? 0 : $line_no - 31;
            my $window = join "", @code[$lo .. $line_no - 1];
            # The LAST binding or reassignment before the use decides:
            # `let mut t = exact_…; t = n.to_string();` is bare.
            while ($window =~ /(?:\blet\s+(?:mut\s+)?|(?<![\w.])(?=\Q$var\E\s*=[^=]))\Q$var\E\b(?:\s*:[^=;]+)?\s*=(?!=)([^;]*);/gs) {
                $ok = rhs_ok($1);
            }
        }
        next if $ok;
        my $src = $lines[$line_no - 1]; chomp $src;
        next if excused($path, $src);
        push @findings, sprintf("%s:%d: \"-t\" followed by non-exact target `%s`\n    %s",
            $path, $line_no, $arg, $src);
    }
}

my $status = 0;
if (!$scanned || !$tmux_files) {
    print STDERR "check_tmux_exact_targets: SCAN FLOOR: no file that mentions tmux was read\n";
    exit 1;
}
for my $a (@allow) {
    next if $a->{used};
    print STDERR "check_tmux_exact_targets: STALE allowlist row (excuses nothing): $a->{path}\t$a->{re}\n";
    $status = 1;
}
if (@findings) {
    print STDERR "check_tmux_exact_targets: bare tmux -t target(s) found (#8443):\n";
    print STDERR "  $_\n" for @findings;
    print STDERR "\nRender the target with trusty_common::tmux::exact_session_target (session\n";
    print STDERR "verbs) or exact_window_target / exact_pane_target (window and pane verbs),\n";
    print STDERR "or spell it =name / =name: in a shell line. A genuine non-tmux -t, or a\n";
    print STDERR "runtime %N pane id, goes in scripts/tmux-exact-targets-allowlist.tsv with a reason.\n";
    $status = 1;
}
if ($status == 0) {
    print "check_tmux_exact_targets: OK: $scanned file(s) scanned ($tmux_files mention tmux), every -t target exact\n";
}
exit $status;
PERL
