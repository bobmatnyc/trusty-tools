<!-- The block between BEGIN/END GENERATED is rebuilt from GitHub milestones by scripts/roadmap/generate.mjs. Everything outside it is hand-written and kept. -->

# trusty-mpm roadmap

trusty-mpm runs your coding sessions and keeps them going. The next two
releases make that dependable. The release after them, 2.0.0, adds a
supervisor that looks after all of your projects for you.

## What 2.0.0 brings

**A supervisor you create with one request.** Ask for a supervisor and
trusty-mpm sets one up: a small project of its own, kept as a local git
repository, watching the projects you choose. You can add or remove a
project later. It runs on the latest Opus model.

**Instructions made for supervising.** A normal trusty-mpm session is a
project manager that hands coding work to specialist agents. A supervisor
has a different job: it watches sessions, acts directly, and reports what it
saw and what it checked. It gets its own instructions and its own writing
style, so it carries none of the project-manager rules it never uses.

**Chat channels for your projects, starting with Slack.** A project can be
given a channel. When the supervisor needs your decision, it sends you the
question there, and your reply goes straight back to it. You no longer have
to be at the terminal to unblock it.

**Careful about who can talk to it.** Messages follow a routing table that
refuses anything not explicitly allowed. At first the only allowed sender and
recipient is the owner. An incoming message can answer a question the
supervisor asked, and nothing else: it cannot start work or run commands. If
two answers arrive, the first one counts.

**One supervisor for everything.** A single supervisor serves every project,
so the same question never reaches you twice.

## How we get there

1.7.1 finishes the reliability work already under way. 1.7.2 is bug fixes
only. 2.0.0 follows.

<!-- BEGIN GENERATED: roadmap -->

## Next

### 1.7.2

trusty-mpm 1.7.2 — bug fixes after 1.7.1. The release line is 1.7.1 (in flight) → 1.7.2 (bug fixes) → 2.0.0 (supervisor and channels platform).

0 of 45 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/96)

### 2.0.0

trusty-mpm 2.0.0 adds a supervisor: one session that watches all of your projects, handles what it can, and asks you about the rest. You create it with a single request, and it comes with its own instructions and writing style, made for watching and reporting instead of for running coding work. Each project can be given its own chat channel, starting with Slack, so the supervisor can send you a question and take your answer there. Incoming messages go through a routing table that refuses anything not explicitly allowed, and a message can only answer a question the supervisor actually asked. One supervisor serves every project, so the same question never reaches you twice.

0 of 8 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/97)

<!-- END GENERATED: roadmap -->
