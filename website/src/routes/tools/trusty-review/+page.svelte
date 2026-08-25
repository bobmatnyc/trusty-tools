<script lang="ts">
	import ToolPage from '$lib/components/ToolPage.svelte';
	import { TOOLS } from '$lib/tools';

	const tool = TOOLS.find((t) => t.slug === 'trusty-review')!;

	const facts = [
		{ label: 'Package', value: 'trusty-review' },
		{ label: 'Default port', value: '7880' },
		{ label: 'Providers', value: 'AWS Bedrock, OpenRouter' },
		{ label: 'MCP tools', value: 'review_pr, review_diff, review_health' }
	];
</script>

<ToolPage {tool} {facts}>
	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Context first, opinion second</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			A model handed a bare diff will find style nits and miss the thing that matters, because the
			caller it breaks is in a file it never saw. trusty-review fixes the input rather than the
			prompt: it retrieves code context from trusty-search and complexity data from trusty-analyze
			before the reviewer model is called at all.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			That is also why it will refuse. A review produced without that context is worse than no
			review — it reads exactly like a real one. When a required dependency is unreachable, a hosted
			review is skipped and the caller is told the reason, in a shape distinct from any verdict so
			it cannot be mistaken for one.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Point it at anything</h2>
		<ul class="mt-6 max-w-3xl space-y-2 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>A GitHub pull request: <code class="text-sm">trusty-review run owner repo 123</code
					>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>A ref range, with no manual diff step:
					<code class="text-sm">trusty-review run --base origin/main</code>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>A patch on stdin:
					<code class="text-sm">git diff origin/main...HEAD | trusty-review run --local-diff -</code
					>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>A checkout the search daemon has never seen, via
					<code class="text-sm">--source-root</code>.</span
				>
			</li>
		</ul>
		<p class="mt-6 max-w-3xl text-foundry-secondary">
			Only a GitHub PR review can post a comment, and only once you turn dry-run off. Every other
			source is dry-run by construction — a local diff cannot reach your repository's review thread
			however it is invoked.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">trusty-review compare</code> runs the same diff past several models at once,
			which is the honest way to decide whether a cheaper reviewer is good enough for your codebase.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">A verdict, not a wall of prose</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Every review returns a letter grade, a verdict — APPROVE, APPROVE with reservations,
			REQUEST_CHANGES, BLOCK, or UNKNOWN when the diff was too truncated to judge — and findings
			carrying their own severity and confidence. The verdict is derived from the grade, then
			clamped so it can never come out weaker than the findings' own severity floor already
			requires. Token counts and an estimated cost ride along in the footer.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			UNKNOWN exists deliberately. A reviewer that cannot see enough to form an opinion should say
			so, not approve.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Standards that live in the repo</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Drop a <code class="text-sm">.trusty-review.toml</code> at your repository root and every contributor
			and CI run picks up the same review standards with no per-machine setup — a voice package, and optionally
			a named template that appends extra scrutiny on top of the stock rubric. Template names are validated
			as bare identifiers precisely because that file is attacker-controlled: any PR author can add one.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			A template only appends. It never replaces the grade scale, the verdict table, or the severity
			anchors, so a project cannot quietly redefine what BLOCK means.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Due-diligence reports</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">trusty-review report --manifest &lt;file&gt;</code> generates a structured
			technical due-diligence report across one or more repositories: executive summary, per-application
			scorecards, findings by severity, and graph-ready data appendices in Markdown and JSON.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The default run is fully deterministic — measured from the checkouts themselves, no model
			involved. <code class="text-sm">--synthesize</code> layers LLM prose over the summary and the non-healthy
			findings only, and fails closed to the deterministic output rather than emit a partially-trusted
			result. Every value carries a marker saying whether it was measured, declared, or inferred, and
			a figure that appears nowhere in the underlying data is rejected before it can reach the page.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The data appendix is not left as pipe tables alone. Each populated dataset carries a declared
			chart type, and the renderer turns it into a Mermaid chart under its table — an
			<code class="text-sm">xychart-beta</code> for bar and stacked-bar data, a
			<code class="text-sm">radar-beta</code> for radar. That pass is pure rendering from the rows already
			in the table: no model, no network. The table stays the authoritative source and the chart is a
			derived view of it, so a dataset that stayed empty simply gets no chart.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">--analyze</code> fills the complexity sections from a running trusty-analyze
			daemon — the complexity distribution and the RED/AMBER finding bands, mapped from the daemon's own
			measurements rather than from prose. It fails open per dataset: whatever answered is kept and whatever
			did not is named under Gaps &amp; Caveats, and a run where the daemon is absent falls back to the
			built-in scan and produces the same output a run without the flag would.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Telling it what to look for</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			A report can carry an analyst brief: a free-form markdown file naming the focus areas, the
			concerns to chase, and the questions this particular review has to answer. Pass it with
			<code class="text-sm">--instructions &lt;path&gt;</code>, name it under the manifest's
			<code class="text-sm">[report].instructions</code> key, or drop a file called
			<code class="text-sm">instructions.md</code> next to
			<code class="text-sm">manifest.toml</code> and it is picked up with no flag and no key. Those three
			are a precedence order, highest first.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The brief is recorded verbatim in the report as its Analyst Instructions section, so every
			report says what it was asked to look for. Under
			<code class="text-sm">--synthesize</code> it also steers where the prose puts its emphasis.
			What it cannot do is loosen a guard: a figure the numeric guardrail cannot trace back to the
			collected data is withheld whether or not the brief asked for it, and the report discloses the
			withholding. Leave the file out and the run is unchanged.
			<a class="underline hover:text-foundry-text" href="/docs/guides/audit-instructions"
				>The instructions.md guide</a
			> has the mechanism and a template to start from.
		</p>
	</div>
</ToolPage>
