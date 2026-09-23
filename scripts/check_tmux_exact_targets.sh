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
# What: scans every tracked Rust, shell, Swift, TS/JS and Svelte file that
#   mentions `tmux` for two shapes, ignoring comment lines:
#     A. an argv token `"-t"` (or `'-t'`) — the NEXT argument must be a string
#        literal starting with `=`, an immutable id literal (`$N` `@N` `%N`), a
#        call to an approved helper (`exact_session_target(`,
#        `exact_window_target(`, `exact_pane_target(`, `.as_target()`), or a
#        variable whose `let` binding within the previous 30 lines makes such a
#        call. A `"-t"` compared with `==`/`!=` or used as a match arm is not
#        an argument and is skipped.
#     B. a line containing `tmux … -t <tok>` (a shell command, a format
#        string) — `<tok>`, less any leading quote, must start with `=`, be an
#        immutable id, or be a `<placeholder>` in prose.
#   A finding is excused only by a row in scripts/tmux-exact-targets-allowlist.tsv
#   (`path<TAB>line-regex<TAB>reason`). A row that excuses nothing is itself a
#   failure, so the allowlist cannot rot.
#
#   Scan floor: zero files scanned is a failure (#4618), never a pass.
#
# Known limits: textual, not a parser. It cannot see a target assembled in a
#   variable bound more than 30 lines up, or passed through a helper function
#   of the crate's own; both read as findings, which is the safe direction.
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

set +e
files="$(git grep -l -i -I 'tmux' -- \
  '*.rs' '*.sh' '*.swift' '*.ts' '*.tsx' '*.js' '*.mjs' '*.svelte' \
  ':!**/node_modules/**' ':!scripts/check_tmux_exact_targets.sh' \
  ':!scripts/check_tmux_exact_targets_selftest.sh')"
status=$?
set -e
# git grep exits 1 on "no match" (the scan floor below) and >1 on a real error.
if [ "$status" -gt 1 ]; then
  echo "check_tmux_exact_targets: TOOL ERROR: git grep exited $status" >&2
  exit 2
fi

if [ -z "$files" ]; then
  echo "check_tmux_exact_targets: SCAN FLOOR: no tracked source file mentions tmux;" >&2
  echo "  a gate that scanned nothing has proven nothing (#4618)." >&2
  exit 1
fi

[ -f "$ALLOWLIST" ] || { echo "check_tmux_exact_targets: TOOL ERROR: missing $ALLOWLIST" >&2; exit 2; }

# shellcheck disable=SC2086
printf '%s\n' "$files" | perl -e '
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

my $helper = qr/\bexact_(?:session|window|pane)_target\s*\(|\.as_target\s*\(\s*\)/;
my $id_lit = qr/^[\x27"][\$@%][0-9]+[\x27"]/;
my (@findings, $scanned);

sub excused {
    my ($path, $text) = @_;
    for my $a (@allow) {
        if ($a->{path} eq $path && $text =~ $a->{re}) { $a->{used}++; return 1; }
    }
    return 0;
}

sub token_ok {
    my ($tok) = @_;
    $tok =~ s/^[\x27"`]+//;
    return 1 if $tok =~ /^=/;
    return 1 if $tok =~ /^[\$@%][0-9]+/;
    return 1 if $tok =~ /^</;
    return 0;
}

# The next argument after a "-t" token: skip conversions, closers and push/arg
# wrappers, then read one expression up to a top-level separator.
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

while (my $path = <STDIN>) {
    chomp $path;
    next unless length $path;
    open(my $fh, "<", $path) or die "open $path: $!";
    my @lines = <$fh>;
    close $fh;
    $scanned++;
    my $shell = $path =~ /\.sh$/;
    # Blank comment lines, keeping line numbers.
    my @code = map {
        my $l = $_;
        ($l =~ m{^\s*(?://|/\*|\*|<!--)} || ($shell && $l =~ /^\s*#/)) ? "\n" : $l
    } @lines;
    my $text = join "", @code;

    # Shape B: tmux ... -t <tok> on one line.
    for my $i (0 .. $#code) {
        my $l = $code[$i];
        while ($l =~ /\btmux\b[^\n]*?\s-t\s+(\S+)/g) {
            my $tok = $1;
            next if token_ok($tok);
            my $src = $lines[$i]; chomp $src;
            next if excused($path, $src);
            push @findings, sprintf("%s:%d: bare tmux target %s\n    %s", $path, $i + 1, $tok, $src);
        }
    }

    next if $shell;
    # Shape A: an argv "-t" token.
    while ($text =~ /([\x27"])-t\1/g) {
        my $pos = pos($text);
        my $start = $pos - 4;
        my $pre = substr($text, $start > 40 ? $start - 40 : 0, $start > 40 ? 40 : $start);
        my $post = substr($text, $pos, 40);
        next if $pre =~ /[=!]=\s*$/ || $post =~ /^\s*(?:=>|==|!=)/;
        my $line_no = (substr($text, 0, $pos) =~ tr/\n//) + 1;
        my $arg = next_arg(substr($text, $pos));
        my $ok = 0;
        $ok = 1 if $arg =~ $helper;
        $ok = 1 if $arg =~ /^&?\s*[\x27"]=/ || $arg =~ /^&?\s*$id_lit/;
        if (!$ok && $arg =~ /^&?\s*([A-Za-z_][A-Za-z0-9_]*)$/) {
            my $var = $1;
            my $lo = $line_no - 31 < 0 ? 0 : $line_no - 31;
            my $window = join "", @code[$lo .. $line_no - 1];
            $ok = 1 if $window =~ /\blet\s+(?:mut\s+)?\Q$var\E\b[^;]*?$helper/s;
        }
        next if $ok;
        my $src = $lines[$line_no - 1]; chomp $src;
        next if excused($path, $src);
        push @findings, sprintf("%s:%d: \"-t\" followed by non-exact target `%s`\n    %s",
            $path, $line_no, $arg, $src);
    }
}

my $status = 0;
if (!$scanned) {
    print STDERR "check_tmux_exact_targets: SCAN FLOOR: zero files read\n";
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
    print "check_tmux_exact_targets: OK: $scanned tmux-mentioning file(s) scanned, every -t target exact\n";
}
exit $status;
' "$ALLOWLIST"
