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
  extractSessionId,
  isDirListing,
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

describe('isDirListing', () => {
  const VALID = {
    path: '/home/bob',
    display_path: '~',
    parent: '/home',
    entries: [{ name: 'code', path: '/home/bob/code', is_dir: true, is_git_repo: false }],
  };

  it('accepts a well-formed listing, including a null parent and empty entries', () => {
    expect(isDirListing(VALID)).toBe(true);
    expect(isDirListing({ ...VALID, parent: null, entries: [] })).toBe(true);
  });

  it('rejects non-object bodies (null, string, array)', () => {
    expect(isDirListing(null)).toBe(false);
    expect(isDirListing('ok')).toBe(false);
    expect(isDirListing([VALID])).toBe(false);
  });

  it('rejects a 200 body missing entries or with non-array entries (PR #3103 HIGH finding)', () => {
    const { entries: _entries, ...noEntries } = VALID;
    expect(isDirListing(noEntries)).toBe(false);
    expect(isDirListing({ ...VALID, entries: 'oops' })).toBe(false);
  });

  it('rejects a listing whose entries have drifted shapes', () => {
    expect(isDirListing({ ...VALID, entries: [{ name: 'x' }] })).toBe(false);
    expect(
      isDirListing({
        ...VALID,
        entries: [{ name: 'x', path: '/x', is_dir: 'yes', is_git_repo: false }],
      }),
    ).toBe(false);
  });

  it('rejects drifted top-level fields the UI dereferences', () => {
    expect(isDirListing({ ...VALID, display_path: 7 })).toBe(false);
    expect(isDirListing({ ...VALID, parent: 42 })).toBe(false);
  });
});

describe('extractSessionId', () => {
  it('returns the id when it is a non-empty string', () => {
    expect(extractSessionId({ id: 'sess-abc12345', status: 'running' })).toBe('sess-abc12345');
  });

  it('returns null for missing, empty, or non-string ids and non-object bodies', () => {
    expect(extractSessionId({})).toBeNull();
    expect(extractSessionId({ id: '' })).toBeNull();
    expect(extractSessionId({ id: 42 })).toBeNull();
    expect(extractSessionId(null)).toBeNull();
    expect(extractSessionId('sess-abc')).toBeNull();
  });
});
