/**
 * Why: one route serves every markdown-driven flagship page. Six per-tool
 * `+page.server.ts` files would be six copies of this eight-line load, and the
 * whole point of the markdown migration (#6960) is that adding or changing a
 * flagship page touches no `.ts` and no `.svelte`.
 *
 * What: resolves one slug to the HTML `$lib/flagship/content` rendered from
 * `website/src/content/tools/<slug>.md` at build time. `entries` is generated
 * from that content map, so the prerenderer emits exactly one file per markdown
 * source — a slug with no markdown file (`trusty-audit`, whose copy embeds live
 * components) keeps its own static route and never reaches here.
 *
 * Test: `src/lib/flagship/content.test.ts` covers the build; the artifacts and
 * the rendered body are pinned by `tests/build-smoke.test.ts`.
 */

import { error } from '@sveltejs/kit';
import type { EntryGenerator, PageServerLoad } from './$types';
import { buildFlagshipContent, buildFlagshipContentIfAvailable } from '$lib/flagship/content';
import { TOOLS } from '$lib/tools';

export const prerender = true;

export const entries: EntryGenerator = () =>
	[...buildFlagshipContent().keys()].map((slug) => ({ slug }));

export const load: PageServerLoad = ({ params }) => {
	const content = buildFlagshipContentIfAvailable()?.get(params.slug);
	const tool = TOOLS.find((candidate) => candidate.slug === params.slug);
	if (!content || !tool) error(404, 'That tool page does not exist.');
	return { tool, html: content.html };
};
