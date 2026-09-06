# Use cases

> Source: https://docs.x.ai/grok-bot/use-cases  
> Fetched: 2026-09-05

---

# Use cases

The best Grok Bot roles own a repeatable outcome, not a loose category of
questions. Start with read-and-prepare work, review the result, then add
approved actions or a routine.

## Sales Outbound

Owns: account research, contact prioritization, and review-ready outreach.

Connect: CRM, product-intent sources, company websites, email, professional networks.

Start with research + draft outreach; return a review list; do not send or enroll.
Then create a nightly research routine that stops at the review list.

## Talent Scout

Owns: sourcing, candidate research, outreach drafts, and scheduling preparation.

Connect: ATS, approved sourcing tools, email, calendar.

Find candidates meeting must-haves; exclude ATS duplicates; draft outreach; do not contact.
Add approvals before external outreach; respect privacy and regional requirements.

## Paid Media

Owns: campaign monitoring and budget recommendations.

Connect: advertising platforms, analytics, budget spreadsheet, Slack.

Pull spend/performance; recommend reallocations; draft Slack update; do not change budgets or send.
Keep campaign changes behind approval even after analysis becomes a routine.

## Expense Manager

Owns: weekly expense reconciliation and missing-information follow-up.

Connect: expense system, email, shared drive, finance spreadsheets.

Build summary; match receipts; flag exceptions; draft follow-ups; do not send or change reimbursements.

## Product Performance

Owns: targeted performance investigations with evidence.

Connect: observability, analytics, incident tooling, source-control links.

Investigate with screenshots and links; separate facts from hypotheses; do not change production.
Use routines for recurring health reports, not unsupervised production changes.

## Bug Reproduction

Owns: turning reports into reliable reproduction packs.

Connect: issue tracker, staging, browser, network tools.

Reproduce in staging with fresh test account; return steps, expected/actual, screenshots, console/network notes.
Do not use production customer data. Provide test credentials via secure handoff.

## Account Health

Owns: risk and expansion signals across a customer portfolio.

Connect: CRM, product usage, support, billing, CS notes.

Ranked watch list with evidence and suggested next steps; do not contact customers or edit CRM.
Define risk thresholds in Bot description.

## Chief of Staff

Owns: a source-linked digest of what changed and what needs attention.

Connect: Slack, email, calendar, meeting notes, planning documents.

Return only priority-mapped items with source, why it matters, proposed next step, decision owed.
Do not send messages or change meetings. Tune by marking useful vs noise; then schedule.

## Turn an example into a durable Bot

1. Put job, source systems, output format, and standing boundaries in the Bot description.
2. Run one real task with a safe scope.
3. Correct until reviewable.
4. Save successful process as a skill.
5. Test on a second input.
6. Create a routine only when retries and failure cases are defined.
7. Keep consequential external actions behind approval.
