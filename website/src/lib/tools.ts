/**
 * Why: the six flagship crates each get a hand-authored page under
 * `src/routes/tools/`, and the landing page cards link into them. Keeping the
 * shared per-tool facts here means the card and the page can never disagree
 * about a crate's name, package flag, install command, or docs link.
 *
 * What: one `Tool` record per flagship. Prose that appears on only ONE page
 * lives in that page's `.svelte` file; only what BOTH surfaces need is here.
 *
 * Sourcing rule: every string below was checked against the crate's own
 * source — `Cargo.toml`, the clap subcommand enums, the MCP tool descriptor
 * tables — not against a README. The crate READMEs carry several claims that
 * do not survive that check (a `TRUSTY_ALLOW_UNLISTED` bypass no production
 * code reads, a `search_code` tool that does not exist, stale tool counts),
 * so a README sentence is a draft here, never a source.
 *
 * Test: `src/lib/tools.test.ts` re-derives the crate directory, the package
 * name, the docs route, and the install target from the repository.
 */

export interface Tool {
	/** Route segment: the page lives at `/tools/<slug>`. */
	slug: string;
	/** Crate DIRECTORY name under `crates/` — not always the package name. */
	name: string;
	/** `cargo … -p <package>`; `crates/trusty-git-analytics` is package `tga`. */
	cargoPackage: string;
	/** Foundry unit stamp shown above the name. */
	unit: string;
	tagline: string;
	/** Landing-page card bullets. */
	points: string[];
	/** One-line summary for the page hero and its `<meta name="description">`. */
	lede: string;
	/** `tctl install` target. Every entry must be a `STABLE_SET` member. */
	install: string;
	/**
	 * The doc reader's page for this crate, at `/docs/tools/<crate>` — a
	 * different URL from this page by one path segment. `null` for
	 * trusty-review, which publishes no doc page.
	 */
	docsPath: string | null;
}

export const TOOLS: Tool[] = [
	{
		slug: 'trusty-search',
		name: 'trusty-search',
		cargoPackage: 'trusty-search',
		unit: 'UNIT 01',
		tagline: 'Hybrid code search',
		points: [
			'BM25, vector, and knowledge-graph retrieval fused with Reciprocal Rank Fusion',
			'One machine-wide daemon, unlimited named project indexes',
			'Query-intent routing across definition, usage, conceptual, and bug/debt lookups',
			'Branch-aware ranking and caller/callee chain expansion'
		],
		lede: 'Three retrieval lanes over one corpus, fused into a single ranking, served by one daemon for every project on the machine.',
		install: 'tctl install trusty-search',
		docsPath: '/docs/tools/trusty-search'
	},
	{
		slug: 'trusty-memory',
		name: 'trusty-memory',
		cargoPackage: 'trusty-memory',
		unit: 'UNIT 02',
		tagline: 'Memory palace storage',
		points: [
			'Named palaces, one per project, with rooms and wings inside them',
			'Hybrid BM25 + vector recall over an HNSW index and a redb store',
			'A knowledge graph of subject/predicate/object triples alongside the prose',
			'A dream cycle that consolidates near-duplicates instead of hoarding them'
		],
		lede: 'Long-term memory an assistant can write to and recall from across sessions, organised per project rather than per conversation.',
		install: 'tctl install trusty-memory',
		docsPath: '/docs/tools/trusty-memory'
	},
	{
		slug: 'trusty-mpm',
		name: 'trusty-mpm',
		cargoPackage: 'trusty-mpm',
		unit: 'UNIT 03',
		tagline: 'Multi-agent orchestration',
		points: [
			'One `tm` binary: daemon, CLI, TUI dashboard, and MCP server',
			'Sessions and worktrees per project, tracked across restarts',
			'Claude Code lifecycle hooks relayed into the daemon',
			'Remote control from Telegram or Slack when you are away from the terminal'
		],
		lede: 'A project manager for coding sessions: it provisions them, watches them, and keeps the roster straight while you work in several at once.',
		install: 'tctl install trusty-mpm',
		docsPath: '/docs/tools/trusty-mpm'
	},
	{
		slug: 'trusty-analyze',
		name: 'trusty-analyze',
		cargoPackage: 'trusty-analyze',
		unit: 'UNIT 04',
		tagline: 'Code analysis sidecar',
		points: [
			'Cyclomatic and cognitive complexity per chunk, file, and index',
			'Code-smell detection with configurable thresholds and named categories',
			'Git-blame temporal decay, so stale complex code sorts to the top',
			'Tree-sitter adapters for 14 languages, behind one HTTP API and one MCP server'
		],
		lede: 'A second daemon that reads trusty-search’s corpus and answers the question search cannot: not where the code is, but how bad it is.',
		install: 'tctl install trusty-analyze',
		docsPath: '/docs/tools/trusty-analyze'
	},
	{
		slug: 'trusty-review',
		name: 'trusty-review',
		cargoPackage: 'trusty-review',
		unit: 'UNIT 05',
		tagline: 'LLM code review',
		points: [
			'Reviews a GitHub PR, a git ref range, or a diff on stdin',
			'Injects code context from trusty-search and metrics from trusty-analyze',
			'A letter grade alongside an APPROVE / REQUEST_CHANGES / BLOCK verdict',
			'Skips a review it cannot ground rather than issue a confident guess'
		],
		lede: 'A reviewer that reads the rest of the repository before it reads your diff, and says so plainly when it cannot.',
		install: 'tctl install trusty-review',
		docsPath: null
	},
	{
		slug: 'trusty-git-analytics',
		name: 'trusty-git-analytics',
		cargoPackage: 'tga',
		unit: 'UNIT 06',
		tagline: 'Developer analytics from git',
		points: [
			'Walks local repositories into SQLite, then classifies every commit',
			'A tiered classification cascade, with an optional LLM tier at the end',
			'Per-author and per-week velocity, quality, and DORA reporting',
			'CSV, JSON, and Markdown output from one `tga analyze` run'
		],
		lede: 'Turns git history into per-author and per-week reporting, with a classification cascade that names the work each commit did.',
		install: 'tctl install tga',
		docsPath: '/docs/tools/trusty-git-analytics'
	}
];
