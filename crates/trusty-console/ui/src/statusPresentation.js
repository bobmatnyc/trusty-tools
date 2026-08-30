/**
 * How one service card reads its status (#6416).
 *
 * Why: the card rendered one sentence for every `available` row — "Binary found
 * but daemon is not running." — in amber. trusty-review has had no daemon since
 * #6290 and trusty-analyze serves on demand since #6287/#6350, so for those two
 * `available` IS the healthy resting state and the card was reporting the
 * correct state as a fault, with remediation text for a daemon the operator
 * cannot start. `status` alone cannot separate the two readings; the payload's
 * `lifecycle` field does.
 *
 * What: one pure function, no DOM and no Svelte, so the wording rule is testable
 * without a browser and `cardDescribedBy` can ask it whether a hint exists
 * instead of keeping a second list of the statuses that render one.
 * Test: `statusPresentation.test.js` — run `node --test src/statusPresentation.test.js`
 * from `crates/trusty-console/ui`.
 */

/**
 * A service that runs per invocation or on demand rather than as a daemon.
 *
 * Matches `ServiceLifecycle::OnDemand`'s serde spelling in
 * `crates/trusty-console/src/connector.rs`. A payload written before #6416 has
 * no `lifecycle` key at all and reads as a daemon, which is what it was.
 */
const ON_DEMAND = 'on_demand';

/**
 * Is this service a per-invocation or on-demand tool?
 *
 * @param {{ lifecycle?: string }} service
 * @returns {boolean}
 */
export function isOnDemand(service) {
  return service?.lifecycle === ON_DEMAND;
}

/**
 * The badge label, badge color and hint sentence for one service card.
 *
 * Why: an on-demand member at rest is healthy, so it gets the success color and
 * a sentence that does not mention a daemon. A stopped daemon keeps the amber
 * badge and the sentence it had — that reading was never wrong for a daemon.
 *
 * What: `hint.kind` tells the markup which shape to render — `install` carries a
 * command the card sets in a `<code>`, the rest are plain sentences — and
 * `degraded` is the only one that takes the warning color. `null` means the card
 * shows no hint at all.
 *
 * @typedef {{ kind: 'install' | 'stopped' | 'on-demand' | 'degraded', text: string }} CardHint
 * @param {{ id?: string, status?: string, hint?: string, lifecycle?: string }} service
 * @returns {{ label: string, toneVar: string, hint: CardHint | null }}
 */
export function cardPresentation(service) {
  const status = service?.status;

  if (status === 'running') {
    return { label: 'Running', toneVar: 'var(--trusty-success)', hint: null };
  }

  if (status === 'absent') {
    return {
      label: 'Absent',
      toneVar: 'var(--trusty-status-absent)',
      hint: { kind: 'install', text: `cargo install ${service?.id ?? ''}` },
    };
  }

  if (status === 'degraded') {
    return {
      label: 'Degraded',
      toneVar: 'var(--trusty-status-degraded)',
      hint: {
        kind: 'degraded',
        text: service?.hint ?? 'Service reachable but its metrics tool is unavailable.',
      },
    };
  }

  if (status === 'available') {
    // #6416: the whole point of the lifecycle field. An on-demand member with
    // its binary installed has nothing left to start, so this is not a warning.
    return isOnDemand(service)
      ? {
          label: 'Ready',
          toneVar: 'var(--trusty-success)',
          hint: {
            kind: 'on-demand',
            text: 'Installed and ready. This tool runs on demand — there is no daemon to start.',
          },
        }
      : {
          label: 'Available',
          toneVar: 'var(--trusty-warning)',
          hint: { kind: 'stopped', text: 'Binary found but daemon is not running.' },
        };
  }

  // An unknown status is rendered verbatim rather than guessed at — the console
  // and the daemon can be different builds.
  return { label: String(status ?? ''), toneVar: 'var(--trusty-text-muted)', hint: null };
}
