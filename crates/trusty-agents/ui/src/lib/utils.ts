/**
 * Why: Centralizes small UI helper functions (relative time formatting,
 * GitHub URL derivation) that are used across multiple components. Keeping
 * them here avoids duplicating the parsing logic in ProjectsView, Sidebar,
 * etc. and makes them straightforward to unit-test in isolation.
 * What: Exports `relativeTime` (ISO timestamp -> "2h ago") and `githubUrl`
 * (git origin -> https://github.com/owner/repo[suffix]).
 * Test: Verify `relativeTime(new Date(Date.now()-3600000).toISOString())`
 * returns "1h ago", and `githubUrl('git@github.com:o/r.git', '/pulls')`
 * returns 'https://github.com/o/r/pulls'.
 */

/**
 * Why: Display "active 2h ago" rather than raw ISO timestamps so users can
 * scan project freshness at a glance.
 * What: Formats a delta between now and `iso` as a short human label.
 * Test: Pass an ISO 1 minute in the past -> '1m ago'; null -> 'never'.
 */
export function relativeTime(iso: string | null | undefined): string {
  if (!iso) return 'never';
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return 'never';
  const diff = Date.now() - t;
  if (diff < 0) return 'just now';
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return 'just now';
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.floor(hrs / 24)}d ago`;
}

/**
 * Why: Project cards link to GitHub for PRs/issues, but the `git_origin`
 * field can be either SSH or HTTPS form. We need a single helper that turns
 * either into a real https://github.com/owner/repo URL with an optional
 * suffix (e.g. '/pulls', '/issues').
 * What: Parses `git@github.com:owner/repo[.git]` or
 * `https://github.com/owner/repo[.git]` and returns the canonical https URL
 * with `suffix` appended; null when the origin isn't a github.com remote.
 * Test: `githubUrl('git@github.com:o/r.git')` -> 'https://github.com/o/r';
 * `githubUrl('https://github.com/o/r.git', '/pulls')` ->
 * 'https://github.com/o/r/pulls'; `githubUrl(null)` -> null.
 */
export function githubUrl(origin: string | null | undefined, suffix = ''): string | null {
  if (!origin) return null;
  // SSH: git@github.com:owner/repo.git
  const ssh = origin.match(/^git@github\.com:(.+?)(?:\.git)?$/);
  if (ssh) return `https://github.com/${ssh[1]}${suffix}`;
  // HTTPS: https://github.com/owner/repo.git
  const https = origin.match(/^https:\/\/github\.com\/(.+?)(?:\.git)?$/);
  if (https) return `https://github.com/${https[1]}${suffix}`;
  // ssh:// form: ssh://git@github.com/owner/repo.git
  const sshUrl = origin.match(/^ssh:\/\/git@github\.com\/(.+?)(?:\.git)?$/);
  if (sshUrl) return `https://github.com/${sshUrl[1]}${suffix}`;
  return null;
}
