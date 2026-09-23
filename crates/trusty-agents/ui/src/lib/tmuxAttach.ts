// Copy-pasteable tmux attach command for a session name (#8443).
//
// Why: tmux resolves a bare `-t <name>` by prefix, so a hint for a missing
// session can attach to a different one. Mirrors trusty_common::tmux's
// `shell_attach_command`: tmux stores `:` and `.` in a session name as `_`, the
// `=` prefix makes the match exact, and the single quotes stop zsh reading a
// leading `=` as a command-path expansion.
// Test: `tmuxAttach.test.ts`.

/**
 * `tmux attach-session -t '=<name>'`, normalized and quoted for a POSIX shell.
 * An empty (or `=`-only) name gets `'$'`, which matches no session: `'='` is
 * what tmux reads as the mouse target.
 */
export function shellAttachCommand(name: string): string {
  const normalized = name.replace(/^=/, '').replace(/[:.]/g, '_');
  if (normalized.trim() === '') {
    return "tmux attach-session -t '$'";
  }
  const quoted = normalized.replace(/'/g, "'\\''");
  return `tmux attach-session -t '=${quoted}'`;
}
