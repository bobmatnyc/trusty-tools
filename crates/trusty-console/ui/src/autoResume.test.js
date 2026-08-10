/**
 * Tests for the Sessions-tab auto-resume state mapping (#5208).
 *
 * Run: `node --test src/autoResume.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { autoResumeEffective, autoResumeLabel } from './autoResume.js';

const sup = (auto_resume) => ({ fleet: {}, auto_resume });

test('env-enabled supervisor with no saved setting reads as on, not off', () => {
  // The defect: anyone who set TRUSTY_MPM_AUTO_RESUME or --auto-resume and never
  // touched the console saw "off" with an Enable button while the supervisor was
  // actively resuming sessions.
  const s = sup({ desired: false, env: true, effective: true, read_error: null });
  assert.equal(autoResumeEffective(s), true);
  assert.equal(autoResumeLabel(s), 'on (env default)');
});

test('toggling from the env-default state asks to disable, not enable', () => {
  const s = sup({ desired: false, env: true, effective: true, read_error: null });
  // This is what `toggleAutoResume` sends as `enabled`.
  assert.equal(!autoResumeEffective(s), false, 'the button must turn it OFF');
});

test('a saved setting reads as plain on/off', () => {
  const on = sup({ desired: true, env: false, effective: true, read_error: null });
  assert.equal(autoResumeLabel(on), 'on');
  assert.equal(!autoResumeEffective(on), false);

  const off = sup({ desired: false, env: true, effective: false, read_error: null });
  assert.equal(autoResumeLabel(off), 'off');
  assert.equal(!autoResumeEffective(off), true);
});

test('nothing set anywhere reads as off', () => {
  const s = sup({ desired: false, env: false, effective: false, read_error: null });
  assert.equal(autoResumeLabel(s), 'off');
});

test('an unreadable setting reads as unknown, never as a confident off', () => {
  const s = sup({
    desired: null,
    env: true,
    effective: null,
    read_error: 'Is a directory (os error 21)',
  });
  assert.equal(autoResumeEffective(s), null);
  assert.equal(autoResumeLabel(s), 'unknown — cannot read setting');
});

test('a daemon older than #5208 falls back to the saved setting', () => {
  // No `effective` / `read_error` keys on the wire.
  const s = sup({ desired: true, env: false, pending_restart: false });
  assert.equal(autoResumeEffective(s), true);
  assert.equal(autoResumeLabel(s), 'on');
});

test('no supervisor block renders the placeholder', () => {
  assert.equal(autoResumeEffective(null), null);
  assert.equal(autoResumeLabel(null), '—');
  assert.equal(autoResumeLabel({ fleet: {} }), '—');
});
