#!/bin/sh
# fake-tmux.sh — a stand-in `tmux` that answers from fixture files (#9822).
#
# Why: the A4 parity runs the prototype's input-state.py and the Rust
#   `tm architect` verbs against the same panes, with no tmux server at all.
# What: installed as `tmux` first on PATH. FAKE_TMUX_PANES names a directory
#   holding targets.tsv (target, TAB, capture file) and list-panes.txt.
#   `list-panes` prints list-panes.txt; `capture-pane` prints the file its
#   target maps to. The target is the one argument that starts with `%` or
#   `=`. A target with no file exits 1 with tmux's "can't find pane" text on
#   stderr; any other call exits 1. A leading `-S <socket>` is ignored.
# Test: regen_a4.py (Python side), tests/verbs_parity.rs and trusty-mpm's
#   tests/tm_architect_verbs.rs (Rust side).
dir="${FAKE_TMUX_PANES:?FAKE_TMUX_PANES is not set}"
if [ "${1:-}" = "-S" ]; then shift 2; fi
verb="${1:-}"
[ "$#" -gt 0 ] && shift
target=""
for arg in "$@"; do
  case "$arg" in
    %*|=*) target="$arg" ;;
  esac
done
case "$verb" in
  list-panes)
    if [ -f "$dir/list-panes.txt" ]; then exec cat "$dir/list-panes.txt"; fi ;;
  capture-pane)
    file="$(awk -F '\t' -v want="$target" '$1 == want { print $2; exit }' "$dir/targets.tsv")"
    if [ -n "$file" ] && [ -f "$dir/$file" ]; then exec cat "$dir/$file"; fi
    echo "can't find pane: $target" >&2
    exit 1 ;;
esac
echo "fake tmux: no answer for ${verb:-<none>}" >&2
exit 1
