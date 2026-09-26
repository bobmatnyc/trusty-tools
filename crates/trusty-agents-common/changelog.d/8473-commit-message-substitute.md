Fixed

- `BASE-AGENT.md`'s worktree-isolation bullet names the substitute for a heredoc-embedded `git commit -m "$(cat <<'EOF' … EOF)"`, which a worktree agent's harness refuses: repeated `-m` flags, or `git commit -F <file>` with the file written by the Write tool ([#8473](https://github.com/bobmatnyc/trusty-tools/issues/8473)).
