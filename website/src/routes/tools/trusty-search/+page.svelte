<script lang="ts">
	import ToolPage from '$lib/components/ToolPage.svelte';
	import { TOOLS } from '$lib/tools';

	const tool = TOOLS.find((t) => t.slug === 'trusty-search')!;

	const facts = [
		{ label: 'Package', value: 'trusty-search' },
		{ label: 'Default port', value: '7878' },
		{ label: 'Languages parsed', value: '14 tree-sitter grammars' },
		{ label: 'MCP tools', value: '21' }
	];
</script>

<ToolPage {tool} {facts}>
	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Three lanes, one ranking</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			A grep knows the token you typed. An embedding knows what you meant. A symbol graph knows what
			calls what. trusty-search runs all three over the same corpus and merges them with Reciprocal
			Rank Fusion, so a query lands whether you spelled the identifier right or only described it.
		</p>
		<ul class="mt-6 max-w-3xl space-y-3 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Lexical.</span> A code-aware BM25 that
					splits
					<code class="text-sm">CodeIndexer</code>
					into <code class="text-sm">code</code> and <code class="text-sm">indexer</code>, so a
					half-remembered identifier still matches.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Vector.</span> An HNSW index over usearch, holding
					384-dimension embeddings produced locally — no text leaves the machine to be indexed.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Graph.</span> A petgraph symbol graph built from
					tree-sitter parses, walked one or two hops to pull in the callers and callees around a hit.</span
				>
			</li>
		</ul>
		<p class="mt-6 max-w-3xl text-foundry-secondary">
			Fusion uses a fixed damping constant of 60 — there is no relevance dial to tune wrong.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">The query picks the weighting</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Asking for a definition and asking a conceptual question want opposite rankings. A
			sub-millisecond regex classifier sorts every query into one of five intents and sets the
			vector/lexical weights accordingly, before any search runs.
		</p>
		<div class="doc-table mt-6 max-w-2xl">
			<table class="w-full border-collapse text-left text-sm">
				<thead class="bg-foundry-card">
					<tr>
						<th class="border-b-[1.5px] border-foundry-border px-3 py-2 font-semibold">Intent</th>
						<th class="border-b-[1.5px] border-foundry-border px-3 py-2 font-semibold">Vector</th>
						<th class="border-b-[1.5px] border-foundry-border px-3 py-2 font-semibold">Lexical</th>
						<th class="border-b-[1.5px] border-foundry-border px-3 py-2 font-semibold"
							>Graph-first</th
						>
					</tr>
				</thead>
				<tbody class="font-mono">
					<tr>
						<td class="border-b border-foundry-border px-3 py-2">Definition</td>
						<td class="border-b border-foundry-border px-3 py-2">0.3</td>
						<td class="border-b border-foundry-border px-3 py-2">0.7</td>
						<td class="border-b border-foundry-border px-3 py-2">—</td>
					</tr>
					<tr>
						<td class="border-b border-foundry-border px-3 py-2">Usage</td>
						<td class="border-b border-foundry-border px-3 py-2">0.5</td>
						<td class="border-b border-foundry-border px-3 py-2">0.5</td>
						<td class="border-b border-foundry-border px-3 py-2">yes</td>
					</tr>
					<tr>
						<td class="border-b border-foundry-border px-3 py-2">Conceptual</td>
						<td class="border-b border-foundry-border px-3 py-2">0.8</td>
						<td class="border-b border-foundry-border px-3 py-2">0.2</td>
						<td class="border-b border-foundry-border px-3 py-2">—</td>
					</tr>
					<tr>
						<td class="border-b border-foundry-border px-3 py-2">Bug / debt</td>
						<td class="border-b border-foundry-border px-3 py-2">0.1</td>
						<td class="border-b border-foundry-border px-3 py-2">0.9</td>
						<td class="border-b border-foundry-border px-3 py-2">—</td>
					</tr>
					<tr>
						<td class="px-3 py-2">Unknown</td>
						<td class="px-3 py-2">0.6</td>
						<td class="px-3 py-2">0.4</td>
						<td class="px-3 py-2">—</td>
					</tr>
				</tbody>
			</table>
		</div>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Graph expansion is gated to Usage, where caller and callee chains are what you actually asked
			for. Everywhere else it would just add noise.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">One daemon for the whole machine</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Install once, run one process, register as many named indexes as you have projects. Nothing is
			per-project except the index itself, and re-running an index is cheap: content fingerprints
			skip files that have not changed, so only the diff pays for embedding.
		</p>
		<ul class="mt-6 max-w-3xl space-y-2 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>Working on a branch? Pass it, and chunks from the files it touched get a 1.5× score
					multiplier — every result reports whether it was boosted.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>Don't need semantics? <code class="text-sm">--lexical-only</code> skips embedding entirely
					and leaves you a daemonised BM25.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>Don't need call chains? <code class="text-sm">--no-kg</code> skips the symbol-graph rebuild
					on every reindex.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>Memory limits — chunk caps, batch sizes, cache sizes — are computed from detected system
					RAM at startup rather than guessed at compile time.</span
				>
			</li>
		</ul>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Nothing is indexed until you say so</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			A fresh daemon accepts zero indexes. A path has to be added to the allowlist before it can be
			registered, whether the request arrives over HTTP, from the CLI, or from an MCP tool call.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			On top of that sits a denylist that the allowlist cannot override: credential directories such
			as <code class="text-sm">.ssh</code>, <code class="text-sm">.aws</code>,
			<code class="text-sm">.gnupg</code>
			and <code class="text-sm">.kube</code>, paths carrying secret markers, and the top level of
			your home directory. Those are refused with the matched pattern named in the error, not
			silently skipped.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">21 tools over MCP</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The MCP server speaks stdio and HTTP/SSE and exposes each retrieval lane separately, so an
			agent can pick the one that fits the question instead of always paying for the fused search.
		</p>
		<p class="mt-4 max-w-3xl font-mono text-sm text-foundry-secondary">
			search · search_lexical · search_semantic · search_kg · search_all · search_similar ·
			get_call_chain · grep · typeahead · index_file · remove_file · list_indexes · create_index ·
			delete_index · reindex · index_status · list_chunks · search_health · chat · console_metrics ·
			upgrade
		</p>
	</div>
</ToolPage>
