---
name: web-search
description: Real-time web search via Tavily/Brave Search
tags: [search, web, tavily, brave, izzie]
agents: [izzie, personal-assistant]
---

# Web Search — Live Web Lookups

Real-time access to the public web. Use whenever Masa asks about current
events, recent news, facts you don't have stored, or anything that may have
changed since training cutoff.

## Backends

- **Primary: Tavily** — AI-summarized web search with citations. Returns
  pre-digested answers plus source URLs. Requires `TAVILY_API_KEY` env var
  (sign up at app.tavily.com → API Keys → free tier available).
- **Fallback: Brave Search** — raw web results (title, URL, snippet) when
  Tavily isn't configured or returns nothing useful. Requires
  `BRAVE_SEARCH_API_KEY` (register at api.search.brave.com).

Either key works on its own. Having both enables automatic fallback when
Tavily quota is exhausted.

## Tool

- **`web_search(query)`** — issue a search. Tavily is tried first; on
  empty/error result, Brave is queried automatically. Returns a result set
  with summarized answer (Tavily) or ranked snippets (Brave) plus citation URLs.

## When to use

- Questions about current events, sports scores, market data, news
- Looking up a fact you can't recall and shouldn't fabricate
- Researching a topic, person, company, or product
- Verifying a claim Masa is uncertain about
- Finding documentation URLs for libraries / APIs

## When NOT to use

- Personal data (calendar, email, notes) — use the gworkspace / granola /
  trusty-search tools instead
- Internal Duetto info — that lives in cto-* skills and the org database, not
  on the public web
- Anything Masa just told you in this conversation

## Response style

- Lead with the answer, then a one-line "per <source>" attribution
- Don't dump every result — synthesize 1–3 sentences with the citation URL
- For ambiguous queries (e.g. multiple people with same name), confirm which
  one Masa means before deep-diving
- Quote sparingly; paraphrase

## Privacy

- Queries and URLs leave the device (sent to Tavily / Brave).
- **No personal data** is included — never ship email content, calendar
  events, file contents, or Slack messages out as part of a query.
- If Masa asks you to "search my email" or similar, route to gworkspace
  Gmail tools, NOT web search.

## Anti-patterns

- ❌ Fabricating current-event facts instead of searching
- ❌ Searching for personal data that lives in private tools
- ❌ Returning a raw list of 10 URLs — synthesize
- ❌ Quoting an entire article — summarize with one citation link
