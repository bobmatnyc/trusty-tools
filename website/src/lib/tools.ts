/**
 * Why: the flagship crates each get a hand-authored page under
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

/**
 * The workspace bootstrap, printed above every `tctl install` line.
 *
 * `$lib/site`'s `INSTALL_OPTIONS` carries the same URL for the landing page and
 * is the copy `site.test.ts` resolves against a tracked file.
 */
const TCTL_BOOTSTRAP =
	'curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh';

/**
 * How a crate is installed, and therefore what its Install section prints.
 *
 * Why a union rather than one string: `tctl` manages the crates in
 * `STABLE_SET`, and trusty-audit is not one of them — it is `publish = false`
 * and ships its own bootstrap script. Flattening both into a single command
 * string would leave the tctl prose on a page whose crate tctl cannot install.
 */
export type ToolInstall =
	| {
			via: 'tctl';
			/** `tctl install <target>`. Must be a `STABLE_SET` member. */
			target: string;
	  }
	| {
			via: 'script';
			/** The crate's own bootstrap one-liner, printed verbatim. */
			command: string;
			/** One sentence under the Install heading, in place of the tctl prose. */
			note: string;
	  };

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
	/** How the page's Install section reads. */
	install: ToolInstall;
	/**
	 * Whether this crate has a published release, and so appears on the
	 * release-driven surfaces — the card's "What's new" strip and `/whats-new`.
	 *
	 * `$lib/changelog/site` fails the build for a crate whose `CHANGELOG.md`
	 * parses to zero release sections, so a crate that has never shipped cannot
	 * be in them. trusty-audit is `publish = false` and carries no release tag
	 * yet; flip this to `true` in the same change that cuts its first release,
	 * and both surfaces pick it up.
	 */
	released: boolean;
	/**
	 * The doc reader's page for this crate, at `/docs/tools/<crate>` — a
	 * different URL from this page by one path segment. `null` for
	 * trusty-review and trusty-audit, which publish no doc page.
	 */
	docsPath: string | null;
}

/**
 * The command block a tool's Install section prints.
 *
 * Why here and not in the component: `tests/build-smoke.test.ts` asserts the
 * built page contains it, and re-deriving the two-line tctl form there would be
 * a second implementation of this concatenation.
 */
export function installCommand(tool: Tool): string {
	return tool.install.via === 'tctl'
		? `${TCTL_BOOTSTRAP}\ntctl install ${tool.install.target}`
		: tool.install.command;
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
		install: { via: 'tctl', target: 'trusty-search' },
		released: true,
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
		install: { via: 'tctl', target: 'trusty-memory' },
		released: true,
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
		install: { via: 'tctl', target: 'trusty-mpm' },
		released: true,
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
		install: { via: 'tctl', target: 'trusty-analyze' },
		released: true,
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
		install: { via: 'tctl', target: 'trusty-review' },
		released: true,
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
		install: { via: 'tctl', target: 'tga' },
		released: true,
		docsPath: '/docs/tools/trusty-git-analytics'
	},
	{
		slug: 'trusty-audit',
		name: 'trusty-audit',
		cargoPackage: 'trusty-audit',
		unit: 'UNIT 07',
		tagline: 'Audit engagements at a client site',
		points: [
			'One command downloads the macOS binary, verifies its checksum, and launches it',
			'Installs and version-pins the tga, trusty-search, trusty-analyze and trusty-review it runs',
			'Registers GitHub repositories and JIRA or Linear boards, checking each can be read first',
			'One resumable run — install, clone, audit, package — ending in a zip to send back'
		],
		lede: 'The client-side half of an audit engagement: it installs the tooling it pins, collects from the repositories you register, and writes one zip to return to your auditor.',
		install: {
			via: 'script',
			// #5873 adds `crates/trusty-audit/install.sh`; this is the Usage line
			// the script's own header documents.
			command:
				'curl -fsSL https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/crates/trusty-audit/install.sh | sh',
			note: 'One command, macOS on Apple Silicon only. It verifies the release tarball against its published SHA-256 before anything reaches your PATH, installs into ${CARGO_HOME:-$HOME/.cargo}/bin with an atomic rename, and then launches the binary.'
		},
		released: true,
		docsPath: null
	}
];
