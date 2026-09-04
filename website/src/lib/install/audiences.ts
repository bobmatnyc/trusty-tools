/**
 * Why: nine products install nine different ways, and the differences that
 * matter most are the ones a reader cannot guess — `tctl install trusty-code`
 * fails, `tctl install trusty-review` quietly installs three crates, and the
 * macOS permission a product needs is per-product and easy to publish
 * backwards. Keeping the walkthrough as data rather than as markup means each
 * of those facts has one place to be checked, and the page cannot show a
 * command for one audience while claiming a different one for another.
 *
 * What: nine `Audience` rows, plus the `Prerequisite` rows they share. A
 * prerequisite is written ONCE and referenced by id, so the page renders the
 * `tctl` bootstrap and the API-key setup a single time instead of nine.
 *
 * Sourcing rule, stricter than `$lib/tools`: every command, env-var name, TCC
 * category, API-key requirement and MCP registration below comes from
 * `docs/research/install-paths-by-audience.md` (#5109, its UNKNOWNs resolved
 * in #5116/#6725), which checked all nine paths against crate source,
 * crates.io and the release tooling. Nothing here was derived from a README or
 * from an adjacent product's shape. Two deliberate, marked exceptions:
 *
 *   - `export <NAME>=<value>` lines. The env-var NAMES are verbatim from that
 *     doc; `export` is the shell mechanism for setting them, which the doc
 *     states as a requirement rather than as a command line.
 *   - the `.mcp.json` wrapper object. `command` and `args` are verbatim from
 *     the doc; the surrounding `mcpServers` key is the config file's own
 *     schema, not a claim about any crate.
 *
 * Anything the doc leaves as prose stays prose here — tga's `--config` flag
 * and trusty-agents' signing script are named, not turned into a command
 * block, because the doc gives no exact invocation for either and inventing
 * one is the failure mode this module exists to prevent.
 *
 * Test: `src/lib/install/audiences.test.ts` re-derives crate directories and
 * `tctl` stable-set membership from the repository and pins the TCC
 * invariants; `src/lib/install/render.test.ts` mounts the walkthrough and
 * asserts every command reaches the DOM under its own audience.
 */

/** A shell (or config-file) block rendered with its own copy button. */
export interface CommandBlock {
	/** Exact text to copy — what lands in the terminal, verbatim. */
	command: string;
	/** Accessible name for the copy button, e.g. "Copy the tctl bootstrap". */
	label: string;
	/**
	 * What to substitute. REQUIRED whenever `command` contains a
	 * `<placeholder>`: a block that ships a placeholder with no instruction is
	 * what `audiences.test.ts` fails on.
	 */
	placeholderNote?: string;
}

/** Does this command carry a `<placeholder>` the reader must replace? */
export function hasPlaceholder(command: string): boolean {
	return /<[^<>\s]+>/.test(command);
}

export type PrerequisiteId =
	'tctl' | 'rust' | 'pnpm' | 'git' | 'ram-16' | 'ram-8' | 'llm-key' | 'claude-code';

/**
 * Setup more than one audience needs. Rendered once, above the picker, and
 * referenced by id from each audience — the deduplication this walkthrough is
 * built around.
 */
export interface Prerequisite {
	id: PrerequisiteId;
	title: string;
	body: string;
	commands: CommandBlock[];
}

/** How badly a given audience needs a prerequisite. */
export type Requirement = 'required' | 'recommended' | 'optional';

export interface PrerequisiteRef {
	id: PrerequisiteId;
	requirement: Requirement;
	/** What this audience uses it for, in the research doc's own terms. */
	note: string;
}

export interface InstallStep {
	title: string;
	body: string;
	commands: CommandBlock[];
	/** What the reader needs after running the commands. */
	notes: string[];
}

/**
 * The macOS permission category, kept as an enum rather than as prose.
 *
 * `full-disk-access` is trusty-search alone. `app-data` is the SEPARATE,
 * narrower "would like to access data from other apps" category `tm` and
 * `tagent` need; a page that offers those two the disk-wide category instead
 * is a security regression, which is why `audiences.test.ts` asserts the wider
 * category's name appears nowhere in their copy.
 */
export type TccCategory = 'none' | 'full-disk-access' | 'app-data';

export interface Audience {
	/** Picker id, and the `#install-<id>` panel anchor. */
	id: string;
	/** Short picker label. */
	label: string;
	/** Binary or binaries this audience ends up with. */
	binary: string;
	tagline: string;
	lede: string;
	prerequisites: PrerequisiteRef[];
	steps: InstallStep[];
	tcc: { category: TccCategory; summary: string };
}

/** The bootstrap line, identical to `$lib/site`'s `INSTALL_OPTIONS` tctl entry. */
const TCTL_BOOTSTRAP =
	'curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh';

/** `.mcp.json` entry for one server. The wrapper is the file's schema. */
function mcpEntry(name: string, command: string, args: string[]): string {
	return [
		'{',
		'  "mcpServers": {',
		`    "${name}": {`,
		`      "command": "${command}",`,
		`      "args": ${JSON.stringify(args)}`,
		'    }',
		'  }',
		'}'
	].join('\n');
}

export const PREREQUISITES: Prerequisite[] = [
	{
		id: 'tctl',
		title: 'The tctl control plane',
		body: 'tctl installs seven of the nine products on this page. It prefers a prebuilt tarball on macOS arm64, Linux x86_64 and Linux arm64, verifies its published SHA-256, and falls back to cargo install --locked from crates.io on any other host. It is also the only path that resolves a product’s runtime dependencies for you and keeps macOS signing identities stable across upgrades, so a permission you grant once survives the next install.',
		commands: [{ command: TCTL_BOOTSTRAP, label: 'Copy the tctl bootstrap command' }]
	},
	{
		id: 'rust',
		title: 'A Rust toolchain',
		body: 'Rust 1.94 or newer — the workspace MSRV. Needed only when you build from source: on a prebuilt platform tctl never invokes cargo, and trusty-agents is the one product with no prebuilt at all.',
		commands: []
	},
	{
		id: 'pnpm',
		title: 'pnpm',
		body: 'Only trusty-agents needs it. trusty-search, trusty-memory and trusty-analyze commit their built Svelte UI to git, so installing those from crates.io never runs a JavaScript build. trusty-agents does not commit its UI: without pnpm the crate still compiles, but its build script writes a placeholder page and the embedded UI does nothing.',
		commands: []
	},
	{
		id: 'git',
		title: 'git',
		body: 'trusty-agents is installed by cloning this repository. trusty-code reads git metadata for branch context and tga reads git history through git2, so both want a git repository to point at.',
		commands: []
	},
	{
		id: 'ram-16',
		title: '16 GB RAM, ~2 GB disk',
		body: 'trusty-search checks available memory at startup and refuses to run below 16 GB; TRUSTY_SKIP_RAM_CHECK=1 bypasses that check. The disk is for the ONNX embedding model it downloads on first run. Apple Silicon uses CoreML automatically; NVIDIA CUDA is an opt-in --features cuda build.',
		commands: []
	},
	{
		id: 'ram-8',
		title: '8 GB RAM, ~500 MB disk',
		body: 'The floor for trusty-analyze and trusty-review, plus room for their model cache.',
		commands: []
	},
	{
		id: 'llm-key',
		title: 'A model provider key',
		body: 'OpenRouter is the default provider everywhere a key is read. It is required only for trusty-review, which will not produce a review without one. It is optional for trusty-memory, trusty-analyze, trusty-code and trusty-agents: each starts fine with no key and asks for one only when a feature that needs it actually runs. AWS Bedrock is the documented alternative for trusty-analyze and trusty-review.',
		commands: [
			{
				command: 'export OPENROUTER_API_KEY=<your-openrouter-key>',
				label: 'Copy the OpenRouter key export',
				placeholderNote:
					'Replace <your-openrouter-key> with the key from your own OpenRouter account — nothing on this page is a real key.'
			},
			{
				command: 'export TRUSTY_LLM_MODEL=<bedrock-model-id>\nexport AWS_REGION=<your-aws-region>',
				label: 'Copy the AWS Bedrock exports',
				placeholderNote:
					'Replace <bedrock-model-id> with a Bedrock model you have access to and <your-aws-region> with the region it is enabled in. These two replace the OpenRouter key rather than joining it.'
			}
		]
	},
	{
		id: 'claude-code',
		title: 'Claude Code',
		body: 'Recommended, never enforced at install time. trusty-mpm orchestrates Claude Code sessions, so it is what tm drives.',
		commands: []
	}
];

/** Every audience, in the order the picker lists them. */
export const AUDIENCES: Audience[] = [
	{
		id: 'trusty-memory',
		label: 'trusty-memory',
		binary: 'trusty-memory',
		tagline: 'Semantic memory, on its own',
		lede: 'A standalone daemon with no external database and nothing else to install first. tctl installs it alone — it has no runtime dependency on any other product.',
		prerequisites: [
			{ id: 'tctl', requirement: 'recommended', note: 'The install path this page recommends.' },
			{
				id: 'llm-key',
				requirement: 'optional',
				note: 'Only the embedded chat panel reads OPENROUTER_API_KEY. Everything else works without it.'
			}
		],
		steps: [
			{
				title: 'Install it',
				body: 'One member, no dependency closure.',
				commands: [
					{ command: 'tctl install trusty-memory', label: 'Copy the trusty-memory tctl install' },
					{
						command: 'cargo install trusty-memory --locked',
						label: 'Copy the trusty-memory cargo install'
					}
				],
				notes: [
					'The second line is the escape hatch: it pulls the published crates.io release directly, which is also what tctl falls back to off a prebuilt platform.',
					'The Svelte UI ships pre-built inside the crate, so this never runs pnpm.'
				]
			},
			{
				title: 'Find the port it serves on',
				body: 'The daemon picks its port at startup and reports it back. The UI is on the same port, on loopback.',
				commands: [{ command: 'trusty-memory port', label: 'Copy the trusty-memory port command' }],
				notes: []
			},
			{
				title: 'Register it as an MCP server',
				body: 'Add the entry to your .mcp.json. The command and args are what the crate’s own CLI parses.',
				commands: [
					{
						command: mcpEntry('trusty-memory', 'trusty-memory', ['serve', '--stdio']),
						label: 'Copy the trusty-memory MCP entry'
					}
				],
				notes: []
			}
		],
		tcc: {
			category: 'none',
			summary:
				'Nothing to grant. trusty-memory reads $HOME locations only, and it carries no signing identity because it has no permission to preserve.'
		}
	},
	{
		id: 'trusty-search',
		label: 'trusty-search',
		binary: 'trusty-search',
		tagline: 'Hybrid code search, on its own',
		lede: 'One machine-wide daemon over as many named project indexes as you like. It is a leaf in the dependency graph — tctl installs it alone.',
		prerequisites: [
			{ id: 'tctl', requirement: 'recommended', note: 'The install path this page recommends.' },
			{
				id: 'ram-16',
				requirement: 'required',
				note: 'Checked at startup. This is the highest floor of any product here.'
			}
		],
		steps: [
			{
				title: 'Install it',
				body: 'One member, no dependency closure.',
				commands: [
					{ command: 'tctl install trusty-search', label: 'Copy the trusty-search tctl install' },
					{
						command: 'cargo install trusty-search --locked',
						label: 'Copy the trusty-search cargo install'
					}
				],
				notes: ['The Svelte UI ships pre-built inside the crate, so this never runs pnpm.']
			},
			{
				title: 'Start the daemon and read its port',
				body: 'First start downloads the embedding model, which is what the ~2 GB of disk is for.',
				commands: [
					{ command: 'trusty-search start', label: 'Copy the trusty-search start command' },
					{ command: 'trusty-search port', label: 'Copy the trusty-search port command' }
				],
				notes: []
			},
			{
				title: 'Register it as an MCP server',
				body: 'Add the entry to your .mcp.json.',
				commands: [
					{
						command: mcpEntry('trusty-search', 'trusty-search', ['serve']),
						label: 'Copy the trusty-search MCP entry'
					}
				],
				notes: []
			}
		],
		tcc: {
			category: 'full-disk-access',
			summary:
				'trusty-search is the one product here that needs Full Disk Access, and only when its index data lives on an external or removable volume. An index on the local disk never triggers the prompt. Installing through tctl re-signs the binary under a stable identity, so the grant survives an upgrade.'
		}
	},
	{
		id: 'search-and-memory',
		label: 'search + memory',
		binary: 'trusty-search, trusty-memory',
		tagline: 'Both daemons together',
		lede: 'The common pairing: search indexes your code, memory stores what you and your agents learned about it. Neither calls the other — they are complementary, not layered, and each runs fine with the other absent.',
		prerequisites: [
			{ id: 'tctl', requirement: 'recommended', note: 'Installs both in one command.' },
			{
				id: 'ram-16',
				requirement: 'required',
				note: 'trusty-search sets the floor; trusty-memory adds no requirement of its own.'
			},
			{
				id: 'llm-key',
				requirement: 'optional',
				note: 'Only trusty-memory’s chat panel reads OPENROUTER_API_KEY.'
			}
		],
		steps: [
			{
				title: 'Install both',
				body: 'There is no dependency edge between them, so the order does not matter and they can be installed in parallel.',
				commands: [
					{
						command: 'tctl install trusty-search trusty-memory',
						label: 'Copy the combined tctl install'
					},
					{
						command: 'cargo install trusty-search --locked\ncargo install trusty-memory --locked',
						label: 'Copy the two cargo installs'
					}
				],
				notes: [
					'What the two share is a library, not a process: both link the same embedder out of trusty-common. Neither daemon talks to the other over the network.'
				]
			},
			{
				title: 'Register both MCP servers',
				body: 'Two independent entries in the same .mcp.json — each keeps the command and args from its own section.',
				commands: [
					{
						command: [
							'{',
							'  "mcpServers": {',
							'    "trusty-search": {',
							'      "command": "trusty-search",',
							'      "args": ["serve"]',
							'    },',
							'    "trusty-memory": {',
							'      "command": "trusty-memory",',
							'      "args": ["serve", "--stdio"]',
							'    }',
							'  }',
							'}'
						].join('\n'),
						label: 'Copy both MCP entries'
					}
				],
				notes: []
			}
		],
		tcc: {
			category: 'full-disk-access',
			summary:
				'Only trusty-search needs Full Disk Access, and only for indexes on an external volume. trusty-memory needs nothing — do not grant it anything on trusty-search’s account.'
		}
	},
	{
		id: 'trusty-analyze',
		label: 'trusty-analyze',
		binary: 'trusty-analyze',
		tagline: 'Complexity and smell analysis',
		lede: 'A leaf install: nothing requires it, and it requires nothing. It is an optional stable-set member, so a missing prebuilt for your platform will not fail a wider tctl run.',
		prerequisites: [
			{ id: 'tctl', requirement: 'recommended', note: 'The install path this page recommends.' },
			{ id: 'ram-8', requirement: 'required', note: 'Plus room for the model cache.' },
			{
				id: 'llm-key',
				requirement: 'optional',
				note: 'Complexity and smell analysis needs no model at all. Only the deep-analysis pass reads a key.'
			}
		],
		steps: [
			{
				title: 'Install it',
				body: 'One member, no dependency closure.',
				commands: [
					{ command: 'tctl install trusty-analyze', label: 'Copy the trusty-analyze tctl install' },
					{
						command: 'cargo install trusty-analyze --locked',
						label: 'Copy the trusty-analyze cargo install'
					}
				],
				notes: ['The Svelte UI ships pre-built inside the crate, so this never runs pnpm.']
			},
			{
				title: 'Register it as an MCP server',
				body: 'Add the entry to your .mcp.json. Both args are required — the crate generates exactly this pair itself.',
				commands: [
					{
						command: mcpEntry('trusty-analyze', 'trusty-analyze', ['serve', '--mcp']),
						label: 'Copy the trusty-analyze MCP entry'
					}
				],
				notes: []
			}
		],
		tcc: {
			category: 'none',
			summary:
				'Nothing to grant. trusty-analyze reads $HOME locations only, the same explicit carve-out trusty-memory gets.'
		}
	},
	{
		id: 'trusty-review',
		label: 'trusty-review',
		binary: 'trusty-review',
		tagline: 'Code review with real context',
		lede: 'The one product with a hard runtime dependency on two others. tctl installs three members for you — trusty-review, trusty-search and trusty-analyze — because a review produced without that context is worse than no review.',
		prerequisites: [
			{
				id: 'tctl',
				requirement: 'recommended',
				note: 'It resolves the three-member closure and orders it for you.'
			},
			{ id: 'ram-8', requirement: 'required', note: 'Plus room for the model cache.' },
			{
				id: 'llm-key',
				requirement: 'required',
				note: 'Not optional here, unlike trusty-analyze’s deep pass. Set OPENROUTER_API_KEY, or the two Bedrock variables instead.'
			}
		],
		steps: [
			{
				title: 'Install it, and what it needs',
				body: 'One tctl line installs three members. The cargo path installs one, so bring up trusty-search and trusty-analyze first if you take it.',
				commands: [
					{ command: 'tctl install trusty-review', label: 'Copy the trusty-review tctl install' },
					{
						command: 'cargo install trusty-review --locked',
						label: 'Copy the trusty-review cargo install'
					}
				],
				notes: [
					'trusty-review checks for both before it starts a review and skips the review entirely if either is unreachable, absent an explicit degraded-mode opt-in.',
					'Install order does not matter under tctl: it resolves the closure and orders it topologically.'
				]
			},
			{
				title: 'Point it at a model',
				body: 'Set the provider key from the prerequisites above before the first review — this is the one product that will not run without it.',
				commands: [],
				notes: []
			},
			{
				title: 'Register it as an MCP server',
				body: 'Add the entry to your .mcp.json.',
				commands: [
					{
						command: mcpEntry('trusty-review', 'trusty-review', ['mcp']),
						label: 'Copy the trusty-review MCP entry'
					}
				],
				notes: [
					'["serve", "--stdio"] still parses as an alias, so an older .mcp.json keeps working. ["mcp"] is the current spelling to write.'
				]
			}
		],
		tcc: {
			category: 'none',
			summary:
				'Nothing to grant, and this was checked rather than assumed: trusty-review was evaluated alongside trusty-memory and trusty-analyze and deliberately excluded — it walks no $HOME tree and reads no other application’s files.'
		}
	},
	{
		id: 'trusty-mpm',
		label: 'trusty-mpm (tm)',
		binary: 'tm, trusty-mpm',
		tagline: 'The session orchestrator',
		lede: 'tctl installs three members here — trusty-mpm, trusty-memory and trusty-search — because tm injects both MCP servers into every managed session by default.',
		prerequisites: [
			{
				id: 'tctl',
				requirement: 'recommended',
				note: 'It resolves the three-member closure and orders it for you.'
			},
			{
				id: 'claude-code',
				requirement: 'recommended',
				note: 'tm orchestrates Claude Code sessions. Not enforced at install time.'
			},
			{
				id: 'rust',
				requirement: 'optional',
				note: 'Only if no prebuilt exists for your platform.'
			}
		],
		steps: [
			{
				title: 'Install it, and check what landed',
				body: 'One line installs three binaries. tctl status prints what it manages and what is running.',
				commands: [
					{
						command: 'tctl install trusty-mpm\ntctl status',
						label: 'Copy the trusty-mpm install and status check'
					},
					{
						command: 'cargo install trusty-mpm --locked',
						label: 'Copy the trusty-mpm cargo install'
					}
				],
				notes: [
					'The crate ships two binaries, tm and trusty-mpm, from the same source — either name drives it.'
				]
			},
			{
				title: 'Know where its config lives',
				body: 'Configuration is TOML at ~/.config/trusty-mpm/config.toml, honouring $XDG_CONFIG_HOME when you set it.',
				commands: [],
				notes: [
					'Two older docs point at a config.yaml and at a trusty-mpmd binary. Neither exists: the file is TOML, and the daemon runs in-process as a mode of tm itself.'
				]
			},
			{
				title: 'Run the daemon',
				body: 'tm owns its own lifecycle rather than handing it to launchd.',
				commands: [{ command: 'tm start', label: 'Copy the tm start command' }],
				notes: ['tm stop and tm restart are the other two halves of the same verb set.']
			}
		],
		tcc: {
			category: 'app-data',
			summary:
				'tm needs the App Data category, and only that. It reads other applications’ $HOME containers — Claude config directories, tmux state — which raises the separate “would like to access data from other apps” prompt. Grant that category and nothing wider. Installing through tctl signs tm and trusty-mpm together under a stable identity, so the grant survives an upgrade.'
		}
	},
	{
		id: 'trusty-code',
		label: 'trusty-code',
		binary: 'tcode',
		tagline: 'A per-project coding harness',
		lede: 'Not a tctl product. trusty-code is published on crates.io but is not a stable-set member, so cargo install is the path — tctl install trusty-code fails with an unknown-member error.',
		prerequisites: [
			{ id: 'git', requirement: 'required', note: 'It reads git metadata for branch context.' },
			{
				id: 'claude-code',
				requirement: 'optional',
				note: 'Useful alongside it, never required by it.'
			},
			{
				id: 'llm-key',
				requirement: 'optional',
				note: 'Resolved lazily: tcode serve starts with no key, and a key is only demanded the first time a chat or task dispatches to a provider that needs one. OpenRouter is the default route.'
			}
		],
		steps: [
			{
				title: 'Install it from crates.io',
				body: 'The published crate, direct. There is no prebuilt tarball and no tctl membership.',
				commands: [
					{ command: 'cargo install trusty-code --locked', label: 'Copy the trusty-code install' }
				],
				notes: [
					'trusty-code has no Svelte build step, so pnpm is not involved at any point.',
					'Its README points at a GitHub release tag that has never existed — an unreplaced template placeholder. Use the line above.'
				]
			},
			{
				title: 'Run one process per project',
				body: 'The harness is scoped to a project’s .claude/ root, so it is one running process per project rather than one per machine.',
				commands: [{ command: 'tcode serve', label: 'Copy the tcode serve command' }],
				notes: []
			}
		],
		tcc: {
			category: 'none',
			summary:
				'No category applies. trusty-code implements no macOS permission state machine at all: it inherits whatever entitlements it is given, and a refusal from the OS surfaces as an ordinary error. It reads only the directory you navigate to.'
		}
	},
	{
		id: 'trusty-agents',
		label: 'trusty-agents (tagent)',
		binary: 'tagent',
		tagline: 'The agentic harness',
		lede: 'The only product here with no published crate and no release binary. Cloning this repository and building it is the install path, not a fallback from one.',
		prerequisites: [
			{ id: 'git', requirement: 'required', note: 'The install starts with a clone.' },
			{
				id: 'rust',
				requirement: 'required',
				note: 'Mandatory here — there is no prebuilt and no crates.io package to fall back to.'
			},
			{
				id: 'pnpm',
				requirement: 'recommended',
				note: 'Without it the build still succeeds, but the embedded web UI is a placeholder page.'
			},
			{
				id: 'llm-key',
				requirement: 'optional',
				note: 'Any ONE of CLAUDE_CODE_OAUTH_TOKEN, ANTHROPIC_API_KEY or OPENROUTER_API_KEY, checked in that order. None is required to launch; without one tagent prints an onboarding banner rather than failing.'
			}
		],
		steps: [
			{
				title: 'Clone and build it',
				body: 'This is the same cargo install --path form the project’s own signed-install script uses.',
				commands: [
					{
						command:
							'git clone https://github.com/bobmatnyc/trusty-tools\ncd trusty-tools\ncargo install --path crates/trusty-agents --locked',
						label: 'Copy the trusty-agents clone and build'
					}
				],
				notes: [
					'tctl install trusty-agents fails — it is not a stable-set member. Older docs naming an open-mpm repository or crate are wrong; neither has ever existed.'
				]
			},
			{
				title: 'Supply one credential, or none',
				body: 'Each credential resolves from the environment, then a project or user .env.local, then the store tagent config keys set writes to. A run against Bedrock or a local Ollama model needs none of the three.',
				commands: [],
				notes: []
			},
			{
				title: 'Register it as an MCP server',
				body: 'tagent exposes itself to other MCP clients over stdio.',
				commands: [
					{
						command: mcpEntry('trusty-agents', 'tagent', ['mcp-serve']),
						label: 'Copy the trusty-agents MCP entry'
					}
				],
				notes: []
			}
		],
		tcc: {
			category: 'app-data',
			summary:
				'tagent needs the App Data category, the same narrow one tm needs and nothing wider. It reads $HOME and project .trusty-agents/ state, and project-local .claude/ directories that live under another application’s data category. Because it is installed outside tctl, its signing identity is not maintained for you — run scripts/install-trusty-agents-signed.sh from the clone if you want the grant to survive a rebuild.'
		}
	},
	{
		id: 'tga',
		label: 'tga',
		binary: 'tga',
		tagline: 'Git analytics',
		lede: 'A pure CLI over git history, and the only product whose published version matches this repository exactly. It is an optional stable-set member and a leaf: tctl installs it alone.',
		prerequisites: [
			{ id: 'tctl', requirement: 'recommended', note: 'The install path this page recommends.' },
			{
				id: 'git',
				requirement: 'required',
				note: 'It reads repository history through git2. SQLite is bundled — nothing to install.'
			}
		],
		steps: [
			{
				title: 'Install it',
				body: 'The package is named tga even though its directory is crates/trusty-git-analytics.',
				commands: [
					{ command: 'tctl install tga', label: 'Copy the tga tctl install' },
					{ command: 'cargo install tga --locked', label: 'Copy the tga cargo install' }
				],
				notes: [
					'Do not copy a built binary onto your PATH by hand. tga’s own user guide still shows a cp and an mv into /usr/local/bin; on macOS that leaves a stale kernel signature cache and the next run is killed in a way that looks like an out-of-memory kill. cargo install renames atomically instead.'
				]
			},
			{
				title: 'Point it at a config file',
				body: 'One global flag, -c or --config, on every subcommand. It defaults to config.yaml resolved against your current directory.',
				commands: [],
				notes: [
					'There is no ~/.config/tga/ fallback and no environment-variable override, whatever INSTALL-CONVENTION.md says.',
					'tga has no MCP transport at all — nothing to register, by design.'
				]
			}
		],
		tcc: {
			category: 'none',
			summary:
				'Nothing to grant. tga reads local git history and nothing else, which has never raised a prompt.'
		}
	}
];

/** Prerequisites any audience references, in `PREREQUISITES` order. */
export function usedPrerequisites(): Prerequisite[] {
	const used = new Set(AUDIENCES.flatMap((a) => a.prerequisites.map((p) => p.id)));
	return PREREQUISITES.filter((p) => used.has(p.id));
}

/** Look one up by id. Throws rather than rendering an empty card. */
export function prerequisite(id: PrerequisiteId): Prerequisite {
	const found = PREREQUISITES.find((p) => p.id === id);
	if (!found) throw new Error(`unknown prerequisite: ${id}`);
	return found;
}

/**
 * Every string an audience renders, joined. The TCC assertions read this, so
 * a category named anywhere in an audience's copy is a category the test sees.
 */
export function audienceText(audience: Audience): string {
	return [
		audience.label,
		audience.binary,
		audience.tagline,
		audience.lede,
		...audience.prerequisites.map((p) => p.note),
		...audience.steps.flatMap((s) => [
			s.title,
			s.body,
			...s.commands.flatMap((c) => [c.command, c.label, c.placeholderNote ?? '']),
			...s.notes
		]),
		audience.tcc.summary
	].join('\n');
}
