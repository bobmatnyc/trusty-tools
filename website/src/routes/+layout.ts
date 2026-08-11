/**
 * Why: this is a marketing and documentation site with no per-request state,
 * so every route should be HTML on a CDN rather than a serverless invocation.
 * Prerendering at the root makes that the default for the doc reader (#5098)
 * too — its pages inherit this and need no separate opt-in.
 * What: prerender everything; no client-side router state is needed on first
 * paint, so `ssr` stays on and `csr` is left at its default for the theme
 * toggle to hydrate.
 */
export const prerender = true;
