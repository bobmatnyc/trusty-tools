<script lang="ts">
	/**
	 * Why: the reader of this page is an operator at a client company who was
	 * sent the URL and has to run the engagement today. Every other flagship
	 * page pitches a tool to someone choosing one; this one is instructions.
	 * Hence the numbered flow, the prerequisites stated before step 1, and the
	 * troubleshooting section carrying the installer's real refusals.
	 *
	 * What: the four steps `crates/trusty-audit` actually implements — install,
	 * first run, register targets, audit — then what comes back, then the
	 * failure modes.
	 *
	 * Sourcing: verbs and flags from `src/cli.rs`; the pinned four-tool set from
	 * `src/tools.rs` (`RequiredTool::ALL`) and `src/config.rs` (`ToolPins`); the
	 * credential precedence and prompt from `src/cli/credential.rs` (#5872);
	 * which commands actually need that credential from `src/session.rs`
	 * (`Command::credential_need` — only `run` and `audit` are `Required`; a
	 * bare/`guided` invocation is `None`, so the first-run status card never
	 * prompts); the working-directory layout and defaults from `src/workdir.rs`;
	 * the return package from `src/package.rs`; the refusals below from
	 * `install.sh` (#5873). The crate README is a draft here, not a source — it
	 * still names three pinned tools where the code pins four, and its "bare
	 * invocation … does not launch an unattended sweep" line is what the shipped
	 * v0.1.0 binary does; PR #5896 (open, unmerged as of this writing) changes
	 * that to a one-step guided registration-then-launch flow — do not document
	 * it until it ships.
	 *
	 * Verified directly against the built v0.1.0 binary in a scratch directory:
	 * `trusty-audit --help`; a bare invocation with no `engagement.toml`, which
	 * prints the status card below and exits 0; `install`/`audit` with no
	 * `engagement.toml`, which both refuse; `add repo`, `targets`, `remove`,
	 * `discover` against a live `gh` credential; and the real
	 * `curl -fsSL … install.sh | sh` command with `TRUSTY_AUDIT_INSTALL_DIR` and
	 * `TRUSTY_AUDIT_NO_LAUNCH=1`, which resolved latest as 0.1.0, verified its
	 * sha256, and installed both `trusty-audit` and `taudit`.
	 */
	import CopyButton from '$lib/components/CopyButton.svelte';
	import ToolPage from '$lib/components/ToolPage.svelte';
	import { installCommand, TOOLS } from '$lib/tools';

	const tool = TOOLS.find((t) => t.slug === 'trusty-audit')!;

	const facts = [
		{ label: 'Package', value: 'trusty-audit' },
		{ label: 'Runs on', value: 'macOS, Apple Silicon' },
		{ label: 'Needs', value: 'gh, authenticated' },
		{ label: 'Returns', value: 'audit-return-package.zip' }
	];
</script>

<ToolPage {tool} {facts}>
	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">What it is</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			An auditor has been engaged to assess your codebase and how your team works in it. Rather than
			ship your source to them, they send you this: a client-side collector that runs on your
			machine, inside your network, against repositories you name. It installs the exact audit
			tooling the engagement pins, runs it, and writes one zip file. You look inside that file, then
			send it back.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Nothing here is a background service. It runs when you run it, writes everything under a
			single working directory, and stops. <code class="text-sm">rm -rf</code> on that directory removes
			everything it wrote.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">Before you start</h2>
		<ul class="mt-6 max-w-3xl space-y-3 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">A Mac with Apple Silicon.</span> The installer
					refuses an Intel Mac and refuses a non-macOS host, rather than downloading a binary that cannot
					execute. There is no Windows or Linux build.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text"
						>The <code class="text-sm">engagement.toml</code> your auditor sent you.</span
					>
					It pins the tool versions this engagement runs and carries its instructions. Put it in the directory
					you will run from, or point at it with
					<code class="text-sm">--config &lt;FILE&gt;</code>. Without it there is nothing to install
					and the run refuses.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text"
						>GitHub, through <code class="text-sm">gh</code>.</span
					>
					Repository access uses your own GitHub login: run
					<code class="text-sm">gh auth login</code> first. Every repository read goes through
					<code class="text-sm">gh</code>, so whatever your credential can see is what the audit can
					see, and nothing more.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Board tokens, if boards are in scope.</span
					>
					JIRA needs a site URL, an account email, and an API token; Linear needs a personal API key.
					Both live in <code class="text-sm">engagement.toml</code> under
					<code class="text-sm">[boards.jira]</code>
					and <code class="text-sm">[boards.linear]</code>. Skip this if the engagement covers code
					only.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Outbound access to github.com.</span> Both the
					installer and the pinned tools download release assets from there. A proxy that blocks binary
					downloads fails in the first thirty seconds, naming the URL it could not reach.</span
				>
			</li>
		</ul>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">1 · Install</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			One command. It detects your platform, resolves the latest release, downloads the tarball and
			its published SHA-256 sidecar, refuses to continue if the two disagree, checks the binary
			actually runs, installs it with an atomic rename, and then launches it.
		</p>
		<!-- `min-w-0`: a `<pre>` never wraps, so without it the flex child widens
		     to the longest command and the page scrolls sideways at 375px.
		     `pr-14` on the `<pre>` keeps the copy button clear of the text; the
		     button sits in the wrapper's own padding, not over the command. -->
		<div class="relative mt-6 max-w-3xl min-w-0">
			<pre
				class="overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-4 pr-14 text-xs leading-relaxed text-foundry-text">{installCommand(
					tool
				)}</pre>
			<div class="absolute right-2 top-2">
				<CopyButton text={installCommand(tool)} label="Copy install command" />
			</div>
		</div>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			It installs into <code class="text-sm">$&#123;CARGO_HOME:-$HOME/.cargo&#125;/bin</code> and
			never uses <code class="text-sm">sudo</code>. If that directory is not on your
			<code class="text-sm">PATH</code>, the installer says so and prints the line to add.
			Re-running the command upgrades in place.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Three environment variables change what it does:
			<code class="text-sm">TRUSTY_AUDIT_VERSION</code> pins an exact version instead of taking the
			latest, <code class="text-sm">TRUSTY_AUDIT_INSTALL_DIR</code> chooses a different destination,
			and <code class="text-sm">TRUSTY_AUDIT_NO_LAUNCH=1</code> installs without starting the binary.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">2 · First run</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Whether the installer launched it for you or you type <code class="text-sm">trusty-audit</code
			> by itself, a bare invocation is a status check, not a sweep. It reports the working
			directory it will use, that no audit has run there yet, that none of the four pinned tools are
			installed, and reminds you to register what to audit. It asks for nothing and downloads
			nothing yet — that starts with the next two steps.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			It does not need <code class="text-sm">engagement.toml</code> to show you this — only installing
			tools and running the audit do. Its reminder to register targets does not go away once you have
			registered some; check <code class="text-sm">trusty-audit targets</code> for the registry itself,
			not this status line.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">3 · Register what to audit</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Name each target once. Registration is additive, so you build the set up over several
			commands, and registering the same thing twice changes nothing.
		</p>
		<div class="mt-6 max-w-3xl min-w-0">
			<pre
				class="overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-4 text-xs leading-relaxed text-foundry-text">trusty-audit add repo acme/api
trusty-audit add board jira:ACME
trusty-audit add board linear:ENG
trusty-audit targets</pre>
		</div>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Each <code class="text-sm">add</code> reaches the target with the same credential the audit
			will use — your
			<code class="text-sm">gh</code> login for a repository, the configured board token for a board —
			and refuses one it cannot read. That is deliberate: a target recorded without ever being reached
			becomes a gap discovered an hour into an unattended run.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			<code class="text-sm">trusty-audit discover</code> lists every repository your GitHub
			credential can see, marking private and archived ones, if you would rather look before you
			choose. <code class="text-sm">trusty-audit remove &lt;target&gt;</code> takes one back out.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">4 · Run the audit</h2>
		<div class="mt-6 max-w-3xl min-w-0">
			<pre
				class="overflow-x-auto rounded-sm border border-foundry-border bg-foundry-card p-4 text-xs leading-relaxed text-foundry-text">trusty-audit audit</pre>
		</div>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			This is the first command that needs your OpenRouter key — installing tools and registering
			targets never touch it. The audit renders its report through a language model, so before the
			sweep can start it resolves a key: an <code class="text-sm">OPENROUTER_API_KEY</code> already
			exported in your shell wins; otherwise a key already in
			<code class="text-sm">engagement.toml</code> — your auditor may have put theirs there — is used;
			otherwise trusty-audit asks at the terminal, with the typing hidden, and asks twice so a mistyped
			key is caught immediately. Either way the run prints which of the three it used. What you type at
			the prompt is written back into <code class="text-sm">engagement.toml</code>, readable only by
			your account, and is not asked for again. The key never reaches a log line, an error message, or
			the package you send back.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			If there is no terminal to ask on — a script, a CI job — it refuses and names both ways to
			supply a key, rather than hanging or reading whatever happened to be on standard input.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			One command chains four phases. It downloads and version-checks the four tools the engagement
			pins — tga, trusty-search, trusty-analyze and trusty-review — clones the repositories you
			registered, sweeps each one, and assembles the return package. Progress prints as it goes,
			phase by phase and repository by repository.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Expect it to take hours on a large set. You can interrupt it and run the same command again:
			installed tools, completed clones, and audited repositories are all carried over rather than
			redone, and each carried-over repository is printed as resumed so a fast re-run does not read
			as a run that did nothing. <code class="text-sm">--fresh</code> discards that record and audits
			everything again, which is the expensive direction and has to be asked for by name.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			One repository failing does not stop the rest. The run continues, names every repository it
			did not cover, and exits non-zero — so a partial engagement can never be mistaken for a whole
			one, by you or by a shell command chained after it. A repository that produces nothing for
			four hours is stopped and recorded as a timeout, so a hang costs one repository instead of the
			whole run.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Everything lands under one directory — <code class="text-sm">./trusty-audit-work</code> by
			default, or wherever <code class="text-sm">--work-dir</code> points. Clones go in
			<code class="text-sm">repos/</code>, the pinned binaries in
			<code class="text-sm">tools/</code>, tool output in <code class="text-sm">logs/</code>, and
			the deliverable in
			<code class="text-sm">out/</code>. Clones are shallow and stop starting new ones past 20 GB on
			disk, so an org-wide audit does not fill the machine.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">What you send back</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The last line of a finished run is the path to one file:
			<code class="text-sm">audit-return-package.zip</code>, inside the working directory unless
			<code class="text-sm">--out</code> named somewhere else. Send that file to your auditor by whatever
			channel you agreed. Nothing is uploaded for you.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			It is unencrypted and has no password, on purpose: open it and read exactly what you are about
			to send. Inside are the report directory for each audited repository, the analysis database
			those reports were computed from, a README describing the contents, and a metadata file naming
			which repositories were covered and at which tool versions.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Two guarantees hold while the zip is written. The OpenRouter key is scanned for across every
			member, and a match refuses the whole package rather than quietly dropping one file. A symlink
			or a hardlink under the collected directories is refused for the same reason — either could
			pull a file from outside the working directory into an archive that leaves your network.
		</p>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			The analysis database holds no file contents, no diffs and no patches. It does hold free text
			— commit messages, pull-request and work-item titles, classification notes — so a snippet
			someone pasted into one of those is in the file.
		</p>
	</div>

	<div>
		<h2 class="font-display text-2xl font-bold sm:text-3xl">When something refuses</h2>
		<p class="mt-4 max-w-3xl text-foundry-secondary">
			Every refusal below leaves nothing installed and nothing half-written. The installer's
			messages name what failed, why, and what to do; these are the ones you are most likely to see.
		</p>
		<ul class="mt-6 max-w-3xl space-y-3 text-foundry-secondary">
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Unsupported operating system.</span> You are
					not on macOS. There is no asset to download; run it on a Mac.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Unsupported macOS architecture.</span> An Intel
					Mac. No x86_64 macOS asset is published for any crate in this workspace, and handing you the
					arm64 one would give you a file that cannot execute.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Checksum mismatch.</span> The download does not
					match its published digest. Retry once; if it mismatches again, do not use the file — report
					it against the repository.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Could not reach the releases API.</span>
					Network or proxy. If you are rate limited, set <code class="text-sm">GITHUB_TOKEN</code>
					and re-run, or pin a version with <code class="text-sm">TRUSTY_AUDIT_VERSION</code> to skip
					the lookup.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Install directory is not writable.</span>
					The installer never uses <code class="text-sm">sudo</code>. Point
					<code class="text-sm">TRUSTY_AUDIT_INSTALL_DIR</code> at a directory you own.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">Installed, but not launched.</span> There was
					no terminal to attach, so it printed the command to run yourself rather than starting something
					that could not prompt you for the key.</span
				>
			</li>
			<li class="flex gap-3">
				<span aria-hidden="true" class="mt-[0.55em] h-1 w-1 shrink-0 bg-foundry-primary"></span>
				<span
					><span class="font-semibold text-foundry-text">A pinned tool will not install.</span> Usually
					an egress proxy blocking binary downloads. The error names the URL. Allow the GitHub release-asset
					host, or ask your auditor for a package built for your network — all four tools install or none
					do, so there is never a half-pinned set.</span
				>
			</li>
		</ul>
	</div>
</ToolPage>
