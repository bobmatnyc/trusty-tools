<script lang="ts">
	/**
	 * Why: `tga audit` is a deliverable in its own right — an acquirer's
	 * reviewer arrives looking for a due-diligence report, not for a git
	 * analytics crate — and appending it to `/tools/trusty-git-analytics`
	 * (PR #5415) buried it under four sections about `tga analyze`. It nests
	 * under the tool that ships it rather than becoming a seventh `/tools/`
	 * entry, because it is a subcommand, not a crate: `$lib/tools` records map
	 * one-to-one onto `crates/<name>/Cargo.toml`, and there is no
	 * `crates/tga-audit`.
	 *
	 * What: the whole `tga audit` page. Deliberately NOT built on
	 * `ToolPage.svelte`: that component takes a `Tool` record and derives its
	 * `<title>`, `<h1>`, source link, and install command from it, all of which
	 * would name the crate instead of the subcommand — and its single install
	 * line cannot express this page's actual precondition, which is two
	 * binaries and an optional daemon. The chrome below is written to match it
	 * (hero, facts strip, raised install band, closing card) rather than to
	 * import it.
	 *
	 * Sourcing rule, inherited from `$lib/tools`: every claim here was checked
	 * against crate source. The prerequisites in particular —
	 * `crates/trusty-git-analytics/src/audit/review.rs` spawns
	 * `trusty-review report --manifest … --analyze --synthesize --out …` and
	 * hard-errors on `BinaryNotFound`; `commands/audit.rs` calls
	 * `require_inference_credential()` before stage 1 (#5454), so the key card is a
	 * real precondition; and `crates/trusty-review/src/report/analyze_adapter.rs`
	 * is fail-open against the analyze daemon — not against a README.
	 *
	 * No version number appears in the copy, for the reason
	 * `$lib/site.ts` gives: a version typed into this file is stale the next
	 * time the crate ships (#5417).
	 *
	 * Test: `tests/build-smoke.test.ts` asserts this route prerenders, carries
	 * its own title, and is reachable from the tool page;
	 * `tests/mobile-overflow.test.ts` measures it at 375px and 320px.
	 */
	import { GITHUB_URL } from '$lib/site';

	const SOURCE_URL = `${GITHUB_URL}/tree/main/crates/trusty-git-analytics/src/audit`;

	const facts = [
		{ label: 'Command', value: 'tga audit' },
		{ label: 'Stages', value: '8' },
		{ label: 'Flags', value: '6, all optional' },
		{ label: 'Output', value: 'up to 18 files' },
		{ label: 'Prompts', value: 'none' }
	];
</script>

<svelte:head>
	<title>tga audit — acquisition due diligence from git history — trusty-tools</title>
	<meta
		name="description"
		content="One non-interactive command walks the git history of a configured repository set and renders a technical due-diligence report, naming what it could not measure instead of scoring it."
	/>
</svelte:head>

<!-- The hero, the raised install band, and the closing card below are a
     deliberate COPY of `$lib/components/ToolPage.svelte`'s chrome, not an
     import — see the reasoning in the script block above. Nothing propagates
     between them: a visual change to either must be made in both. -->

<!-- HERO -->
<section class="border-b border-foundry-border">
	<div class="mx-auto max-w-content px-4 py-14 sm:px-6 sm:py-20">
		<p class="eyebrow">
			<a href="/tools/trusty-git-analytics" class="hover:text-foundry-primary"
				>trusty-git-analytics</a
			> · Acquisition due diligence
		</p>
		<h1
			class="mt-4 break-words font-display text-4xl font-bold leading-tight tracking-tight text-foundry-primary sm:text-5xl"
		>
			<code>tga audit</code>
		</h1>
		<p class="mt-6 max-w-2xl text-lg text-foundry-secondary">
			One command walks the git history of the repositories your config already names, and renders a
			technical due-diligence report for someone deciding whether to buy the codebase. The report
			has eight sections, and the parts it cannot fill are named as gaps rather than scored.
		</p>

		<div class="mt-8 flex flex-wrap gap-3">
			<a href="#install" class="btn btn-primary">Install and run</a>
			<a href="/tools/trusty-git-analytics" class="btn btn-secondary">trusty-git-analytics</a>
			<a href={SOURCE_URL} rel="noreferrer noopener" class="btn btn-secondary">Source</a>
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

<!-- BODY, part one — the thesis, before the mechanics. -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="flex flex-col gap-12">
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">
				The gaps are printed, not filled in
			</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				A due-diligence report is read by someone deciding whether to buy the thing it describes.
				The failure that matters there is not a missing number. It is a number nobody measured,
				printed as though somebody had.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				<code class="text-sm">tga audit</code> has no PERFORMANCE dimension and no COST dimension — nothing
				in the pipeline measures either one. So the report declares both as gaps rather than scoring them
				off a proxy.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				Failure works the same way. Point the sweep at a config with no JIRA project key and the
				<code class="text-sm">jira sync</code> stage fails. That failure becomes a named line in
				<strong class="font-semibold text-foundry-text">Gaps &amp; Caveats</strong>. The alternative
				— a zero in a cell — is indistinguishable from a project that genuinely had no JIRA
				activity, and the reader has no way to tell which one they are looking at.
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
	</div>
</section>

<!-- INSTALL — the raised band, matching the tool pages' own install section. -->
<section class="border-y border-foundry-border bg-foundry-raised">
	<div class="mx-auto max-w-content px-4 py-16 sm:px-6">
		<h2 id="install" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">
			What you need installed
		</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Two binaries, and one daemon you can skip. <code class="text-sm">tga</code> collects and
			classifies; <code class="text-sm">trusty-review</code> renders the report at the end of the
			sweep. They meet at a file — the manifest — rather than at a library edge, which is why the
			renderer is a separate install rather than something
			<code class="text-sm">tga</code> carries.
		</p>

		<div class="mt-6 max-w-xl min-w-0">
			<p class="eyebrow">shell</p>
			<pre
				class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
tctl install tga
tctl install trusty-review</pre>
		</div>

		<p class="mt-6 max-w-3xl text-sm text-foundry-secondary">
			Homebrew and <code class="text-sm">cargo install</code> from a checkout both work too; the
			<a href="/#install" class="text-foundry-primary underline underline-offset-2"
				>other install paths</a
			>
			are on the home page. Whatever route you take, the sweep looks for
			<code class="text-sm">trusty-review</code> on your PATH — set
			<code class="text-sm">TRUSTY_REVIEW_BIN</code> to a full path if it lives somewhere PATH cannot
			see it.
		</p>

		<div class="mt-8 grid max-w-3xl gap-4 sm:grid-cols-2">
			<div class="card bg-foundry-card">
				<span class="badge">Required</span>
				<h3 class="mt-3 font-display text-lg font-semibold text-foundry-text">
					<code class="text-base">trusty-review</code>
				</h3>
				<p class="mt-2 text-sm text-foundry-secondary">
					Without it the sweep still runs and still writes
					<code class="text-xs">manifest.toml</code>, then stops with an error naming that file.
					Nothing collected is lost — see the recovery command below.
				</p>
			</div>
			<div class="card bg-foundry-card">
				<span class="badge">Required</span>
				<h3 class="mt-3 font-display text-lg font-semibold text-foundry-text">
					<code class="text-base">OPENROUTER_API_KEY</code>
				</h3>
				<p class="mt-2 text-sm text-foundry-secondary">
					The renderer writes the report's analysis with a model, so an audit cannot finish
					without a key. It is checked before stage 1, not at the end, so an unset one costs
					you the error and not the sweep:
					<code class="text-xs">export OPENROUTER_API_KEY=…</code>
				</p>
			</div>
			<div class="card bg-foundry-card">
				<span class="badge">Optional</span>
				<h3 class="mt-3 font-display text-lg font-semibold text-foundry-text">
					<code class="text-base">trusty-analyze</code>
				</h3>
				<p class="mt-2 text-sm text-foundry-secondary">
					The renderer asks a running analyze daemon for the findings and the complexity
					distribution, and shrugs when it is absent: the report is produced either way, and the
					sections it feeds become declared gaps instead.
				</p>
			</div>
		</div>
	</div>
</section>

<!-- BODY, part two — configure, run, and everything the sweep produces. -->
<section class="mx-auto max-w-content px-4 py-16 sm:px-6">
	<div class="flex flex-col gap-12">
		<!-- ============ configuring ============ -->
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">Point it at a repository set</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				<code class="text-sm">tga audit</code> audits local checkouts that a config file names. It
				does not discover or clone anything, so this step is the one that decides what gets audited.
				<code class="text-sm">tga install</code> is an interactive wizard that writes the config for you;
				a hand-written one can be as small as a list of repository paths, because every other section
				has a default.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				<code class="text-sm">-c, --config &lt;FILE&gt;</code> is global on
				<code class="text-sm">tga</code>, so it goes before the subcommand. A missing config logs a
				warning and runs on defaults rather than failing — which is worth knowing, because an audit
				of nothing is a report of nothing.
			</p>
		</div>

		<!-- ============ running it ============ -->
		<div class="min-w-0">
			<h2 id="running" class="scroll-mt-24 font-display text-2xl font-bold sm:text-3xl">
				Running it
			</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				Six flags, all optional, nothing interactive. Once it starts it never prompts, confirms, or
				waits for input — start it and walk away.
			</p>

			<div class="mt-6 max-w-xl min-w-0">
				<p class="eyebrow">shell — defaults</p>
				<pre
					class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">tga audit</pre>
			</div>

			<p class="mt-4 max-w-3xl text-sm text-foundry-secondary">
				Bare, it reads <code class="text-sm">./config.yaml</code> and writes
				<code class="text-sm">./audit-output</code>. That is the whole minimal invocation.
			</p>

			<div class="mt-6 max-w-xl min-w-0">
				<p class="eyebrow">shell — named engagement</p>
				<pre
					class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">tga --config config.yaml audit \
  --org acme --client "Acme Holdings" --analyst "J. Reviewer" \
  --weeks 26 --output ./acme-dd</pre>
			</div>

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
								>The organisation under audit. Metadata only — it titles the report and nothing
								else.</td
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
					config already names, and <code class="text-sm">--org</code> only reaches the report's title
					block.
				</p>
			</div>
		</div>

		<!-- ============ reading the result ============ -->
		<div class="min-w-0">
			<h2 class="font-display text-2xl font-bold sm:text-3xl">Where the report comes out</h2>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				Everything lands in the output directory — <code class="text-sm">./audit-output</code>
				unless
				<code class="text-sm">--output</code> says otherwise. The run ends by printing the paths it
				wrote under
				<strong class="font-semibold text-foundry-text">Report artifacts</strong>, so the last lines
				on your terminal are the deliverable. The Markdown file is the report; open it first.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				If the sweep stopped at the render — the renderer was not installed, or the model call
				failed — the manifest survived it, because it is written before the renderer is called.
				Fix the cause and run the last stage by hand; this is exactly what the sweep would have
				run:
			</p>

			<div class="mt-6 max-w-xl min-w-0">
				<p class="eyebrow">shell — render an existing manifest</p>
				<pre
					class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-3 text-xs leading-relaxed text-foundry-text">trusty-review report \
  --manifest ./audit-output/manifest.toml \
  --analyze --synthesize --out ./audit-output</pre>
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
			     states the ordering decides the result, so the reader needs the index. -->
			<ol class="mt-6 max-w-3xl list-none space-y-3 text-foundry-secondary">
				<li class="flex gap-3">
					<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary"
						>1</span
					>
					<span
						><code class="text-sm">collect</code> — walks the configured repositories into
						<code class="text-sm">commits</code> via git2.</span
					>
				</li>
				<li class="flex gap-3">
					<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary"
						>2</span
					>
					<span
						><code class="text-sm">classify</code> — runs the four-tier classification cascade over those
						commits.</span
					>
				</li>
				<li class="flex gap-3">
					<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary"
						>3</span
					>
					<span
						><code class="text-sm">jira sync</code> — ingests JIRA transitions and comments.</span
					>
				</li>
				<li class="flex gap-3">
					<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary"
						>4</span
					>
					<span
						><code class="text-sm">deployments collect</code> — deploy events into
						<code class="text-sm">fact_deployments</code>.</span
					>
				</li>
				<li class="flex gap-3">
					<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary"
						>5</span
					>
					<span
						><code class="text-sm">incidents collect</code> — incidents into
						<code class="text-sm">fact_incidents</code>.</span
					>
				</li>
				<li class="flex gap-3">
					<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary"
						>6</span
					>
					<span
						><code class="text-sm">dora</code> — reduces those two fact tables to the four DORA keys.</span
					>
				</li>
				<li class="flex gap-3">
					<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary"
						>7</span
					>
					<span
						><code class="text-sm">pr-metrics</code> — per-engineer pull-request metrics; writes
						<code class="text-sm">pr-metrics.csv</code>.</span
					>
				</li>
				<li class="flex gap-3">
					<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary"
						>8</span
					>
					<span
						><code class="text-sm">report</code> — renders tga's own CSV, JSON and Markdown.</span
					>
				</li>
			</ol>

			<div class="card mt-8 max-w-3xl">
				<p class="eyebrow">No stage aborts the run</p>
				<p class="mt-3 text-foundry-secondary">
					Every stage's result is recorded, never propagated. A stage that fails is marked failed,
					the sweep moves to the next one, and the failure surfaces as a named gap in the report
					instead of a halt. One thing stops a sweep: a pre-flight failure creating the output
					directory.
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
			<h2 class="font-display text-2xl font-bold sm:text-3xl">
				What lands in the output directory
			</h2>
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
				Eight sections, in this order. A model writes the executive summary, the top-risk
				rationale, and the RED/AMBER finding prose, working only from what the sweep collected —
				every figure it cites is checked against that data and the sentence is dropped if the
				figure is not there. Section 2 falls back to a roll-up composed from the report's own
				counts whenever that check rejects the written one.
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
			<h2 class="font-display text-2xl font-bold sm:text-3xl">
				What the last fix actually changed
			</h2>
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
				The 20 pre-fix RED entries were all "Extract method" refactor suggestions misrouted into
				RED. The two non-code components were <code class="text-sm">CHANGELOG.md</code> and
				<code class="text-sm">FAQ.md</code>, scored as if they were applications. The complexity
				buckets summed to a round 1,000 instead of the 4,328 units actually counted.
			</p>
			<p class="mt-4 max-w-3xl text-foundry-secondary">
				All three are the same defect wearing different clothes: output that looked like a
				measurement and was not one.
			</p>

			<div class="card mt-8 max-w-3xl">
				<p class="eyebrow">Scope of that evidence</p>
				<p class="mt-3 text-foundry-secondary">
					These numbers are the ripgrep run only. The trusty-tools audit was not re-run after the
					fix, so nothing here says the same before-and-after holds on a second codebase.
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
						DOC-67 §8 specifies one; it is not implemented (#5241, #5242). DORA is computed at stage
						6 and never reaches the report. If you need deployment frequency or change failure rate
						in the deliverable, this does not produce it today.
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
						The undercount that made this figure unquotable is fixed (#5249, #5403): measured
						against tga's own history the catch rate went from 47.70% to 91.04%. What the number
						<em>means</em> did not change. Detection is marker-based only — the run's own disclosure line
						says so — so commits whose trailers or footers were stripped, squashed, or rewritten are indistinguishable
						from human commits, and a low share means "no markers emitted", not "no AI assistance". Twelve
						markers cover seven tool labels, and five of those (Devin, OpenHands, Aider, Copilot, Cursor)
						are exercised only by synthetic tests, because this repository's history contains none of
						their markers. Read it as a floor on marker-emitting work, not as a provenance figure.
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
	</div>
</section>

<!-- CLOSE — the four steps, in order, and the way back. -->
<section class="mx-auto max-w-content px-4 pb-16 sm:px-6">
	<div class="card max-w-3xl min-w-0">
		<p class="eyebrow">Start to finish</p>
		<h2 class="mt-3 font-display text-xl font-semibold">Four steps to a report</h2>
		<ol class="mt-4 space-y-3 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">1</span
				>
				<span
					>Install <code class="text-sm">tga</code> and
					<code class="text-sm">trusty-review</code>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">2</span
				>
				<span
					>Write <code class="text-sm">config.yaml</code>, by hand or with
					<code class="text-sm">tga install</code>.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">3</span
				>
				<span>Run the sweep. It will not ask you anything.</span>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="w-5 shrink-0 font-mono text-sm text-foundry-primary">4</span
				>
				<span
					>Open the Markdown file whose path it printed last, and read
					<strong class="font-semibold text-foundry-text">Gaps &amp; Caveats</strong> before the scores.</span
				>
			</li>
		</ol>
		<div class="mt-6 max-w-xl min-w-0">
			<p class="eyebrow">shell</p>
			<pre
				class="mt-2 overflow-x-auto rounded-sm border border-foundry-border bg-foundry-raised p-3 text-xs leading-relaxed text-foundry-text">tga audit --org acme --output ./acme-dd</pre>
		</div>
	</div>
	<p class="mt-8">
		<a
			href="/tools/trusty-git-analytics"
			class="text-sm text-foundry-primary underline underline-offset-2"
			>← Back to trusty-git-analytics</a
		>
	</p>
</section>
