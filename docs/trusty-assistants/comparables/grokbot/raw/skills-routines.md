# Skills and routines

> Source: https://docs.x.ai/grok-bot/skills-routines-and-automations  
> Cross-checked: https://cursor.com/docs/grok-bot/work  
> Fetched: 2026-09-05

---

# Skills and routines

Turn a successful task into a repeatable process. Grok Bot uses two building blocks:

- A **skill** is a reusable set of instructions for **how** to do a task.
- A **routine** tells one Bot **when** to run a workflow—on a schedule or, where supported, after an event.

Start with a one-time task. Make it reliable, save the method as a skill, and only then automate it.

## Save a skill

A skill captures steps, decision rules, expected output, and safety boundaries.
Skills are available across your Bots, although a Bot may need the relevant
connector or login to use one.

A useful skill states:

1. When to use it
2. Required inputs and access
3. The sequence of work
4. How to validate the result
5. What to return
6. What requires approval

Type `/` in the desktop composer to reference a saved skill; use `@` for Bots,
groups, routines, and connectors. Installed private skills can be enabled per Bot
under Settings → Plugins → Yours.

## Teach a workflow by demonstration

When **Teach a task** is available:

1. Open a one-to-one Bot conversation and its computer view.
2. Choose Teach a task.
3. Describe the result you are about to demonstrate.
4. Perform the workflow once.
5. Stop the recording and review the skill the Bot creates.
6. Test it on a safe example before scheduling it.

Teaching records visible computer interaction for **up to ten minutes**. It does
not record microphone audio. Avoid exposing secrets during the demonstration.

The learned skill is a draft — add decision rules, failure handling, and
approval boundaries. Teach-by-demonstration may be enabled gradually.

## Create a routine

Ask the Bot that should own the recurring job. Confirm:

- The owning Bot
- The schedule and time zone
- The input source
- The expected result
- The approval boundary
- What should happen when a source is missing

Background routines can run while your laptop is closed.

## Trigger work from an event

**Cursor account integrations** can start a routine from an event, such as a Slack
message or a GitHub notification. They are **separate from Slack or GitHub plugins**
and may require their own connection flow.

Define a narrow matching rule. Avoid broad listeners such as "every new message."

## Test before enabling

Use **Test run** after creating or editing a routine. A test run performs real
work. Use safe inputs and keep write actions behind approval.

## Manage routines

Open the Bot → View conversation details → Routines:

- Enable or pause
- Run a test
- Edit schedule or instructions
- Inspect recent success/failure history
- Delete (immediate, no undo)

**Limits:** A Bot can own up to **50 routines**; the app keeps the **20 most recent
run records** for each routine. Deleting a Bot also removes its routines.

To control unattended usage, Grok Bot may ask whether to keep routines running
after a long period away and pause them if there is no response.

## Design routines for trust

- Automate preparation before execution.
- Have the Bot draft, reconcile, or recommend first.
- Require approval for sending, purchasing, deleting, publishing, or changing production systems.
- Include a no-data and stale-data policy.
- Make retries idempotent where possible.
- Tell the Bot where to report partial completion.
- Re-test after a website, connector, or source format changes.
