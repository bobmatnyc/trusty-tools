/**
 * Why: keeping the landing page's factual content in one typed module makes it
 * reviewable as data rather than as markup, and gives every claim a single
 * place to be checked against its source. Every string below is drawn from the
 * repository root `README.md` or `CLAUDE.md` — nothing here is a product
 * promise, benchmark, or roadmap item invented for the site.
 *
 * What: navigation, external URLs, the flagship-server summaries, the crate
 * lineup, and the install commands rendered by `src/routes/+page.svelte`.
 *
 * Deliberately omitted: the crate COUNT. Root `README.md` says "21 crates"
 * while `crates/` currently holds 27 directories, so any number printed here
 * would ship one of two wrong answers and drift again on the next crate.
 *
 * Test: `src/lib/site.test.ts` asserts the install commands and the MSRV stay
 * in sync with README.md, that every named crate exists under `crates/`, and
 * that no entry is left as placeholder text.
 */

export const GITHUB_URL = 'https://github.com/bobmatnyc/trusty-tools';

export const NAV_LINKS: { href: string; label: string }[] = [
	{ href: '/', label: 'Home' },
	{ href: '/docs', label: 'Docs' }
];

export interface Flagship {
	name: string;
	unit: string;
	tagline: string;
	points: string[];
}

/** The three flagship MCP servers, per README.md's "Three Flagship MCP Servers". */
export const FLAGSHIPS: Flagship[] = [
	{
		name: 'trusty-search',
		unit: 'UNIT 01',
		tagline: 'Hybrid code search',
		points: [
			'BM25, vector, and knowledge-graph retrieval fused with Reciprocal Rank Fusion',
			'One machine-wide daemon, unlimited named project indexes',
			'Query-intent routing across definition, usage, conceptual, and bug/debt lookups',
			'Branch-aware ranking and caller/callee chain expansion'
		]
	},
	{
		name: 'trusty-memory',
		unit: 'UNIT 02',
		tagline: 'Memory palace storage',
		points: [
			'HNSW vector index over SQLite, embedded with fastembed',
			'Semantic recall across everything you have stored',
			'Collections for notes, snippets, code patterns, and decisions',
			'Svelte UI for browsing and editing, plus an MCP server'
		]
	},
	{
		name: 'trusty-analyze',
		unit: 'UNIT 03',
		tagline: 'Code analysis sidecar',
		points: [
			'Cyclomatic and cognitive complexity per chunk, file, and index',
			'Configurable code-smell categories and A–F quality grading',
			'Git-blame temporal decay to surface stale, complex code',
			'Tree-sitter adapters for 14 languages; HTTP and MCP parity'
		]
	}
];

export interface CrateGroup {
	group: string;
	crates: { name: string; description: string }[];
}

/** Crate lineup, transcribed from README.md's "Full Crate Index" tables. */
export const CRATE_GROUPS: CrateGroup[] = [
	{
		group: 'Daemons & MCP servers',
		crates: [
			{ name: 'trusty-search', description: 'Hybrid code search (BM25 + vector + KG)' },
			{ name: 'trusty-memory', description: 'Memory palace UI and MCP frontend' },
			{ name: 'trusty-analyze', description: 'Complexity, smells, and a facts store' },
			{ name: 'trusty-bm25-daemon', description: 'Standalone BM25 full-text search daemon' }
		]
	},
	{
		group: 'Shared libraries',
		crates: [
			{ name: 'trusty-common', description: 'Tracing, daemon helpers, shared utilities' },
			{ name: 'trusty-embedderd', description: 'fastembed wrapper, 384-dimension output' },
			{ name: 'trusty-gworkspace', description: 'Google Workspace client' },
			{ name: 'trusty-cto-db', description: 'SQLite schema and rusqlite bindings' }
		]
	},
	{
		group: 'Platform & agents',
		crates: [
			{ name: 'trusty-mpm', description: 'Multi-agent platform: CLI, daemon, MCP server' },
			{ name: 'trusty-mpm-gui', description: 'Desktop GUI, built with Tauri' },
			{ name: 'trusty-agents', description: 'Agent orchestration platform' },
			{ name: 'trusty-installer', description: 'Install and upgrade orchestrator (tctl)' }
		]
	},
	{
		group: 'Tools & analytics',
		crates: [
			{ name: 'trusty-review', description: 'Code review automation and analysis' },
			{ name: 'trusty-git-analytics', description: 'Developer analytics from git history' },
			{ name: 'trusty-console', description: 'Terminal UI for system monitoring' },
			{ name: 'trusty-code', description: 'Code generation and analysis utilities' }
		]
	}
];

export interface InstallOption {
	id: string;
	title: string;
	note: string;
	command: string;
}

/** Install paths, transcribed from README.md's "Installation" section. */
export const INSTALL_OPTIONS: InstallOption[] = [
	{
		id: 'bootstrap',
		title: 'Bootstrap installer',
		note: 'No Rust toolchain required. Downloads a SHA-256 verified binary.',
		command:
			'curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh'
	},
	{
		id: 'homebrew',
		title: 'Homebrew',
		note: 'Six published binaries live in the tap.',
		command: 'brew tap bobmatnyc/trusty\nbrew install trusty-search'
	},
	{
		id: 'cargo',
		title: 'From source',
		note: 'Requires Rust 1.94 or newer.',
		command: 'git clone https://github.com/bobmatnyc/trusty-tools\ncargo build --release'
	}
];

/** Facts stated verbatim in README.md's "Workspace Info". */
export const FACTS: { label: string; value: string }[] = [
	{ label: 'License', value: 'MIT' },
	{ label: 'MSRV', value: 'Rust 1.94' },
	{ label: 'Platforms', value: 'macOS (Apple Silicon), Linux (x86_64)' }
];
