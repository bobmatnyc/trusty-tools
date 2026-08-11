Fixed

- the Sessions tab's auto-resume widget shows what the supervisor is actually doing (closes [#5208](https://github.com/bobmatnyc/trusty-tools/issues/5208))
  - the label and the Enable/Disable button read `desired` — the toggle's own saved value — so with no saved setting and a supervisor booted auto-resume-on (anyone who set `TRUSTY_MPM_AUTO_RESUME` or `--auto-resume` and never used the console) it read "off" beside an Enable button while the supervisor was resuming sessions
  - both now read the daemon's new `effective` field, and toggling sends the negation of what is in force rather than of what the file says
  - that case renders "on (env default)" to mark the value as coming from the supervisor's boot flag rather than a saved setting. With no saved setting the daemon infers it from its OWN environment, and the supervisor is a separate process that may not share it — a bound the tooltip states and the supervisor publishing its resolved flag on `/metrics` would close
  - an unreadable setting renders "unknown — cannot read setting" with the button disabled, instead of a confident "off"
  - the mapping moved out of the component into `src/autoResume.js` so it can be asserted directly: `node --test src/autoResume.test.js`
