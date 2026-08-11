/**
 * Auto-resume control state derivation for the Sessions tab (#5208).
 *
 * Why: the supervisor's auto-resume flag has three inputs — the console's saved
 * setting, the supervisor's boot flag, and the file being unreadable — and the
 * widget got it wrong by reading only the saved setting. With no saved setting
 * and a supervisor booted auto-resume-on, `desired` is false while the
 * supervisor is actively resuming, so the label read "off" beside an "Enable"
 * button. These live outside the component so the mapping can be asserted
 * directly rather than through a rendered DOM.
 * What: `autoResumeEffective` returns what the supervisor's next sweep will do
 * (`null` when unknown); `autoResumeLabel` renders that for a human.
 * Test: `autoResume.test.js` — run `node --test src/autoResume.test.js`.
 */

/**
 * What the supervisor's next sweep will actually do.
 *
 * Returns `true`/`false`, or `null` when unknown — either no supervisor block
 * yet, or the daemon could not read the desired-state file. `effective` is
 * absent on daemons older than #5208, which fall back to the saved setting.
 */
export function autoResumeEffective(supervisor) {
  const ar = supervisor?.auto_resume;
  if (!ar) return null;
  if (ar.read_error) return null;
  return ar.effective ?? ar.desired ?? null;
}

/**
 * Human-readable auto-resume state.
 *
 * "on (env default)" marks the case where auto-resume is in force but no saved
 * setting exists, so the value came from the supervisor's boot flag — which the
 * daemon infers from its OWN environment and may not share with the supervisor
 * process. Any value the operator has actually toggled reads as plain on/off.
 */
export function autoResumeLabel(supervisor) {
  const ar = supervisor?.auto_resume;
  if (!ar) return '—';
  if (ar.read_error) return 'unknown — cannot read setting';
  const eff = autoResumeEffective(supervisor);
  if (eff === null) return '—';
  if (eff && !ar.desired) return 'on (env default)';
  return eff ? 'on' : 'off';
}
