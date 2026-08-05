---
name: crypto-price
description: Look up the current spot price of a cryptocurrency via a keyless CoinGecko endpoint, with an offline fallback.
tags: [crypto, price, coingecko, plugins-python, demo]
agents: [demo-assistant]
---

# Crypto Price — bundled `[[plugins.python]]` tool

This is a **demo "enhanced skill"** (#446, epic #3052): a skill package that
bundles domain instructions (this file) **with a co-located Python tool**
(`crypto_price.py`). The tool is exposed to the agent through the
`[[plugins.python]]` plugin model declared in the agent's `agent.toml` — the
agent itself stays declarative (instructions + bindings); the Python script is
a *plugin it references*, not agent code.

## The bundled tool: `crypto_price`

`crypto_price.py` speaks the harness NDJSON tool contract — it reads one
`{"type":"tool_call",...}` line on stdin and writes one
`{"type":"tool_result",...}` line on stdout. It fetches a spot price from
CoinGecko's **keyless** `simple/price` endpoint and falls back to a
deterministic offline price table when the network is unavailable, so a flaky
demo network never breaks the run.

### Parameters

- `coin` (string, required) — a CoinGecko coin id, e.g. `bitcoin`,
  `ethereum`, `solana`, `dogecoin`, `cardano`.
- `vs_currency` (string, optional) — fiat code, default `usd`.

### When to use it

Call `crypto_price` whenever the user asks for the current price / value of a
cryptocurrency:

- "What's Bitcoin trading at?" / "price of ETH"
- "How much is a Solana right now?"
- "Give me the current Dogecoin price in USD."

Pass the CoinGecko coin id as `coin` (map common tickers: BTC → `bitcoin`,
ETH → `ethereum`, SOL → `solana`, DOGE → `dogecoin`, ADA → `cardano`). Report
the returned figure verbatim, including whether it was a live quote or the
offline fallback.

## How it is wired (reference)

`.trusty-agents/agents/demo-assistant/agent.toml` declares:

```toml
[[plugins.python]]
name = "crypto_price"
description = "Look up the current spot price of a cryptocurrency."
script = "../skills/crypto-price/crypto_price.py"   # relative to the agents dir
timeout_secs = 12
```

and lists `crypto_price` in `[tools].allow`. At chat time,
`run_pm_task_with_persona` resolves the script relative to the agent/skill
package dir, builds a `PythonToolPlugin`, and registers it as a callable tool.
