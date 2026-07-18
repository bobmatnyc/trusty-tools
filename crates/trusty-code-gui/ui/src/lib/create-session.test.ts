// Why: `lib/create-session.ts` carries every piece of pure gating/body-
// construction logic `CreateSessionForm.svelte` depends on for correctness
// (what the daemon receives, when the submit button may be clicked) —
// covering it here means those invariants are checked without mounting
// Svelte or touching `fetch`, mirroring the split `session-status.test.ts`/
// `context-budget.test.ts` already establish for their own modules.
// What: One `describe` block per exported function.
// Test: this file.
import { describe, expect, it } from 'vitest';
import {
  bindingLabel,
  buildCreateBody,
  canSubmitCreate,
  describeFsError,
  type ProjectSelection,
} from './create-session';

const GIT_PROJECT: ProjectSelection = {
  path: '/Users/bob/code/acme-api',
  displayPath: 'acme-api',
  isGitRepo: true,
};

const NON_GIT_PROJECT: ProjectSelection = {
  path: '/Users/bob/code/scratch',
  displayPath: 'scratch',
  isGitRepo: false,
};

describe('buildCreateBody', () => {
  it('trims the task and omits project when projectless (AC-2.1)', () => {
    expect(buildCreateBody('  fix the bug  ', null)).toEqual({ task: 'fix the bug' });
  });

  it('includes project.path when a project is selected', () => {
    expect(buildCreateBody('add a feature', GIT_PROJECT)).toEqual({
      task: 'add a feature',
      project: '/Users/bob/code/acme-api',
    });
  });

  it('includes a non-git project path identically to a git one (AC-2.6)', () => {
    expect(buildCreateBody('index this', NON_GIT_PROJECT)).toEqual({
      task: 'index this',
      project: '/Users/bob/code/scratch',
    });
  });

  it('never sends the agent field (no pre-session roster route, see module doc)', () => {
    const body = buildCreateBody('t', GIT_PROJECT);
    expect('agent' in body).toBe(false);
  });
});

describe('canSubmitCreate', () => {
  it('is false for an empty task', () => {
    expect(canSubmitCreate('', 'idle')).toBe(false);
  });

  it('is false for a whitespace-only task', () => {
    expect(canSubmitCreate('   ', 'idle')).toBe(false);
  });

  it('is true for a non-empty task while idle', () => {
    expect(canSubmitCreate('do the thing', 'idle')).toBe(true);
  });

  it('is false while submitting, even with a valid task (no double submit)', () => {
    expect(canSubmitCreate('do the thing', 'submitting')).toBe(false);
  });
});

describe('bindingLabel', () => {
  it('labels a null selection as projectless', () => {
    expect(bindingLabel(null)).toBe('projectless — chat/planning only');
  });

  it('labels a git-repo selection distinctly from a plain directory', () => {
    expect(bindingLabel(GIT_PROJECT)).toBe('git repo — acme-api');
    expect(bindingLabel(NON_GIT_PROJECT)).toBe('directory — scratch');
  });
});

describe('describeFsError', () => {
  it('maps the four rpc_error_to_status-mapped GET /fs statuses distinctly', () => {
    expect(describeFsError(404)).toBe('path not found');
    expect(describeFsError(400)).toBe('not a directory');
    expect(describeFsError(403)).toBe('permission denied');
    expect(describeFsError(500)).toBe('error (HTTP 500)');
  });
});
