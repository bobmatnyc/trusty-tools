<script lang="ts">
	import ToolPage from '$lib/components/ToolPage.svelte';
	import { TOOLS } from '$lib/tools';

	const tool = TOOLS.find((t) => t.slug === 'trusty-git-analytics')!;

	const facts = [
		{ label: 'Package', value: 'tga' },
		{ label: 'Binary', value: 'tga' },
		{ label: 'Store', value: 'SQLite, on disk' },
		{ label: 'Output', value: 'CSV, JSON, Markdown' }
	];
</script>

<ToolPage {tool} {facts}>
	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Three stages, one command</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">tga analyze</code> runs the whole pipeline. Each stage is also a subcommand,
			because on a large history you will want to re-run one without paying for the others.
		</p>
		<ul class="mt-6 max-w-3xl space-y-3 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tga collect</code> — walk each configured repository, extract commit
					metadata and diff stats, resolve author identities, and write it all to SQLite. Optionally pull
					pull-request and issue metadata from GitHub, JIRA, Linear, or Azure DevOps alongside it.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tga classify</code> — run every unclassified commit through the cascade
					and write the verdict back. Rule tiers run in parallel.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tga report</code> — aggregate per author, per week, and per DORA metric,
					then write CSV, JSON, and Markdown into the output directory.</span
				>
			</li>
		</ul>
		<p class="mt-6 max-w-3xl text-foundry-secondary">
			The database is a local SQLite file, so every number in a report is one you can go and check
			with a query.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">The classification cascade</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Naming what a commit actually did is the hard part, and a single heuristic gets it wrong often
			enough to be useless. tga tries tiers in order and takes the first confident answer: a manual
			override you pinned, the issue type from a linked ticket, a project-key mapping, an
			Aho-Corasick scan for conventional-commit prefixes, regex patterns, a weighted sum over
			several independent signals, and fuzzy heuristics for merges and reverts.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			An LLM tier sits at the end for the commits the rules could not place, disabled by default and
			enabled with <code class="text-sm">--use-llm</code>. Its answers are accepted only above a
			confidence threshold you set. <code class="text-sm">--no-external</code> skips every network-bound
			source, which is what you want while iterating on a rule file.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The rule set is introspectable rather than a black box: <code class="text-sm"
				>tga rules list</code
			>
			enumerates it, <code class="text-sm">tga rules test "&lt;message&gt;"</code> shows you which
			tier would fire, and <code class="text-sm">tga override</code> pins a verdict that outranks all
			of them.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">What comes out</h2>
		<ul class="mt-6 max-w-3xl space-y-2 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tga author &lt;email&gt;</code> — a per-engineer drill-down: commits,
					effort, pull requests, category mix.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tga pr-metrics</code> — pull-request metrics per engineer, once PR fetching
					is turned on.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tga dora</code> — all four DORA metrics, fed by
					<code class="text-sm">tga deployments</code> and
					<code class="text-sm">tga incidents</code>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tga aliases</code> — merge the four email addresses one person has committed
					under, so the per-author numbers mean anything at all.</span
				>
			</li>
		</ul>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Getting started</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">tga install</code> is an interactive wizard that writes the config for
			you. A hand-written one can be as small as a list of repository paths — every other section
			has a default. Note the package and binary are both <code class="text-sm">tga</code>, not the
			crate directory's longer name.
		</p>
	</div>
</ToolPage>
