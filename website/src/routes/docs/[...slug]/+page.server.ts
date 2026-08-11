/**
 * Why: this is the ONLY route that serves documentation, and it is where the
 * publication boundary is enforced. `entries` is generated from the manifest,
 * so the prerenderer emits exactly one file per manifest row and nothing else;
 * a path the manifest does not name has no output, and `load` has no lookup
 * that could find one either. There is deliberately no catch-all that reads a
 * path from the filesystem.
 *
 * What: resolves one rest-parameter slug to its prebuilt page. The rest
 * parameter also matches the EMPTY slug, so `/docs` serves the manifest's `/`
 * route (the Introduction) rather than a separate index that would duplicate
 * the sidebar.
 *
 * Test: `src/lib/docs/site.test.ts` — `entries` covers exactly the manifest, an
 * unlisted source resolves to no slug, and `tests/build-smoke.test.ts` proves
 * the same at the artifact level.
 */

import { error } from '@sveltejs/kit';
import type { EntryGenerator, PageServerLoad } from './$types';
import { buildDocSite, buildDocSiteIfAvailable } from '$lib/docs/site';

export const prerender = true;

/**
 * Without this the prerenderer only discovers routes something already links
 * to, which would silently drop any page the nav or a cross-link misses.
 */
export const entries: EntryGenerator = () =>
	buildDocSite().pages.map((page) => ({ slug: page.slug }));

export const load: PageServerLoad = ({ params }) => {
	const page = buildDocSiteIfAvailable()?.bySlug.get(params.slug);
	if (!page) error(404, 'That documentation page is not published.');
	return { page };
};
