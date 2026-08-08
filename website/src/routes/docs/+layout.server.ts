/**
 * Why: the sidebar is the same on all published pages, so it belongs to the
 * layout rather than being rebuilt and re-serialised into every page payload.
 * What: the manifest's section/page tree, in manifest order. Server-side
 * because `buildDocSite()` reads files above `website/`, and prerendered
 * (inherited from `src/routes/+layout.ts`) so that read happens at build time
 * and never on a request. An empty tree is what the catchall function returns
 * when there is no repository to read — see `buildDocSiteIfAvailable`.
 * Test: `src/lib/docs/site.test.ts` covers the tree this returns.
 */

import { buildDocSiteIfAvailable } from '$lib/docs/site';

export function load() {
	const site = buildDocSiteIfAvailable();
	return { nav: site?.nav ?? [], commitSha: site?.commitSha ?? '' };
}
