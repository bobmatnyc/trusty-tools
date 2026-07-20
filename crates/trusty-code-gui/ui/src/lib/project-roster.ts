// Why: `ProjectPickerModal.svelte` (issue #3365 — the GUI's
// workstream-first project-picker modal, replacing the inline
// browse-and-descend folder list `CreateSessionForm.svelte` used to render)
// needs typed wire shapes and a thin transport for `GET /projects`
// (mirrors `crate::fs_browse::roster::{ProjectRoster, ProjectCandidate}`
// field-for-field). Split out the same way `workstreams.ts` splits its
// `fetchWorkstreams`/etc. transport from `WorkstreamSwitcher.svelte` — the
// modal is the first component with its own dedicated roster fetch, worth
// making independently testable rather than mockable only by mounting the
// full component.
// What: [`ProjectCandidate`]/[`ProjectRoster`] mirror the daemon's wire
// shape; [`fetchProjectRoster`] is a thin `fetch()` wrapper over
// `GET /projects` with a runtime shape guard ([`isProjectRoster`]) on the
// response body — same rationale as `create-session.ts::isDirListing`'s
// per-field structural check (PR #3103 HIGH finding): a `200` status is a
// promise from THIS daemon version's handler, not from whatever actually
// answered the socket, so the shape must be verified before it becomes
// reactive state. [`projectCandidateLabel`] is the one-line display label
// the modal's row buttons render.
// Test: `project-roster.test.ts`.

/** Mirrors `crate::fs_browse::roster::ProjectCandidate` field-for-field. */
export interface ProjectCandidate {
  name: string;
  path: string;
  owner: string | null;
}

/** Mirrors `crate::fs_browse::roster::ProjectRoster` field-for-field — the
 * `GET /projects` response body. */
export interface ProjectRoster {
  entries: ProjectCandidate[];
}

/**
 * Runtime shape guard for a `GET /projects` 200 response body.
 *
 * Why: see the module doc's PR #3103 precedent — a bare `as ProjectRoster`
 * assertion would let a shape-invalid 200 (schema drift, an interposed
 * proxy, a future daemon) flow into `roster.entries` and throw once the
 * modal iterates it.
 * What: structural check of every field the modal dereferences: `entries`
 * is an array whose members each carry string `name`/`path` and an
 * `owner` that is a string or `null`.
 * Test: `project-roster.test.ts::isProjectRoster`.
 */
export function isProjectRoster(body: unknown): body is ProjectRoster {
  if (typeof body !== 'object' || body === null) return false;
  const b = body as Record<string, unknown>;
  if (!Array.isArray(b.entries)) return false;
  return b.entries.every((e: unknown) => {
    if (typeof e !== 'object' || e === null) return false;
    const c = e as Record<string, unknown>;
    return (
      typeof c.name === 'string' &&
      typeof c.path === 'string' &&
      (c.owner === null || typeof c.owner === 'string')
    );
  });
}

/**
 * `GET /projects` -> [`ProjectRoster`].
 *
 * Why: the modal's one network call, opened whenever it becomes visible.
 * What: throws a plain `Error` on a non-OK status or a shape-invalid body
 * (via [`isProjectRoster`]) — the caller (`ProjectPickerModal.svelte`)
 * degrades both to the same "daemon unreachable" state, matching every
 * other poller/fetcher in this codebase's generic error handling.
 * Test: `project-roster.test.ts::fetchProjectRoster`.
 */
export async function fetchProjectRoster(
  base: string,
  signal?: AbortSignal,
): Promise<ProjectRoster> {
  const res = await fetch(`${base}/projects`, { signal });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const body: unknown = await res.json();
  if (!isProjectRoster(body)) throw new Error('malformed response from daemon');
  return body;
}

/**
 * Display label for one roster row.
 *
 * Why: a candidate found under `~/trusty-mpm-projects/<owner>/<repo>`
 * carries an `owner`, which disambiguates two repos that share a name
 * under different owners; a candidate found via the flat home-directory
 * fallback carries `owner: null` (see `fs_browse::roster` module docs for
 * the two scan layouts).
 * What: `"<owner>/<name>"` when `owner` is present, else bare `name`.
 * Test: `project-roster.test.ts::projectCandidateLabel`.
 */
export function projectCandidateLabel(candidate: ProjectCandidate): string {
  return candidate.owner ? `${candidate.owner}/${candidate.name}` : candidate.name;
}
