<script lang="ts">
	import ToolPage from '$lib/components/ToolPage.svelte';
	import { TOOLS } from '$lib/tools';

	const tool = TOOLS.find((t) => t.slug === 'trusty-analyze')!;

	const facts = [
		{ label: 'Package', value: 'trusty-analyze' },
		{ label: 'Default port', value: '7879' },
		{ label: 'Languages', value: '14 tree-sitter adapters' },
		{ label: 'Requires', value: 'trusty-search on 7878' }
	];
</script>

<ToolPage {tool} {facts}>
	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">A sidecar, on purpose</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			trusty-analyze does not index anything. It pulls the chunk corpus trusty-search has already
			built, runs static analysis over it, and serves the results on its own port. One parse of your
			repository feeds both, and a crash in either does not take the other down.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			That coupling is explicit rather than best-effort: the analyzer health-checks trusty-search at
			startup and exits rather than come up half-useful. There is no offline mode to accidentally
			end up in.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">What it measures</h2>
		<ul class="mt-6 max-w-3xl space-y-3 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Complexity.</span> Cyclomatic and cognitive scores
					per chunk, per file, and aggregated per index — the second because branch counting alone rewards
					code that is short and unreadable.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Smells.</span> Named categories — long functions,
					deep nesting, too many parameters — each with a threshold you can move rather than a hard-coded
					opinion.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Grades.</span> An A-to-F letter per file and
					per index, for the times a number is more argument than signal.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Age.</span> A temporal-decay score over git blame,
					with a half-life of about ten weeks. Complex code touched yesterday is being worked on; complex
					code nobody has touched in a year is the one to worry about.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Structure.</span> Concept clusters over the corpus,
					entity extraction, SCIP protobuf ingest for symbol data an LSP already computed, and a facts
					store of subject/predicate/object triples persisted locally.</span
				>
			</li>
		</ul>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Fourteen languages, one shape</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Tree-sitter adapters cover Rust, Python, TypeScript, JavaScript, Java, Go, Ruby, PHP, C, C++,
			C#, Kotlin, Swift, and Scala. They all produce the same metric shape, so a polyglot repository
			gets one comparable report rather than a per-language dialect of the truth.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Two ways in</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			An HTTP API serves complexity hotspots, smells, quality grades, clusters, and the facts store;
			an MCP server exposes the same analysis to an agent over stdio or SSE. A deep-analysis pass
			will additionally write a prose narrative over an analyzed index, routed through OpenRouter or
			AWS Bedrock depending on the model id you configure — it is opt-in, and nothing else in the
			crate calls an LLM.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The default build links no ONNX runtime and downloads no model. One install command works on
			every supported host.
		</p>
	</div>
</ToolPage>
