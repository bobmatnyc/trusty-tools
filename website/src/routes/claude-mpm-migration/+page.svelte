<script lang="ts">
	/**
	 * Why: people arrive at this site already running the Python `claude-mpm`
	 * and want one page that answers "what changes if I switch". The `/tools/`
	 * pages describe trusty-mpm to someone who has never seen either tool, and
	 * `/docs/` is generated from the manifest, so neither is that page. It is
	 * top-level rather than nested under `/tools/trusty-mpm` because the reader
	 * is choosing between two tools, not reading about one.
	 *
	 * What: the whole migration page. Not built on `ToolPage.svelte`, for the
	 * same reason `/tools/trusty-git-analytics/audit` is not: that component
	 * derives its `<h1>`, source link, and single install line from a `Tool`
	 * record, and this page is about a move between two projects rather than
	 * about a crate. The chrome below matches it by hand.
	 *
	 * Sourcing rule, inherited from `$lib/tools`: every claim was checked
	 * against repository source, never against a README sentence.
	 *
	 *   - the disambiguation, the roster, and the `outputStyle` check —
	 *     `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md`
	 *   - `tm launch` deploying the full sequence then attaching, and every
	 *     other verb named here — `crates/trusty-mpm/src/bin/tm/cli/mod.rs`
	 *   - tmux session naming and the 1:1 worktree map —
	 *     `crates/trusty-mpm/src/session_manager/naming.rs` and
	 *     `crates/trusty-mpm/docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md`
	 *   - `CLAUDE_CONFIG_DIR` excluding the real `~/.claude` —
	 *     `crates/trusty-mpm/src/core/managed_config.rs`
	 *   - which daemons launchd supervises and which run their own verb —
	 *     `crates/trusty-installer/src/commands/stable_set.rs`. trusty-mpm is
	 *     NOT launchd-managed; `docs/architecture/port-assignments.md` says it
	 *     is, and the code is the one that decides.
	 *   - ports and the loopback guards — `docs/architecture/port-assignments.md`
	 *     and `docs/reference/threat-model.md`
	 *   - the kuzu-memory targets, their required flags, and the idempotency
	 *     claims — `crates/trusty-memory/src/commands/migrate.rs` and
	 *     `src/main.rs`'s `Migrate` variant; the importer's behaviour and the
	 *     four refused predicates —
	 *     `crates/trusty-memory/src/commands/kuzu_migrate.rs` and
	 *     `crates/trusty-memory/src/prompt_facts.rs::HOT_PREDICATES`, pinned by
	 *     `kuzu_migrate_refuses_hot_predicates_and_passes_cold_ones`. There is
	 *     no default `--from` path: the handler errors when it is absent, so
	 *     the `~/.open-mpm/...` path in the example is kuzu-memory's own
	 *     convention rather than a default this command applies.
	 *
	 * claude-mpm claims are deliberately general — a Python package on PyPI
	 * that discovers everything through the real `~/.claude`. Nothing about its
	 * internals is asserted here, because none of it is checkable from this
	 * repository.
	 *
	 * No version number appears in the copy, for the reason `$lib/site.ts`
	 * gives: a version typed into this file is stale the next time tm ships.
	 *
	 * Test: `tests/build-smoke.test.ts` asserts this route prerenders with its
	 * disambiguation and install lines intact and is reachable from the nav;
	 * `tests/mobile-overflow.test.ts` measures it at 375px and 320px.
	 */
	import { GITHUB_URL } from '$lib/site';

	const IDENTITY_DOC = `${GITHUB_URL}/blob/main/crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md`;

	const description =
		'Moving off the Python claude-mpm and onto trusty-mpm (tm): what installs, what carries over, and what genuinely behaves differently — the daemon fleet, tmux-hosted sessions, and per-project agent deployment.';

	const facts = [
		{ label: 'Binary', value: 'tm, trusty-mpm' },
		{ label: 'Install', value: 'tctl install trusty-mpm' },
		{ label: 'Daemons', value: 'mpm, memory, search' },
		{ label: 'Platforms', value: 'macOS 12+, Linux' }
	];
</script>

<svelte:head>
	<title>Coming from claude-mpm — migrating to trusty-mpm — trusty-tools</title>
	<meta name="description" content={description} />
</svelte:head>

<!-- HERO -->
<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">Migration guide · claude-mpm → trusty-mpm</p>
		<h1
			class="mt-4 break-words font-display text-4xl font-bold leading-tight tracking-tight text-foundry-primary sm:text-5xl"
		>
			Coming from claude-mpm
		</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			trusty-mpm is a Rust meta-harness for Claude Code sessions, driven by one binary called
			<code class="text-base">tm</code>. If you already run a PM-style multi-agent setup, the way
			you start a session barely changes. What changes is everything underneath it: a supervising
			daemon instead of a per-session process, tmux-hosted sessions you can walk away from, and
			agents deployed where they cannot leak into your other tools.
		</p>

		<div class="mt-8 flex flex-wrap gap-3">
			<a href="#install" class="btn btn-primary">Install</a>
			<a href="#checklist" class="btn btn-secondary">Migration checklist</a>
			<a href="/tools/trusty-mpm" class="btn btn-secondary">trusty-mpm</a>
		</div>

		<dl class="mt-12 flex flex-wrap gap-x-10 gap-y-4">
			{#each facts as fact (fact.label)}
				<div>
					<dt class="eyebrow">{fact.label}</dt>
					<dd class="mt-1 font-mono text-sm text-foundry-text">{fact.value}</dd>
				</div>
			{/each}
		</dl>
	</div>
</section>

<!-- DISAMBIGUATION — the first thing the reader needs, before any instruction. -->
<section class="mx-auto max-w-content px-4 pt-16 sm:px-6">
	<div class="card max-w-3xl min-w-0 border-l-4 border-l-foundry-primary">
		<p class="eyebrow">Read this first</p>
		<h2 class="mt-3 font-display text-xl font-semibold">
			trusty-mpm is not a version of claude-mpm
		</h2>
		<p class="mt-3 text-foundry-secondary">
			They are unrelated codebases. trusty-mpm is not a fork, a port, or a rewrite of claude-mpm —
			there is no shared code, the languages differ (Rust and Python), the maintainers differ, and
			they ship through different channels (crates.io and Homebrew, versus PyPI). What they share is
			an idea: a project-manager session that delegates work to specialised agents.
		</p>
		<p class="mt-3 text-foundry-secondary">
			The similar names cause real confusion, including for Claude Code sessions asked what they are
			running under. trusty-mpm keeps a canonical answer in its own repository —
			<a
				href={IDENTITY_DOC}
				rel="noreferrer noopener"
				class="text-foundry-primary underline underline-offset-2">WHAT-IS-TRUSTY-MPM.md</a
			> — and that document, not a shell probe, is the thing to quote.
		</p>
	</div>
</section>

<!-- BODY, part one — what carries over. -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="flex flex-col gap-12">
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">What carries over</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				The shape of the work is the same. A PM session orchestrates and delegates; specialised
				agents do the implementing; skills are reusable instruction packs; an output style decides
				how the PM writes back to you. If those four words already mean something to you, you know
				how to drive <code class="text-sm">tm</code>.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				That parity is deliberate rather than incidental. <code class="text-sm">tm launch</code>
				runs the full deployment sequence — instructions, agents, skills, MCP config — into the project
				and then starts or attaches the session in your current terminal, which is the same single-command
				experience you already have. The source comment on that subcommand says it plainly: it behaves
				like running <code class="text-sm">claude-mpm</code>.
			</p>

			<div class="doc-prose doc-table max-w-3xl">
				<table>
					<caption class="eyebrow px-3 py-2 text-left">Same concept, different plumbing</caption>
					<thead>
						<tr>
							<th scope="col">Concept</th>
							<th scope="col">Where it lives under tm</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td>PM delegation model</td>
							<td class="text-foundry-secondary"
								>Unchanged. The PM orchestrates and delegates; it does not write code itself.</td
							>
						</tr>
						<tr>
							<td>Agent roster</td>
							<td class="text-foundry-secondary"
								>Composed and deployed by <code>tm install</code> and by each session launch.</td
							>
						</tr>
						<tr>
							<td>Skills</td>
							<td class="text-foundry-secondary"
								>Deployed alongside the agents, plus a project tier under the project's own
								<code>.claude/skills</code>.</td
							>
						</tr>
						<tr>
							<td>Output styles</td>
							<td class="text-foundry-secondary"
								>Three bundled ids — <code>trusty-mpm</code>, <code>trusty-mpm-teacher</code>,
								<code>trusty-mpm-research</code>. Selected with <code>/config</code> or the
								<code>outputStyle</code> settings key.</td
							>
						</tr>
					</tbody>
				</table>
			</div>
		</div>
	</div>
</section>

<!-- INSTALL — the raised band, matching the tool pages' own install section. -->
<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<h2 id="install" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">Install</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Two lines. The bootstrap puts <code class="text-sm">tctl</code> — the install control plane —
			on your PATH; <code class="text-sm">tctl install trusty-mpm</code> then resolves what tm needs at
			runtime and brings those daemons up too. No Rust toolchain is required on a supported platform.
		</p>

		<div class="mt-6 max-w-xl min-w-0">
			<p class="eyebrow">shell</p>
			<pre
				class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
tctl install trusty-mpm
tm --version</pre>
		</div>

		<p class="mt-6 max-w-3xl text-sm text-foundry-secondary">
			Homebrew and <code class="text-sm">cargo install</code> from a checkout both work; the
			<a href="/#install" class="text-foundry-primary underline underline-offset-2"
				>other install paths</a
			>
			are on the home page. macOS 12 or later and Linux are supported. Windows is not.
		</p>

		<p class="mt-4 max-w-3xl text-sm text-foundry-secondary">
			The bootstrap script verifies every downloaded tarball against its published SHA-256 checksum.
			The script itself is unsigned — read it before piping it to a shell if you need higher
			assurance.
		</p>
	</div>
</section>

<!-- BODY, part two — what actually behaves differently. -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="flex flex-col gap-12">
		<!-- ============ daemon model ============ -->
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">
				A supervising daemon, not a process per session
			</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				claude-mpm runs inside the session it manages: start it, and its lifetime is that session's
				lifetime. trusty-mpm inverts that. A long-lived daemon holds the project registry and the
				session roster, and sessions are things it configures, launches, and supervises. Close a
				terminal and the daemon still knows what is running; restart the daemon and the roster
				survives, because it is on disk rather than in a process.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				Two more daemons come with it. Together they are what a session reads from and writes to,
				and each one is also an MCP server, so a session reaches them through tool calls rather than
				by shelling out.
			</p>

			<div class="doc-prose doc-table max-w-3xl">
				<table>
					<thead>
						<tr>
							<th scope="col">Daemon</th>
							<th scope="col">Address</th>
							<th scope="col">What it holds</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td><code class="whitespace-nowrap">trusty-mpm</code></td>
							<td><code class="whitespace-nowrap">127.0.0.1:7880</code></td>
							<td class="text-foundry-secondary"
								>Registered projects, the session roster, and the relayed hook feed. A separate
								supervisor process watches sessions and reports on
								<code class="whitespace-nowrap">7881</code>.</td
							>
						</tr>
						<tr>
							<td><code class="whitespace-nowrap">trusty-memory</code></td>
							<td><code class="whitespace-nowrap">127.0.0.1:7070</code></td>
							<td class="text-foundry-secondary"
								>Memory palaces — long-term recall organised per project, over an HNSW vector index,
								a redb store, and a knowledge graph.</td
							>
						</tr>
						<tr>
							<td><code class="whitespace-nowrap">trusty-search</code></td>
							<td><code class="whitespace-nowrap">127.0.0.1:7878</code></td>
							<td class="text-foundry-secondary"
								>Named code indexes, one per project, kept fresh by a file watcher. One install per
								machine.</td
							>
						</tr>
					</tbody>
				</table>
			</div>

			<p class="mt-6 max-w-3xl text-foundry-secondary">
				Every one of those addresses is loopback. Nothing in the fleet listens on an external
				interface, and the HTTP routers additionally reject requests whose origin is not the machine
				itself, so a page you happen to have open cannot reach them.
			</p>

			<div class="mt-6 grid max-w-3xl gap-4 sm:grid-cols-2">
				<div class="card">
					<span class="badge">launchd</span>
					<h3 class="mt-3 font-display text-lg font-semibold text-foundry-text">
						memory and search
					</h3>
					<p class="mt-3 text-sm text-foundry-secondary">
						Registered as LaunchAgents and controlled through
						<code class="text-xs">tctl start</code> / <code class="text-xs">stop</code> /
						<code class="text-xs">restart</code>, which drive <code class="text-xs">launchctl</code> underneath.
						They come back after a reboot without you doing anything.
					</p>
				</div>
				<div class="card">
					<span class="badge">Own verb</span>
					<h3 class="mt-3 font-display text-lg font-semibold text-foundry-text">the tm daemon</h3>
					<p class="mt-3 text-sm text-foundry-secondary">
						Not launchd-managed. It ships its own
						<code class="text-xs">tm start</code> / <code class="text-xs">stop</code> /
						<code class="text-xs">restart</code>, and <code class="text-xs">tctl</code> shells out to
						those. Worth knowing before you go looking for a plist that does not exist.
					</p>
				</div>
			</div>

			<p class="mt-6 max-w-3xl text-sm text-foundry-secondary">
				Addresses are resolved from each daemon's discovery file rather than assumed, so a daemon
				that rebinds to a different port is still found. When one is unreachable the caller gets an
				error, not a silent timeout.
			</p>
		</div>

		<!-- ============ tmux ============ -->
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">Sessions live in tmux</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				A tm session is a tmux session. That is not a convenience wrapper you can opt out of — it is
				how the daemon can keep a session alive while no terminal is attached to it, and how several
				sessions run at once without one window per session on your screen.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				Names are derived rather than random. A session gets
				<code class="text-sm">tm-&lt;project&gt;-NN</code> — an explicit hint if you gave one,
				otherwise the repository name, otherwise the directory's own name — with a per-project
				serial so two sessions on the same project stay distinguishable. When a session does get its
				own git worktree, the worktree is named after the session, so a path like
				<code class="text-sm">.worktrees/tm-trusty-tools-01/</code> tells you which session owns it without
				consulting anything.
			</p>

			<div class="doc-prose doc-table max-w-3xl">
				<table>
					<thead>
						<tr>
							<th scope="col">Command</th>
							<th scope="col">What it does</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td><code class="whitespace-nowrap">tm launch</code></td>
							<td class="text-foundry-secondary"
								>Deploys instructions, agents, skills and MCP config, then starts or attaches the
								session here.</td
							>
						</tr>
						<tr>
							<td><code class="whitespace-nowrap">tm connect</code></td>
							<td class="text-foundry-secondary"
								>Starts or attaches the tmux-hosted session and nothing else — no deployment step.</td
							>
						</tr>
						<tr>
							<td><code class="whitespace-nowrap">tm ls</code></td>
							<td class="text-foundry-secondary"
								>On a terminal, an interactive picker over the live fleet. Piped or with
								<code>--json</code>, a plain list.</td
							>
						</tr>
						<tr>
							<td><code class="whitespace-nowrap">tm attach &lt;target&gt;</code></td>
							<td class="text-foundry-secondary"
								>Finds a session by id, name prefix, or project path and opens the dashboard focused
								on it.</td
							>
						</tr>
						<tr>
							<td><code class="whitespace-nowrap">tm tui</code></td>
							<td class="text-foundry-secondary">A terminal dashboard across every live session.</td
							>
						</tr>
					</tbody>
				</table>
			</div>

			<p class="mt-6 max-w-3xl text-foundry-secondary">
				Sessions also pause and resume — <code class="text-sm">tm sessions pause</code> and
				<code class="text-sm">tm sessions resume</code> — and finish with
				<code class="text-sm">tm sessions decommission</code>, which removes the session's worktree
				and branch together rather than leaving either behind.
			</p>
		</div>

		<!-- ============ segregation ============ -->
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">
				Agents deploy per project, not into your global config
			</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				This is the difference most likely to surprise you, and it comes from a real incident. A
				shared global <code class="text-sm">~/.claude/agents</code> is a single namespace every
				Claude Code tool writes into, and in July 2026 one tool's agents leaked into another tool's
				sessions. The fix was a rule: trusty-mpm never depends on the user's real
				<code class="text-sm">~/.claude</code>, and never writes your live checkout.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				The mechanism is one environment variable. tm launches Claude Code with
				<code class="text-sm">CLAUDE_CONFIG_DIR</code> pointed at a directory tm owns, so every
				<code class="text-sm">~/.claude</code> path Claude Code would otherwise read resolves inside
				that directory instead. Your real
				<code class="text-sm">~/.claude</code> — and whatever hooks, MCP servers, and agents another
				tool put there — is excluded outright rather than merged on top. Project-scoped content
				lands in the checkout's own <code class="text-sm">.claude/</code> inside a workspace tm controls.
			</p>

			<div class="doc-prose doc-table max-w-3xl">
				<table>
					<thead>
						<tr>
							<th scope="col">Surface</th>
							<th scope="col">claude-mpm</th>
							<th scope="col">trusty-mpm</th>
						</tr>
					</thead>
					<tbody>
						<tr>
							<td>Agents and skills</td>
							<td class="text-foundry-secondary">The real <code>~/.claude</code>, shared</td>
							<td class="text-foundry-secondary"
								>A tm-owned config dir, plus the project's own <code>.claude/</code></td
							>
						</tr>
						<tr>
							<td>Hooks and MCP servers</td>
							<td class="text-foundry-secondary">The real <code>~/.claude</code>, shared</td>
							<td class="text-foundry-secondary"
								>The same tm-owned dir; the real global is not merged</td
							>
						</tr>
						<tr>
							<td>Per-session state</td>
							<td class="text-foundry-secondary">—</td>
							<td class="text-foundry-secondary"
								>The project's <code>.trusty-mpm/</code>, holding the instructions the session
								actually received</td
							>
						</tr>
					</tbody>
				</table>
			</div>

			<div class="card mt-8 max-w-3xl">
				<p class="eyebrow">A side effect worth knowing</p>
				<p class="mt-3 text-foundry-secondary">
					Because the real global is excluded, a customisation you added to
					<code class="text-sm">~/.claude</code> for claude-mpm does not follow you into a tm session.
					That is the point of the isolation, not a bug in it — but it does mean a hand-written agent
					or hook you rely on has to be reinstated on tm's side rather than inherited.
				</p>
			</div>
		</div>

		<!-- ============ memory and search ============ -->
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">Memory and search per project</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				Memory is a palace: a named store scoped to a project rather than to a conversation, holding
				prose an assistant wrote and can recall later, alongside a knowledge graph of structured
				triples. A session reaches it through MCP tool calls at an address resolved from the
				daemon's own discovery file — never a hardcoded port, which is what used to make this
				silently fail when the daemon rebound.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				Search is scoped the same way, and more tightly than you might expect. When a session is
				provisioned, its project gets a search index derived from the project's path, and that index
				id is pinned into the session's own <code class="text-sm">.mcp.json</code>. Every search the
				session runs is therefore already scoped — there is no ambiguity for the daemon to guess at,
				and a query cannot return results from the last project you worked on. When the session is
				decommissioned the index is marked for collection, so indexes do not accumulate.
			</p>
		</div>

		<!-- ============ kuzu-memory ============ -->
		<div class="min-w-0">
			<h2 id="kuzu" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">
				Migrating your kuzu-memory data
			</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				If the memory server you were running is <code class="text-sm">kuzu-memory</code>,
				trusty-memory migrates it for you rather than leaving you to re-enter anything.
				<code class="text-sm">trusty-memory migrate</code> takes two targets, and they are independent:
				one rewrites configuration, the other moves data. Most people want both, in that order.
			</p>

			<h3 class="mt-8 font-display text-lg font-semibold text-foundry-text">
				<code class="text-base">kuzu-memory</code> — the configuration
			</h3>
			<p class="mt-3 max-w-3xl text-foundry-secondary">
				Scans every <code class="text-sm">.claude/settings.json</code> and
				<code class="text-sm">settings.local.json</code> under your home directory, drops any
				<code class="text-sm">kuzu-memory</code> or <code class="text-sm">kuzu_memory</code>
				entry from the <code class="text-sm">mcpServers</code> block, and inserts a canonical
				<code class="text-sm">trusty-memory</code> one in its place. Unrelated keys survive, each
				write is atomic with a <code class="text-sm">.bak</code> alongside it, and a file already
				carrying a <code class="text-sm">trusty-memory</code> entry is left byte-for-byte alone — so running
				it twice is safe.
			</p>

			<div class="mt-6 max-w-xl min-w-0">
				<p class="eyebrow">shell — look first, then write</p>
				<pre
					class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">trusty-memory migrate kuzu-memory --dry-run
trusty-memory migrate kuzu-memory</pre>
			</div>

			<h3 class="mt-8 font-display text-lg font-semibold text-foundry-text">
				<code class="text-base">kuzu-data</code> — the memories themselves
			</h3>
			<p class="mt-3 max-w-3xl text-foundry-secondary">
				Reads a kuzu-memory <code class="text-sm">store.redb</code> and imports it into a palace:
				every entity becomes a drawer, every relation becomes a knowledge-graph triple. There is no
				default source path — <code class="text-sm">--from</code> and
				<code class="text-sm">--palace</code> are both required, and the command errors naming the missing
				one rather than guessing. The palace is created if it does not exist yet. Re-running it changes
				nothing: drawer ids come from a stable hash of the entity id and the palace name, and an existing
				triple is skipped rather than duplicated.
			</p>

			<div class="mt-6 max-w-xl min-w-0">
				<p class="eyebrow">shell</p>
				<pre
					class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">trusty-memory migrate kuzu-data \
  --from ~/.open-mpm/memory/store.redb \
  --palace your-project --dry-run</pre>
			</div>

			<p class="mt-4 max-w-3xl text-sm text-foundry-secondary">
				<code class="text-sm">--dry-run</code> prints the schema it found and the plan without
				writing. <code class="text-sm">--limit &lt;N&gt;</code> caps how many entities are imported,
				which is the way to try it on a slice before committing to the whole store. Drop
				<code class="text-sm">--dry-run</code> to run it for real.
			</p>

			<div class="card mt-8 max-w-3xl">
				<p class="eyebrow">One class of triple does not come across</p>
				<p class="mt-3 text-foundry-secondary">
					Four predicates — <code class="text-sm">is_alias_for</code>,
					<code class="text-sm">has_convention</code>, <code class="text-sm">is_fact</code>, and
					<code class="text-sm">is_shorthand_for</code> — put a fact on the surface injected into
					every turn of every session, and the importer refuses them outright. A bulk import of a
					legacy file is not somebody deciding a standing rule should be in front of every session,
					so a relation whose type collides with one of the four is logged and skipped while the
					rest of the import continues. Ordinary relation types —
					<code class="text-sm">relates_to</code>, <code class="text-sm">mentions</code>,
					<code class="text-sm">derived_from</code>, <code class="text-sm">part_of</code>,
					<code class="text-sm">alias_of</code> — are unaffected. If one of the four really was a
					standing rule, re-assert it with <code class="text-sm">kg_assert</code>, which is the
					deliberate path and carries the real gate.
				</p>
			</div>
		</div>

		<!-- ============ where sessions run ============ -->
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">Where the work happens</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				By default a session runs on your project's main checkout — several sessions sharing one
				checkout is the normal arrangement rather than a hazard. What keeps that safe is a write
				boundary the harness enforces rather than asks for: a session standing in a main checkout
				may write documents and configuration there, and an agent it dispatches that needs to change
				source is granted its own git worktree instead.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				<code class="text-sm">tm launch --worktree</code> opts the whole session into its own worktree
				when you want the live checkout left completely alone.
			</p>
		</div>
	</div>
</section>

<!-- CHECKLIST — the actual migration, in order. -->
<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<h2 id="checklist" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">
			The migration, step by step
		</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Nothing here uninstalls claude-mpm. The two can coexist — they read different configuration
			directories, so neither sees the other's agents — and you can migrate one project at a time.
		</p>

		<ol class="mt-8 max-w-3xl list-none space-y-6 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">1</span
				>
				<div class="min-w-0">
					<p class="font-semibold text-foundry-text">Install trusty-mpm.</p>
					<p class="mt-1 text-sm">
						The two lines above. <code class="text-xs">tctl</code> brings up the memory and search daemons
						as part of the same run.
					</p>
				</div>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">2</span
				>
				<div class="min-w-0">
					<p class="font-semibold text-foundry-text">Deploy the framework.</p>
					<p class="mt-1 text-sm">
						<code class="text-xs">tm install</code> composes and writes the agent roster, the skills,
						the hook registrations, and the output styles into the tm-owned config directory. This is
						the step that makes a session find a full roster rather than a partial one.
					</p>
				</div>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">3</span
				>
				<div class="min-w-0">
					<p class="font-semibold text-foundry-text">
						Bring your kuzu-memory data across — only if you ran it.
					</p>
					<p class="mt-1 text-sm">
						<code class="text-xs">trusty-memory migrate kuzu-memory</code> repoints the MCP config,
						then
						<code class="text-xs"
							>trusty-memory migrate kuzu-data --from &lt;store.redb&gt; --palace &lt;name&gt;</code
						>
						imports the memories. Both take
						<code class="text-xs">--dry-run</code>, and both are safe to re-run.
						<a
							href="#kuzu"
							class="text-foundry-primary underline decoration-1 underline-offset-2 hover:decoration-2"
							>The full section</a
						> covers what each one touches and the one class of triple it declines to import.
					</p>
				</div>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">4</span
				>
				<div class="min-w-0">
					<p class="font-semibold text-foundry-text">Register the project.</p>
					<p class="mt-1 text-sm">
						From the project directory, <code class="text-xs">tm project init</code> registers it with
						the daemon. Registration rather than inference is what makes the same directory resolve to
						the same project across sessions and across restarts.
					</p>
				</div>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">5</span
				>
				<div class="min-w-0">
					<p class="font-semibold text-foundry-text">Clear a stale output style.</p>
					<p class="mt-1 text-sm">
						If a project or global <code class="text-xs">settings.json</code> still carries an
						<code class="text-xs">outputStyle</code> left over from claude-mpm, the session will not
						be running under trusty-mpm's instructions at all.
						<code class="text-xs">tm doctor</code>'s output-style check reports exactly this, and a
						fresh
						<code class="text-xs">tm launch</code> rewrites the setting correctly.
					</p>
				</div>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">6</span
				>
				<div class="min-w-0">
					<p class="font-semibold text-foundry-text">Verify before you start work.</p>
					<p class="mt-1 text-sm">
						<code class="text-xs">tm doctor</code> runs the full diagnostic and needs a reachable
						daemon. <code class="text-xs">tm validate</code> runs the same deployment diff against
						the filesystem with no daemon at all and exits non-zero when something is missing, which
						makes it the one to put in a script. <code class="text-xs">tm health</code> is the one-line
						answer.
					</p>
				</div>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">7</span
				>
				<div class="min-w-0">
					<p class="font-semibold text-foundry-text">Start a session.</p>
					<p class="mt-1 text-sm">
						<code class="text-xs">tm launch</code> in the project directory. From there it should
						feel familiar — and <code class="text-xs">tm ls</code> will find it again after you close
						the terminal.
					</p>
				</div>
			</li>
		</ol>

		<div class="mt-8 max-w-xl min-w-0">
			<p class="eyebrow">shell — the whole thing</p>
			<pre
				class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">tm install
trusty-memory migrate kuzu-memory      # only if you ran kuzu-memory
cd ~/your-project
tm project init
tm doctor
tm launch</pre>
		</div>
	</div>
</section>

<!-- CLOSE -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="card flex max-w-3xl flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
		<div>
			<h2 class="font-display text-xl font-semibold">Everything tm can do</h2>
			<p class="mt-1 text-sm text-foundry-secondary">
				The daemon, the dashboard, the hook relay, and remote control from Telegram or Slack.
			</p>
		</div>
		<a href="/tools/trusty-mpm" class="btn btn-primary shrink-0">trusty-mpm</a>
	</div>
	<p class="mt-8">
		<a href="/docs" class="text-sm text-foundry-primary underline underline-offset-2"
			>Browse the documentation →</a
		>
	</p>
</section>
