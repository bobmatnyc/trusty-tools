# Grok Bot — Overview (docs.x.ai)

> Source: https://docs.x.ai/grok-bot/overview  
> Fetched: 2026-09-05  
> Type: official product docs (SpaceXAI)

---

# Grok Bot

Bots are AI teammates you can give real work to. Bots can sign and use apps and
websites just like you do on a persistent cloud computer. They can collaborate
independently, passing context between each other and handing off tasks, and
understand the nuances of how you like work done over time. They finish jobs
end to end, and only come back when something needs your approval.

You work with a Bot by messaging it like a teammate. Give it a task or function,
relevant context, and access to any tools or files needed to do the work. Bots
take on ambitious projects, work across multiple tools and systems, and keep you
updated of their actions in the conversation.

> In the Docs and in the Grok Bot app, a Bot = a single persistent, named agent
> or one AI teammate.

## What makes Grok Bot different

- **It has a computer of its own.** Each Bot runs on a persistent cloud VM with
  a browser, filesystem, and terminal. It can use connectors/MCP where
  available, and computer use for apps and websites without a clean API, so work
  finishes in the real tools rather than as chat drafts.
- **It is very easy to get started.** Create a Bot, message it, and grant access
  as needed. No workflow builder or prior Bot setup required. The same Bot is
  reachable from the desktop and mobile apps.
- **It coordinates independently with other Bots.** Multiple Bots share one
  user-scoped computer and can run in parallel. They can message each other,
  share context in threads or group chats, and pass ownership so you are not the
  router between tools.
- **It can learn workflows from live demonstration.** Ask a Bot to follow along
  once through a multi-step or multi-system path. It persists that path as a
  routine and can re-run it on a schedule or on demand.
- **It is a persistent, named teammate with a durable state.** Named Bots keep
  memory, files, browser sessions, and preferences across turns. Context
  compounds instead of resetting to a fresh environment on every task.

## Bots share one computer

All of your Bots use the same persistent cloud computer. They share files,
browser sessions, and app logins, which makes handoffs possible without
repeating setup.

The computer is isolated to your account, not to an individual Bot. Treat a
login or file placed on the computer as available to all of your Bots. Each Bot
gets its own screen on that computer, so several Bots can use browser and
desktop tools in parallel without getting separate security boundaries.

## A good first handoff

Start with a real task that involves multiple tools and has a clear result:

> Pull this week's Strategic Prospects PG List from Salesforce. Skip anyone
> already in a sequence. Research the top 5 accounts across the web, Slack,
> Databricks, and Sumble, pull contacts, and draft LinkedIn and email in my
> voice, and leave me drafts to approve by tomorrow morning

This tells the Bot what to do, where to work, what context to pull in, what
you'd like to see as a finished deliverable.

You can review a Bot's work, provide corrections, and turn the process into a
reusable skill or proactive routine.

## What to explore next

- Get started with the desktop app and your first Bot.
- Discover more use cases for Bots.
- Make recurring work repeatable with skills and routines.
- Learn how to use the computer and apps.
- Review approvals, security, and privacy before granting access to sensitive systems.

## For team admins and security reviewers

- Grok Bot for teams and enterprises
- Grok Bot security and Grok Bot security FAQ
- Configure identity and access (Okta / Entra ID)
- Connect to private networks (Tailscale / Cloudflare Tunnel via Team Setup)
