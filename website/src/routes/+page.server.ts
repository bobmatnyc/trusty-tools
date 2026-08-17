/**
 * Why: the landing page's flagship cards each carry a "What's new" strip, and
 * the version in it is derived from the crate's own `CHANGELOG.md` at BUILD
 * time. Reading it here rather than in the component keeps the page a pure
 * render, and keeps the only filesystem access on the prerender path.
 *
 * What: the newest release of each RELEASED flagship, flattened to at most
 * three lines. A crate whose changelog is missing, unparseable, or empty stops
 * the build in `$lib/changelog/site` — nothing here substitutes a placeholder,
 * so every strip a page renders is populated. A flagship with no release yet
 * (`Tool.released`) produces no strip at all, and its card omits the block.
 *
 * Test: `src/lib/changelog/site.test.ts` (`the landing-page strip`).
 */

import { buildChangelogSite, stripItems } from '$lib/changelog/site';
import type { PageServerLoad } from './$types';

export const prerender = true;

export const load: PageServerLoad = () => ({
	strips: buildChangelogSite().crates.map((crate) => ({
		name: crate.name,
		version: crate.latest.version,
		date: crate.latest.date,
		items: stripItems(crate.latest)
	}))
});
