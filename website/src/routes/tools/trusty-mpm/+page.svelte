<script lang="ts">
	import ToolPage from '$lib/components/ToolPage.svelte';
	import { TOOLS } from '$lib/tools';

	const tool = TOOLS.find((t) => t.slug === 'trusty-mpm')!;

	const facts = [
		{ label: 'Package', value: 'trusty-mpm' },
		{ label: 'Binaries', value: 'tm, trusty-mpm' },
		{ label: 'Surfaces', value: 'CLI, daemon, TUI, MCP' },
		{ label: 'Remote', value: 'Telegram, Slack' }
	];
</script>

<ToolPage {tool} {facts}>
	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">
			One binary, one daemon, many sessions
		</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Running one coding session is easy. Running five across three repositories, remembering which
			worktree each belongs to and which are still waiting on you, is the part that goes wrong.
			trusty-mpm is the process that keeps that straight.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Everything ships as a single binary installed under two names — <code class="text-sm">tm</code
			>
			and <code class="text-sm">trusty-mpm</code> — with each surface behind a subcommand rather than
			a separate package to install and version-match.
		</p>
		<ul class="mt-6 max-w-3xl space-y-2 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tm daemon</code> — the background service everything else talks to.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tm tui</code> — a terminal dashboard across every live session.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tm telegram</code> and <code class="text-sm">tm slack</code> — remote
					control from a phone when you are not at the terminal.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tm gui</code> — an optional desktop shell over the same daemon.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><code class="text-sm">tm wait</code> — poll a condition rather than sleep on it:
					<code class="text-sm">--for run</code> until a process exits,
					<code class="text-sm">--for file</code> until a sentinel appears (optionally containing a
					string), <code class="text-sm">--for check</code> until a pull request's checks settle. It exits
					0 when the condition holds and 75 while it is still pending, so an agent whose turn has a ceiling
					re-runs the identical command instead of losing the wait.</span
				>
			</li>
		</ul>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Sessions you can walk away from</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">tm launch</code> provisions a session with everything it needs already
			deployed — instructions, agent roster, skills — and starts it.
			<code class="text-sm">tm ls</code>
			and <code class="text-sm">tm f</code> find one again by name or by prefix;
			<code class="text-sm">tm attach</code> reconnects. The daemon holds the roster, so closing a terminal
			does not lose the session behind it.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Projects are registered rather than inferred, which is what makes the rest work: the same
			directory resolves to the same project every time, across sessions and across restarts.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">
			Hooked into the session, not beside it
		</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">tm hook</code> handles Claude Code lifecycle events — before a tool
			runs, after it runs, and when a session stops — and relays them to the daemon. That event feed
			is what makes the dashboards live rather than a periodic poll, and it is readable directly
			with
			<code class="text-sm">tm events</code>.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			An MCP server exposes the same orchestration surface to a session itself, so an agent can
			enumerate sessions, delegate work, and read project state through tool calls rather than by
			shelling out.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Coming from claude-mpm</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			trusty-mpm is not a fork or a version of the Python <code class="text-sm">claude-mpm</code> —
			unrelated codebases that share an idea. If you already run one, the
			<a
				href="/claude-mpm-migration"
				class="text-foundry-primary underline decoration-1 underline-offset-2 hover:decoration-2"
				>migration guide</a
			> covers what installs, what carries over, and what genuinely behaves differently.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">When it goes wrong</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">tm doctor</code> runs a full diagnostic of the stack.
			<code class="text-sm">tm validate</code>
			checks a workspace's deployed agents, skills, and settings against the canonical roster, and
			<code class="text-sm">tm repair</code> recovers from a deploy state that has drifted.
			<code class="text-sm">tm health</code> reports daemon reachability and a fleet summary in one line.
		</p>
	</div>
</ToolPage>
