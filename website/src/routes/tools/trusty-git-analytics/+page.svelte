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

	/** The `tga audit` sweep's own facts strip, rendered inside the body. */
	const auditFacts = [
		{ label: 'Binary', value: 'tga' },
		{ label: 'Stages', value: '8' },
		{ label: 'Flags', value: '6, all optional' },
		{ label: 'Output', value: 'up to 18 files' },
		{ label: 'Prompts', value: 'none' }
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

	<!-- ============ tga audit ============ -->
	<div>
		<p class="eyebrow">Acquisition due diligence</p>
		<h2 id="audit" class="mt-3 scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">
			<code>tga audit</code>
		</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			One command walks the git history of the repositories your config already names, and renders a
			technical due-diligence report for someone deciding whether to buy the codebase. The report
			has eight sections, and the parts it cannot fill are named as gaps rather than scored.
		</p>
		<dl class="mt-8 flex flex-wrap gap-x-10 gap-y-4">
			{#each auditFacts as fact (fact.label)}
				<div>
					<dt class="eyebrow">{fact.label}</dt>
					<dd class="mt-1 font-mono text-sm text-foundry-text">{fact.value}</dd>
				</div>
			{/each}
		</dl>
	</div>

	<!-- ============ thesis ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">The gaps are printed, not filled in</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			A due-diligence report is read by someone deciding whether to buy the thing it describes. The
			failure that matters there is not a missing number. It is a number nobody measured, printed as
			though somebody had.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">tga audit</code> has no PERFORMANCE dimension and no COST dimension — nothing
			in the pipeline measures either one. So the report declares both as gaps rather than scoring them
			off a proxy.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Failure works the same way. Point the sweep at a config with no JIRA project key and the
			<code class="text-sm">jira sync</code> stage fails. That failure becomes a named line in
			<strong class="font-semibold text-foundry-text">Gaps &amp; Caveats</strong>. The alternative —
			a zero in a cell — is indistinguishable from a project that genuinely had no JIRA activity,
			and the reader has no way to tell which one they are looking at.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Every report-quality bug fixed in the last milestone was that same shape: a partial signal
			rendered as an authoritative statement.
		</p>

		<div class="card mt-8 max-w-3xl min-w-0">
			<p class="eyebrow">The failure you will actually hit</p>
			<pre
				class="mt-3 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-raised p-3 text-xs leading-relaxed text-foundry-text">no JIRA project scope: pass --project &lt;KEY&gt; or set jira.project_key in config.yaml</pre>
			<p class="mt-3 text-sm text-foundry-secondary">
				This is the stage that most often fails on an unattended run. It does not stop the sweep,
				and it does not become a zero.
			</p>
		</div>
	</div>

	<!-- ============ running it ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Running it</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Six flags, all optional, nothing interactive. Configure your repository set first;
			<code class="text-sm">tga audit</code> reads it and goes.
		</p>

		<div class="mt-6 max-w-xl min-w-0">
			<p class="eyebrow">shell — named engagement</p>
			<pre
				class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">tga --config config.yaml audit \
  --org acme --client "Acme Holdings" --analyst "J. Reviewer" \
  --weeks 26 --output ./acme-dd</pre>
		</div>

		<div class="mt-6 max-w-xl min-w-0">
			<p class="eyebrow">shell — defaults</p>
			<pre
				class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">tga audit</pre>
		</div>

		<p class="mt-6 max-w-3xl text-sm text-foundry-secondary">
			Bare, it reads <code class="text-sm">./config.yaml</code> and writes
			<code class="text-sm">./audit-output</code>.
			<code class="text-sm">-c, --config &lt;FILE&gt;</code>
			is global on <code class="text-sm">tga</code>; a missing config logs a warning and runs on
			defaults rather than failing.
		</p>

		<div class="doc-prose doc-table max-w-3xl">
			<table>
				<caption class="eyebrow px-3 py-2 text-left">Flags</caption>
				<thead>
					<tr>
						<th scope="col">Flag</th>
						<th scope="col">Effect</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td><code class="whitespace-nowrap">--org &lt;ORG&gt;</code></td>
						<td class="text-foundry-secondary"
							>The organisation under audit. Metadata only — it titles the report and nothing else.</td
						>
					</tr>
					<tr>
						<td><code class="whitespace-nowrap">--title &lt;TITLE&gt;</code></td>
						<td class="text-foundry-secondary"
							>Defaults to <code>&lt;org&gt; — Technical Due Diligence</code>, or
							<code>Technical Due Diligence</code> with no <code>--org</code>.</td
						>
					</tr>
					<tr>
						<td><code class="whitespace-nowrap">--analyst &lt;NAME&gt;</code></td>
						<td class="text-foundry-secondary">Renders as <em>not stated</em> if omitted.</td>
					</tr>
					<tr>
						<td><code class="whitespace-nowrap">--client &lt;NAME&gt;</code></td>
						<td class="text-foundry-secondary">Same — absent is recorded as absent.</td>
					</tr>
					<tr>
						<td><code class="whitespace-nowrap">-o, --output &lt;DIR&gt;</code></td>
						<td class="text-foundry-secondary"
							>Where the audit writes. Defaults to <code>./audit-output</code>.</td
						>
					</tr>
					<tr>
						<td><code class="whitespace-nowrap">--weeks &lt;N&gt;</code></td>
						<td class="text-foundry-secondary">Limit collection to the last N ISO weeks.</td>
					</tr>
				</tbody>
			</table>
		</div>

		<div class="card max-w-3xl">
			<p class="eyebrow">What <code class="text-sm">--org</code> is not</p>
			<p class="mt-3 text-foundry-secondary">
				It does not discover repositories. The sweep audits whatever local checkouts the resolved
				config already names, and <code class="text-sm">--org</code> only reaches the report's title block.
			</p>
		</div>
	</div>

	<!-- ============ stages ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">
			Eight stages, in the order the data flows
		</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The numbering is real: each stage writes what the next one reads, which is why they run in
			this order and not another. A stage that fails is recorded and named in
			<strong class="font-semibold text-foundry-text">Gaps &amp; Caveats</strong>, and the run
			continues.
		</p>
		<!-- Numbered rather than the page's usual bullet dot: the copy above
		     states the ordering is load-bearing, so the reader needs the index. -->
		<ol class="mt-6 max-w-3xl list-none space-y-3 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">1</span
				>
				<span
					><code class="text-sm">collect</code> — walks the configured repositories into
					<code class="text-sm">commits</code> via git2.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">2</span
				>
				<span
					><code class="text-sm">classify</code> — runs the four-tier classification cascade over those
					commits.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">3</span
				>
				<span><code class="text-sm">jira sync</code> — ingests JIRA transitions and comments.</span>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">4</span
				>
				<span
					><code class="text-sm">deployments collect</code> — deploy events into
					<code class="text-sm">fact_deployments</code>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">5</span
				>
				<span
					><code class="text-sm">incidents collect</code> — incidents into
					<code class="text-sm">fact_incidents</code>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">6</span
				>
				<span
					><code class="text-sm">dora</code> — reduces those two fact tables to the four DORA keys.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">7</span
				>
				<span
					><code class="text-sm">pr-metrics</code> — per-engineer pull-request metrics; writes
					<code class="text-sm">pr-metrics.csv</code>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">8</span
				>
				<span><code class="text-sm">report</code> — renders tga's own CSV, JSON and Markdown.</span>
			</li>
		</ol>

		<div class="card mt-8 max-w-3xl">
			<p class="eyebrow">No stage aborts the run</p>
			<p class="mt-3 text-foundry-secondary">
				Every stage's result is recorded, never propagated. A stage that fails is marked failed, the
				sweep moves to the next one, and the failure surfaces as a named gap in the report instead
				of a halt. One thing stops a sweep: a pre-flight failure creating the output directory.
			</p>
			<p class="mt-3 text-sm text-foundry-secondary">
				That is what makes an unattended run worth starting. A pipeline that stops on the first
				unreachable source produces nothing; this one produces a report whose missing pieces are
				labelled.
			</p>
		</div>
	</div>

	<!-- ============ output directory ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">What lands in the output directory</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">Up to 18 files, from three producers.</p>

		<div class="doc-prose doc-table max-w-3xl">
			<table>
				<thead>
					<tr>
						<th scope="col">From</th>
						<th scope="col" class="text-right">Files</th>
						<th scope="col">What</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td><code class="whitespace-nowrap">report</code> stage</td>
						<td class="text-right font-mono tabular-nums">14</td>
						<td class="text-foundry-secondary">9 CSV, 4 JSON, 1 Markdown.</td>
					</tr>
					<tr>
						<td><code class="whitespace-nowrap">pr-metrics</code> stage</td>
						<td class="text-right font-mono tabular-nums">1</td>
						<td class="text-foundry-secondary"><code>pr-metrics.csv</code>.</td>
					</tr>
					<tr>
						<td>the seam</td>
						<td class="text-right font-mono tabular-nums">1</td>
						<td class="text-foundry-secondary"
							><code>manifest.toml</code> — what tga hands to trusty-review.</td
						>
					</tr>
					<tr>
						<td>trusty-review</td>
						<td class="text-right font-mono tabular-nums">2</td>
						<td class="text-foundry-secondary"
							><code>&#123;slug&#125;.md</code> and <code>&#123;slug&#125;.json</code> — the due-diligence
							report itself.</td
						>
					</tr>
				</tbody>
			</table>
		</div>
	</div>

	<!-- ============ report sections ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">The report, section by section</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Eight sections, in this order. Section 2 renders deterministically —
			<code class="text-sm">tga audit</code> never passes <code class="text-sm">--synthesize</code>,
			so no model writes the executive summary.
		</p>

		<div class="doc-prose doc-table max-w-3xl">
			<table>
				<thead>
					<tr>
						<th scope="col" class="w-10 text-right">§</th>
						<th scope="col">Section</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td class="text-right font-mono tabular-nums">1</td>
						<td>Report Metadata</td>
					</tr>
					<tr>
						<td class="text-right font-mono tabular-nums">2</td>
						<td>Executive Summary, with Top Risks</td>
					</tr>
					<tr>
						<td class="text-right font-mono tabular-nums">3</td>
						<td>Scoring Model Normalization</td>
					</tr>
					<tr>
						<td class="text-right font-mono tabular-nums">4</td>
						<td>Per-Application Scorecard</td>
					</tr>
					<tr>
						<td class="text-right font-mono tabular-nums">5</td>
						<td>
							Findings by Severity
							<ul class="mt-2 text-sm text-foundry-secondary">
								<li>5.1 RED / CRITICAL — full detail</li>
								<li>5.2 AMBER / MEDIUM — compact</li>
								<li>5.3 GREEN / POSITIVE — topic list only</li>
							</ul>
						</td>
					</tr>
					<tr>
						<td class="text-right font-mono tabular-nums">6</td>
						<td>
							Risk Registers
							<ul class="mt-2 text-sm text-foundry-secondary">
								<li>6.1 Security Violations</li>
								<li>6.2 Open-Source / CVE Exposure</li>
								<li>6.3 License / IP Risk</li>
								<li>6.4 Obsolescence</li>
								<li>6.5 Cloud Readiness Blockers</li>
								<li>6.6 Technical-Debt / Remediation Economics</li>
							</ul>
						</td>
					</tr>
					<tr>
						<td class="text-right font-mono tabular-nums">7</td>
						<td>Graph-Ready Data Appendix</td>
					</tr>
					<tr>
						<td class="text-right font-mono tabular-nums">8</td>
						<td>Gaps &amp; Caveats</td>
					</tr>
				</tbody>
			</table>
		</div>

		<div class="mt-6 grid gap-4 sm:grid-cols-3">
			<div class="card">
				<span class="badge">From tga</span>
				<p class="mt-3 text-sm text-foundry-secondary">
					§1 in full, and §4's tech stack, lines of code, and frameworks.
				</p>
			</div>
			<div class="card">
				<span class="badge">From trusty-analyze</span>
				<p class="mt-3 text-sm text-foundry-secondary">
					§5, §6.1, and §7's <code class="text-xs">complexity_distribution</code> and
					<code class="text-xs">loc_by_technology</code>.
				</p>
			</div>
			<div class="card">
				<span class="badge">Declared gap</span>
				<p class="mt-3 text-sm text-foundry-secondary">
					§4 Benchmark Position, §6.2, §6.3, §6.4, §6.5, §6.6 — no data source behind any of them,
					and the report says so rather than scoring them.
				</p>
			</div>
		</div>
	</div>

	<!-- ============ severity ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">
			Red, amber, green — and what routes a finding there
		</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The colours are the report's own convention, and the mapping is mechanical: the diagnostic's
			own severity decides, nothing else.
		</p>

		<div class="doc-prose doc-table max-w-3xl">
			<table>
				<thead>
					<tr>
						<th scope="col">Band</th>
						<th scope="col">Diagnostic severity</th>
						<th scope="col">How it renders</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td><span class="badge badge-red">Red</span></td>
						<td><code>error</code> · <code>critical</code></td>
						<td class="text-foundry-secondary">§5.1, full detail.</td>
					</tr>
					<tr>
						<td><span class="badge badge-amber">Amber</span></td>
						<td><code>warning</code> · <code>high</code></td>
						<td class="text-foundry-secondary">§5.2, compact.</td>
					</tr>
					<tr>
						<td><span class="badge badge-green">Green</span></td>
						<td class="text-foundry-secondary">everything else</td>
						<td class="text-foundry-secondary">§5.3, topic list only.</td>
					</tr>
				</tbody>
			</table>
		</div>

		<div class="card max-w-3xl">
			<p class="eyebrow">The ceiling that matters</p>
			<p class="mt-3 text-foundry-secondary">
				A refactor suggestion caps at Amber. It can never come out Red, no matter what the
				underlying tool called it — because "extract this method" is advice, and a buyer reading a
				RED line is entitled to assume something is broken.
			</p>
		</div>
	</div>

	<!-- ============ before / after ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">What the last fix actually changed</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Measured on one run, over the ripgrep repository, before and after the report stage was
			corrected.
		</p>

		<div class="doc-prose doc-table max-w-3xl">
			<table>
				<thead>
					<tr>
						<th scope="col">Measure</th>
						<th scope="col" class="text-right">Before</th>
						<th scope="col" class="text-right">After</th>
					</tr>
				</thead>
				<tbody>
					<tr>
						<td>RED findings</td>
						<td class="text-right font-mono tabular-nums">20</td>
						<td class="text-right font-mono tabular-nums">0</td>
					</tr>
					<tr>
						<td>AMBER findings, each carrying real numbers</td>
						<td class="text-right font-mono tabular-nums">0</td>
						<td class="text-right font-mono tabular-nums">20</td>
					</tr>
					<tr>
						<td>Non-code components scored</td>
						<td class="text-right font-mono tabular-nums">2</td>
						<td class="text-right font-mono tabular-nums">0</td>
					</tr>
					<tr>
						<td>Complexity buckets sum to</td>
						<td class="text-right font-mono tabular-nums">1,000</td>
						<td class="text-right font-mono tabular-nums">4,328</td>
					</tr>
				</tbody>
			</table>
		</div>

		<p class="mt-6 max-w-3xl text-foundry-secondary">
			The 20 pre-fix RED entries were all "Extract method" refactor suggestions misrouted into RED.
			The two non-code components were <code class="text-sm">CHANGELOG.md</code> and
			<code class="text-sm">FAQ.md</code>, scored as if they were applications. The complexity
			buckets summed to a round 1,000 instead of the 4,328 units actually counted.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			All three are the same defect wearing different clothes: output that looked like a measurement
			and was not one.
		</p>

		<div class="card mt-8 max-w-3xl">
			<p class="eyebrow">Scope of that evidence</p>
			<p class="mt-3 text-foundry-secondary">
				These numbers are the ripgrep run only. The trusty-tools audit was not re-run after the fix,
				so nothing here says the same before-and-after holds on a second codebase.
			</p>
		</div>
	</div>

	<!-- ============ limitations ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">What this does not do</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The buyer's alternative is a tool that would have guessed at all six of these. Read this
			section as part of the pitch, not as a disclaimer at the bottom.
		</p>

		<div class="mt-6 grid gap-4 sm:grid-cols-2">
			<div class="card">
				<h3 class="font-display text-lg font-semibold text-foundry-text">
					It does not find or clone repositories
				</h3>
				<p class="mt-3 text-sm text-foundry-secondary">
					Every entry in the sweep is a local checkout that the resolved config already names.
					Discovery is open work (#5215); until it lands, naming a GitHub organisation or a
					Bitbucket workspace in <code class="text-xs">--org</code> gets you a report title, not a repository
					list.
				</p>
			</div>

			<div class="card">
				<h3 class="font-display text-lg font-semibold text-foundry-text">
					There is no engineering-velocity section
				</h3>
				<p class="mt-3 text-sm text-foundry-secondary">
					DOC-67 §8 specifies one; it is not implemented (#5241, #5242). DORA is computed at stage 6
					and never reaches the report. If you need deployment frequency or change failure rate in
					the deliverable, this does not produce it today.
				</p>
			</div>

			<div class="card">
				<h3 class="font-display text-lg font-semibold text-foundry-text">
					§6.1 is linter output, not a security scan
				</h3>
				<p class="mt-3 text-sm text-foundry-secondary">
					It carries general-purpose linter findings — clippy, ruff, biome, rubocop, PMD. A linter
					flagging an <code class="text-xs">unwrap()</code> is a different claim from a scanner flagging
					SQL injection or a hardcoded credential, and this section is only ever making the first one.
				</p>
			</div>

			<div class="card">
				<h3 class="font-display text-lg font-semibold text-foundry-text">
					Six registers have no data source
				</h3>
				<p class="mt-3 text-sm text-foundry-secondary">
					§4 Benchmark Position, and §6.2 through §6.6 — CVE exposure, license and IP risk,
					obsolescence, cloud readiness, remediation economics. Each is declared a gap. None is
					estimated.
				</p>
			</div>

			<div class="card">
				<h3 class="font-display text-lg font-semibold text-foundry-text">
					<code class="text-base">agentic_pct</code> counts markers, not AI
				</h3>
				<p class="mt-3 text-sm text-foundry-secondary">
					The undercount that made this figure unquotable is fixed (#5249, #5403): measured against
					tga's own history the catch rate went from 47.70% to 91.04%. What the number
					<em>means</em> did not change. Detection is marker-based only — the run's own disclosure line
					says so — so commits whose trailers or footers were stripped, squashed, or rewritten are indistinguishable
					from human commits, and a low share means "no markers emitted", not "no AI assistance". Twelve
					markers cover seven tool labels, and five of those (Devin, OpenHands, Aider, Copilot, Cursor)
					are exercised only by synthetic tests, because this repository's history contains none of their
					markers. Read it as a floor on marker-emitting work, not as a provenance figure.
				</p>
			</div>

			<div class="card">
				<h3 class="font-display text-lg font-semibold text-foundry-text">
					No PERFORMANCE or COST dimension exists
				</h3>
				<p class="mt-3 text-sm text-foundry-secondary">
					Nothing in the pipeline measures runtime behaviour or spend, so neither is scored. Both
					appear in Gaps &amp; Caveats, which is the whole design: the report would rather be
					visibly incomplete than quietly wrong.
				</p>
			</div>
		</div>
	</div>

	<!-- ============ close ============ -->
	<div class="min-w-0">
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Start here</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Configure your repository set, then run the sweep. Nothing in it will prompt you, and nothing
			in it will stop halfway.
		</p>
		<div class="mt-6 max-w-xl min-w-0">
			<p class="eyebrow">shell</p>
			<pre
				class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">tga audit --org acme --output ./acme-dd</pre>
		</div>
	</div>
</ToolPage>
