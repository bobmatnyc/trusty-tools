// #8443: the attach hint matches trusty_common::tmux::shell_attach_command.
import { describe, expect, it } from 'vitest';
import { shellAttachCommand } from './tmuxAttach';

describe('shellAttachCommand', () => {
  it('targets the session exactly and quotes it', () => {
    expect(shellAttachCommand('tm-cto')).toBe("tmux attach-session -t '=tm-cto'");
  });

  it('normalizes : and . the way tmux stores the name', () => {
    expect(shellAttachCommand('tm:proj.0')).toBe("tmux attach-session -t '=tm_proj_0'");
  });

  it('never renders a bare = for an empty name', () => {
    for (const name of ['', '=', '  ']) {
      expect(shellAttachCommand(name)).toBe("tmux attach-session -t '$'");
    }
  });

  it("escapes a single quote for the shell", () => {
    expect(shellAttachCommand("a'b")).toBe("tmux attach-session -t '=a'\\''b'");
  });
});
