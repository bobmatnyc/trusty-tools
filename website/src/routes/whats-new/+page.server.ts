/**
 * Why: the whole page is one build-time computation over files that live
 * ABOVE `website/`, exactly like the doc reader (`src/routes/docs/[...slug]`).
 * Prerendering bakes the result into static HTML, so the published page reads
 * no changelog at request time and opens no connection to GitHub.
 *
 * What: every release of every flagship, newest first. The recent ones arrive
 * with their items already rendered to HTML; older ones arrive as headings
 * only, because SvelteKit inlines this data for hydration on top of the
 * rendered markup and shipping all 234 in full made the page 2.4 MB. See
 * `DETAILED_RELEASES`. A crate that cannot be read, parses to nothing, or
 * whose newest release is empty stops the build in `$lib/changelog/site` —
 * this load has no branch that could serve a partial page instead.
 *
 * Test: `src/lib/changelog/site.test.ts`; `tests/build-smoke.test.ts` proves
 * the artifact exists and is self-contained.
 */

import { whatsNewSections } from '$lib/changelog/site';
import type { PageServerLoad } from './$types';

export const prerender = true;

export const load: PageServerLoad = () => whatsNewSections();
