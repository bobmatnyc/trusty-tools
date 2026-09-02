# Running the audit — {{ENGAGEMENT}}

Everything you need is in the folder above this one. Nothing is installed into
your system directories, and nothing here needs a Rust toolchain.

{{PLATFORM_LINE}}
- Configuration: `../{{CONFIG}}` — plain TOML, readable. Open it before you run
  anything. `engagement.template.toml`, beside this file, documents every key it
  accepts.
- Working directory: `{{WORK}}`, created on first run.

The working directory holds the clones, the collected database, the tooling and
the reports. It is NOT inside the extracted folder: the code analysis cannot
read a checkout under `~/Downloads`, `~/Desktop`, `~/Documents` or a temporary
directory, which is where an emailed zip usually lands. Pass
`--work-dir <path>` to any command below to put it somewhere else.

## 1. Extract the folder

```sh
unzip trusty-audit-install.zip
cd trusty-audit
```

On macOS a zip that arrived over the network is quarantined, and this build is
not yet notarized, so the first run reports "cannot be opened because the
developer cannot be verified". Clear the flag once, from inside the extracted
folder:

```sh
xattr -dr com.apple.quarantine .
```

## 2. Install the pinned tools

```sh
./{{LAUNCHER}} install
```

This downloads the four tools this engagement pins — `tga`, `trusty-search`,
`trusty-analyze`, `trusty-review` — verifies each against its recorded version
and digest, and puts them inside the working directory. It needs no OpenRouter
key. A version or digest that does not match refuses the whole set rather than
installing part of it.

It needs outbound HTTPS to `github.com` and `objects.githubusercontent.com`. A
network that blocks binary downloads is the one failure that has no workaround
inside this package — tell your auditor and they will send the binaries.

## 3. Choose what to audit

{{TARGETS_STEP}}

Repository access uses **your own GitHub credential**, not the auditor's. If
you have never authenticated `gh` on this machine, do it first — this is your
credential and it never leaves your machine:

```sh
gh auth login
```

`./{{LAUNCHER}} targets` prints what this engagement is registered to audit.
Check the count before you start: `17 repositories, 2 boards` catches a
truncated list at a glance.

## 4. Run the audit

```sh
./{{LAUNCHER}} audit
```

One command, start to finish. It clones each registered repository, indexes the
checkout in `trusty-search`, measures it with `trusty-analyze`, sweeps the git
history with `tga`, renders the report, and assembles the package to send back.

{{KEY_STEP}}

Once it starts it asks you nothing else. It may run for hours and prints
progress as it goes. A repository that fails does not stop the rest — the run
names every repository it could not cover and exits non-zero so a partial sweep
never reads as a complete one. Interrupt it and run the same command again:
installed tools, finished clones and audited repositories are all carried over.

**macOS: Full Disk Access.** `trusty-search` indexes your checkouts, and macOS
will ask for Full Disk Access the first time it reads one outside your home
directory — or refuse silently if you are not asked. Grant it in **System
Settings → Privacy & Security → Full Disk Access**, click **+**, and add the
`trusty-search` binary from `{{WORK}}/tools/`. Without it the report renders
with the code-analysis sections empty.

## 5. Read the report

The reports land under `{{WORK}}/out/`, one directory per repository, as a
markdown file and a JSON twin carrying the same data. Open the markdown.

## 6. Send the results back

```sh
./{{LAUNCHER}} package
```

This writes one zip and prints its path on the last line. Send that file.

Open it before you send it. It is unencrypted and has no password on purpose,
so you can read exactly what leaves your network. It carries each repository's
report and the git-metadata database those reports were computed from. It never
carries the OpenRouter key: every member's bytes are scanned for it while the
zip is written, and a match refuses the whole package.

The metadata database holds no file content, diffs, patches, hunks or blobs. It
does hold free-text fields — commit messages, pull-request and work-item titles
— so a code snippet someone pasted into a commit message is in it.

## 7. Remove it afterwards

```sh
rm -rf {{WORK}}
trusty-search index remove <each cloned repository path>
```

Then delete the extracted folder. The second command matters: to analyse your
code the client approves each clone with `trusty-search`, and that approval is
recorded in `trusty-search`'s own settings, outside the working directory. The
run prints each path it approves, and `trusty-search index list` shows what is
still approved.

## If something fails

Every command names the phase it failed in — install, materialize, collect or
package — so the message says which step to look at. Then:

| What to read | Where |
|---|---|
| Output from the tools this client runs | `{{WORK}}/logs/` |
| What the sweep actually covered | `{{WORK}}/out/manifest.toml` |
| Where the run stopped, for a resume | `{{WORK}}/state/run-progress.toml` |
| What is registered to be audited | `./{{LAUNCHER}} targets` |
| Which tools are installed, at which versions | `./{{LAUNCHER}} tools` |

Add `RUST_LOG=debug` before any command for more detail on stderr:

```sh
RUST_LOG=debug ./{{LAUNCHER}} audit
```

Four failures have a specific answer:

- **"cannot be opened because the developer cannot be verified"** — the
  quarantine flag. Step 1.
- **The report's code-analysis sections are empty** — Full Disk Access. Step 4.
- **A repository is refused when you register it** — `gh auth login`, or the
  account you authenticated cannot read that repository. Step 3.
- **A tool download is refused** — the version or digest did not match, or the
  network blocked it. Nothing in the package works around this; send your
  auditor the message.

Anything else: send your auditor the last twenty lines of output and the
contents of `{{WORK}}/logs/`. Neither carries your OpenRouter key — it is
redacted everywhere it could otherwise be printed.
