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
 * Test: `src/lib/site.test.ts` grounds each install command in the repository
 * rather than in prose docs — the bootstrap URL resolves to a tracked file,
 * `cargo install --path` names a real crate, `tctl` names real stable-set
 * members — and pins the MSRV and Tier-1 platforms against their source of
 * truth.
 */

import { TOOLS, type Tool } from './tools';

export const GITHUB_URL = 'https://github.com/bobmatnyc/trusty-tools';

/**
 * Rendered by both `SiteHeader.svelte` and `SiteFooter.svelte`, which is why
 * the list lives here rather than in `+layout.svelte`.
 */
export const NAV_LINKS: { href: string; label: string }[] = [
	{ href: '/', label: 'Home' },
	{ href: '/docs', label: 'Docs' },
	{ href: '/whats-new', label: "What's new" }
];

/**
 * The six flagship crates, each with a hand-authored page under `/tools/`.
 * The records live in `./tools` because those pages need more per-crate detail
 * than a landing-page card does, and a second copy here would drift.
 */
export type Flagship = Tool;
export const FLAGSHIPS: Flagship[] = TOOLS;

export interface CrateGroup {
	group: string;
	crates: { name: string; description: string }[];
}

/**
 * Why: what a visitor wants below the six flagship cards is "what else can I
 * install, and what is coming" — not an inventory of every workspace member.
 * Internal plumbing is deliberately absent: shared libraries (`trusty-common`,
 * `trusty-progress`), private launchers, release tooling, and sidecars that
 * are installed BY another crate rather than on their own (`trusty-embedderd`,
 * whose own README says it is not installed standalone) have nothing to offer
 * a reader here. The six flagships are carded above and are not repeated.
 *
 * What: two groups, split on release state rather than on subject matter.
 * Placement was checked per crate against crates.io and this repository's
 * release tags on 2026-08-08, not inferred from version numbers —
 * `trusty-agents` reads 0.38.6 in its manifest while having no tag and no
 * crates.io release at all.
 *
 * Deliberately omitted here, like the crate COUNT above: any per-crate version
 * number. A number TYPED INTO THIS FILE is stale the next time that crate
 * ships. The six flagship cards do now show one, and that is not a reversal of
 * the same rule — `$lib/changelog` derives it from the crate's own
 * `CHANGELOG.md` at build time, so it cannot disagree with the repository. The
 * rule is "never hand-write a version", not "never show one".
 */
export const CRATE_GROUPS: CrateGroup[] = [
	{
		group: 'Also shipped',
		crates: [
			{ name: 'trusty-installer', description: 'The tctl install and upgrade control plane' },
			{ name: 'trusty-console', description: 'Web dashboard over the trusty services you run' },
			{ name: 'trusty-code', description: 'Per-project coding harness (tcode)' },
			{ name: 'trusty-gworkspace', description: 'Google Workspace MCP server' }
		]
	},
	{
		group: 'In development',
		crates: [
			{ name: 'trusty-agents', description: 'Agentic harness with multi-model routing (tagent)' },
			{ name: 'trusty-channels', description: 'Chat-channel MCP servers, starting with Slack' },
			{ name: 'trusty-kb', description: 'Personal knowledge base as an MCP server' },
			{ name: 'trusty-sld-lint', description: 'Linter for spec-linked documentation' },
			{ name: 'trusty-mpm-gui', description: 'Desktop dashboard for trusty-mpm' },
			{ name: 'trusty-code-gui', description: 'Desktop shell for the tcode daemon' }
		]
	}
];

export interface InstallOption {
	id: string;
	title: string;
	note: string;
	command: string;
}

/**
 * Why: an earlier revision took these from root `README.md` and
 * `docs/installation-tiers-draft.md`. Neither is a trustworthy source — the
 * draft is self-labelled and excluded from publication, and the README
 * undercounts the tap and omits a supported platform. The verified source is
 * `docs/research/install-paths-by-audience.md` (#5109), which checked all nine
 * install paths against crates.io and the release tooling and recommends
 * centring on `tctl`. Every command below was additionally re-verified on
 * 2026-08-07; see `site.test.ts` for what is machine-checked.
 *
 * What: three paths, in the order the research recommends them.
 */
export const INSTALL_OPTIONS: InstallOption[] = [
	{
		// install.sh is tracked at the repo root and its raw URL returns 200;
		// `tctl install trusty-search --dry-run` resolves and exits 0.
		id: 'tctl',
		title: 'Bootstrap, then tctl',
		note: 'Recommended. No Rust toolchain needed on a supported platform. Resolves the runtime dependency graph, and keeps macOS signing grants stable across upgrades.',
		command:
			'curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh\ntctl install'
	},
	{
		// Tap `bobmatnyc/homebrew-trusty` carries ten formulae; every
		// darwin-arm64 asset URL returned HTTP 200 on 2026-08-07. The
		// fully-qualified formula name is what the tap's own README documents.
		id: 'homebrew',
		title: 'Homebrew',
		note: 'Ten formulae in the tap. Installs one binary, without the dependency resolution tctl does.',
		command: 'brew tap bobmatnyc/trusty\nbrew install bobmatnyc/trusty/trusty-search'
	},
	{
		// `cargo install --path` — never `cargo build` plus a copy. A plain `cp`
		// over an on-PATH binary leaves a stale macOS cdhash cache and the next
		// exec is SIGKILLed (CLAUDE.md, "CRITICAL macOS note").
		id: 'cargo',
		title: 'From source',
		note: 'Requires Rust 1.94. cargo install writes atomically — never copy a built binary onto your PATH on macOS.',
		command:
			'git clone https://github.com/bobmatnyc/trusty-tools\ncd trusty-tools\ncargo install --path crates/trusty-search --locked'
	}
];

/** The seven crates `tctl install` manages, from `stable_set.rs`. */
export const STABLE_SET = [
	'trusty-search',
	'trusty-memory',
	'trusty-analyze',
	'trusty-review',
	'tga',
	'trusty-console',
	'trusty-mpm'
];

/**
 * MSRV is `rust-version` in the root Cargo.toml. Platforms are the three
 * Tier-1 triples in `crates/trusty-installer/src/download/platform.rs` — root
 * README.md lists only two and omits Linux arm64.
 */
export const FACTS: { label: string; value: string }[] = [
	{ label: 'License', value: 'MIT' },
	{ label: 'MSRV', value: 'Rust 1.94' },
	{ label: 'Prebuilt for', value: 'macOS arm64, Linux x86_64, Linux arm64' }
];
