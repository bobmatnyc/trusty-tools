# Forum — Introducing Grok Bot (staff launch post) + routine queue signal

> Sources:  
> - https://forum.cursor.com/t/introducing-grok-bot/168053 (Kevin Neilson / CursorStaff, 2026-08-11)  
> - https://forum.cursor.com/t/grok-bot-routines-dont-auto-run-on-schedule/170358 (staff reply Colin, 2026-09-02)  
> Fetched: 2026-09-05  
> Type: public community forum (staff announcements / support)

---

## Introducing Grok Bot (staff announcement summary)

**Author:** Kevin Neilson (CursorStaff / moderator)  
**Date:** 2026-08-11  

Today we're launching **Grok Bot** in early beta: AI teammates you can give real work to.

Bots sign in to the tools you already use and work in them just like you do, on a
persistent cloud computer with a browser, filesystem, and terminal. They finish
jobs end to end and only come back when something needs your approval.

You work with a Bot like you would a teammate. Give it a task, shut your computer,
and pick the thread back up from desktop or iOS.

### What's new

1. **A computer of its own** — persistent cloud computer that keeps files and
   logins; connectors and MCP where available; computer use everywhere else;
   work lands in the real tool, not a draft.
2. **Bots that coordinate with each other** — share one computer; each gets its
   own screen; parallel work; message each other; threads and group chats; pass
   ownership.
3. **Show a Bot how it's done** — follow along once through a multi-step path;
   saves as a routine; re-runs on demand or schedule.

**Shared-computer warning (staff):** The computer is isolated to your account
rather than to an individual Bot, so treat a login or file placed on it as
available to all of your Bots. See Approvals, security, and privacy docs.

**Launch eligibility (as stated in post):** beta on desktop and iOS for
**SuperGrok Heavy**, **Cursor Ultra**, and **Cursor Teams Premium**. Enterprise
waitlist. (Later expanded — see news-more-plans.md.)

Links: Blog (x.ai/news/introducing-grok-bot), Docs overview, Get started.

---

## Community signal: routine queue lag (label as community/staff-confirmed ops issue)

**Thread:** Grok Bot routines don't auto-run on schedule  
**Staff (Colin), 2026-09-02:** Routines did fire at every scheduled slot, but each
run sat in a queue before it started, so it began **10 to 37 minutes** after the
set time. "Next run" reading "Run now" is that queued state. Separate known issue:
some runs finish without posting a chat message.

**Community follow-up (2026-09-04):** Reporter confirms queue lag still present on
Grok Bot 0.39.0; irregular gaps on short `@every` probes.

**Research use:** Cite as a public reliability signal for scheduled automation —
not as a documented SLA. Label clearly as forum/staff operational report.
