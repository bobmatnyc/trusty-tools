<script lang="ts">
	import ToolPage from '$lib/components/ToolPage.svelte';
	import { TOOLS } from '$lib/tools';

	const tool = TOOLS.find((t) => t.slug === 'trusty-memory')!;

	const facts = [
		{ label: 'Package', value: 'trusty-memory' },
		{ label: 'Default port', value: '7070' },
		{ label: 'MCP tools', value: '45' },
		{ label: 'Storage', value: 'usearch + redb, on disk' }
	];
</script>

<ToolPage {tool} {facts}>
	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">A place to put what was learned</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			An assistant that forgets everything at the end of a session relearns your codebase every
			morning. trusty-memory is the store that stops that: an MCP server over a local vector index
			and a key-value store, where an agent writes what it worked out and recalls it by meaning
			later.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Memories are organised into named <em>palaces</em> — one per project — with rooms and wings
			inside them. The naming is deliberate: a palace is anchored to a real project directory, so
			the memories for one repository can never quietly bleed into another's recall. Work outside
			any project and there is a single <code class="text-sm">personal</code> palace for the notes that
			belong to you rather than to a codebase.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Recall that does not need the words</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Recall is hybrid — lexical BM25 alongside vector similarity — so a query finds a memory that
			said the same thing differently. Embeddings are computed on the machine, and both the vector
			index and the metadata store are ordinary local files. Nothing is shipped to a hosted memory
			service.
		</p>
		<ul class="mt-6 max-w-3xl space-y-2 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">memory_remember</code> and
					<code class="text-sm">memory_recall</code>
					are the whole day-to-day surface; <code class="text-sm">memory_recall_deep</code> trades latency
					for reach when the fast lane comes up short.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>A knowledge-graph layer stores subject/predicate/object triples next to the prose, so
					structured facts can be asserted and queried directly rather than fished back out of a
					paragraph.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					>A chat-session store keeps conversation turns verbatim, bypassing the signal filters that
					apply to ordinary memories — a transcript is not a fact and should not be deduplicated
					like one.</span
				>
			</li>
		</ul>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">The dream cycle</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			A memory store that only ever accumulates degrades into a landfill. trusty-memory runs a
			consolidation pass — the dream cycle — that merges near-duplicates, prunes what has gone
			stale, and, when an inference backend is configured, summarises a room's older facts into
			canonical entries and links the originals to their replacement so the lineage stays traceable.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Task drawers — goals, milestones, checkpoints an application must re-derive across sessions —
			are exempt. They are never evicted and never consolidated away, however old they get, until
			something deletes them explicitly.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Wiring it up</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">trusty-memory setup</code> installs the background service, warms the embedding
			model cache, and patches the Claude settings files it finds with the right server entry. From then
			on the daemon is just there.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			For a manual configuration, the canonical entry runs
			<code class="text-sm">trusty-memory serve --stdio</code>, which forwards every request to the
			running HTTP daemon and returns its answers verbatim. The stdio process never opens the
			database itself, so it coexists safely with the daemon and with other clients. The same daemon
			serves a REST API and an embedded browser dashboard on the port it bound.
		</p>
	</div>
</ToolPage>
