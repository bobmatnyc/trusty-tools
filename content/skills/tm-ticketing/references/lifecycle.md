# tm-ticketing — lifecycle and attribution

## Lifecycle — open → in-progress → coded → merged → tested → closed

Four mutually exclusive labels carry the middle of an issue's life, between
GitHub's native `open` and `closed`:

| Label | Meaning |
|---|---|
| `status:in-progress` | A session has claimed it and is working it now |
| `status:coded` | Implementation pushed on a branch; PR not yet merged |
| `status:merged` | PR merged to main; live verification pending |
| `status:tested` | Verified live (installed binary, real run); eligible to close |

Advance with `tm issue transition N status:merged`, never a hand-typed label
swap. It reads the project's state model (`issue-state.yaml` at the repo root),
refuses any edge the model does not declare with exit 1 and the list of states
you may move to, and issues the `--add-label` and `--remove-label` as ONE
`gh issue edit`. Two `status:` labels on one issue is a defect the tool makes
unreachable; an issue that already has two is refused with a pointer to
`tm issue repair N`. `tm issue states` lists the model, `tm issue current N`
reads an issue's state back. On a host with no `tm` — and only there — fall back
to `gh issue edit N --add-label status:merged --remove-label status:coded`, both
flags in one call. When `tm issue transition` refuses a target, report the
refusal to the PM or owner; never author or edit a repo `issue-state.yaml`, and
never swap the status label by hand to get past it (#9580). The `ticketing`
agent runs every one of these; the PM never runs `gh issue` or `tm issue`
itself.

**Claim at dispatch.** `status:in-progress` goes on when the work is dispatched,
with a dated comment naming the claiming session ("Claimed by session `<name>`,
`<date>` — fix in flight"). Another session takes a claimed issue only when the
claim is provably stale: the named session is gone AND nothing referencing the
issue — branch push, PR, comment — has moved since the claim. Either alone is
not enough. When in doubt, leave it.

**Filing a NEW issue is not itself a dispatch.** Apply `status:in-progress`
only when the dispatch brief explicitly says a dispatch is starting on this
issue now — a freshly created issue stays plain open unless that same brief
assigns it for immediate work; when in doubt, file open (#7803).

**Advances are event-driven, not swept.** The agent that observed the event owes
the label pass then and there:

| Event | Command |
|---|---|
| PR opened for the fix | `tm issue transition N status:coded` |
| Merge CONFIRMED (`gh pr view <n> --json state` reports `MERGED`) | `tm issue transition N status:merged` |
| Live verification evidence in hand | `tm issue transition N status:tested` |
| Live verification FAILED and a follow-up fix PR is open | `tm issue transition N status:coded` |
| Claim released — session gone, nothing moved | `tm issue transition N open` |

A confirmed merge with no label pass is an incomplete step, not a tidy-up for
later (learned 2026-08-31: auto-merge lands PRs unattended, so nothing is
watching at the moment the state changes). `version-control` reports the
confirmed merge and flags the advance it owes; the PM routes that report to
`ticketing`, which makes the edit.

**The close bar.** An issue closes only from `status:tested`, with the live
verification evidence in the closing comment — what ran against the installed
artifact and what it printed: `tm issue transition N closed --note "<evidence>"`.
The model declares that edge only from `status:tested` and marks it
`requires_note`, so a close without evidence is refused. A merged fix that fails
live verification stays open — at `status:merged` while nobody is working it, and
back at `status:coded` once a follow-up fix PR is open (owner ruling 2026-09-07;
the model declares `status:merged -> status:coded`). A fix PR carries `Refs #N`,
never `Closes #N`, so a merge cannot auto-close something nobody has verified.

**Docs-only exception (owner ruling 2026-09-09).** A docs-only issue may close
directly from `status:merged` on merged-text evidence — a squash-merge SHA
plus confirmation the merged text is present on `origin/main` at the cited
file/symbol — since there is no installed binary or run to point at. First
application: #7190, closed on squash SHA `0024aa7af6a0cbab6fbe12b35ef5c6e26e312c46`
with the text verified present at `destructive_delete.rs` lines 73-74 on
`origin/main`. Every other issue class keeps the `status:tested` +
live-verification bar unchanged.

**Comments along the way.** A progress comment at each meaningful transition —
diagnosis confirmed, fix pushed, review verdict received, blocked — carrying
deliverables and links. Not per-poll spam. Blocked work keeps its `status:`
label and gains a comment naming the blocker, its impact, and the unblock
criteria.

Every delegation in this chain carries the ticket context, so downstream agents
can reference it in their own output. Projects without formal tracking workflows
are not subject to any of this.

### Attribution on Issues and Comments

Every issue body and issue comment ends with one line:

```
🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
```

One line, machine-readable, no preamble around it. That covers issue bodies and
comments only. The commit and PR footer comes from the `attribution` key tm
writes into the provisioned Claude Code settings; never restate it in prose. The
PR body is `version-control`'s to write.
