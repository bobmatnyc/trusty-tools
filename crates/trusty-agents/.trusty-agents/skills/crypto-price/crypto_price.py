#!/usr/bin/env python3
"""crypto_price.py — bundled ``[[plugins.python]]`` tool for the demo-assistant (#446).

Why: This is the reference "enhanced skill" tool for the Friday 2026-07-25 demo.
It proves the ``[[plugins.python]]`` plugin model end to end: a user-authored
Python script that BUNDLES WITH its skill package (this file sits next to
``SKILL.md``) and is referenced from an agent's ``agent.toml`` — no changes to
trusty-agents core.

What: Speaks the harness NDJSON tool contract (see
``crates/trusty-agents/src/plugins/python_tool/mod.rs``):

  stdin  : exactly one line  {"type":"tool_call","id":<id>,"params":{...}}
  stdout : exactly one line  {"type":"tool_result","id":<id>,"status":"success","content":<str>}

It fetches a spot crypto price from CoinGecko's KEYLESS simple-price endpoint,
with a DETERMINISTIC OFFLINE FALLBACK so flaky venue Wi-Fi never breaks the
demo. Every failure path is reported AS a valid ``tool_result`` — the script
never crashes and always exits 0.

Test: Drive it directly —
  echo '{"type":"tool_call","id":"1","params":{"coin":"bitcoin"}}' | python3 crypto_price.py
"""

import json
import sys
import urllib.error
import urllib.request

COINGECKO = "https://api.coingecko.com/api/v3/simple/price"

# Deterministic offline fallback prices (USD). Used only when the live fetch
# fails, so the demo is resilient to venue Wi-Fi. Always clearly labelled as a
# fallback in the returned content so it is never mistaken for a live quote.
FALLBACK = {
    "bitcoin": 64000.0,
    "ethereum": 3400.0,
    "solana": 145.0,
    "dogecoin": 0.12,
    "cardano": 0.45,
}


def fetch_live(coin, vs):
    """Fetch one spot price from CoinGecko. Raises on any failure."""
    url = f"{COINGECKO}?ids={coin}&vs_currencies={vs}"
    req = urllib.request.Request(url, headers={"User-Agent": "trusty-agents-demo/1.0"})
    with urllib.request.urlopen(req, timeout=8) as resp:
        payload = json.loads(resp.read().decode("utf-8"))
    price = payload.get(coin, {}).get(vs)
    if price is None:
        raise ValueError(f"no price for {coin}/{vs} in CoinGecko response")
    return float(price)


def emit(call_id, status, content="", error=None):
    """Write exactly one NDJSON tool_result line and flush."""
    out = {"type": "tool_result", "id": call_id, "status": status}
    if status == "error":
        out["error"] = error or content
        out["content"] = error or content
    else:
        out["content"] = content
    sys.stdout.write(json.dumps(out) + "\n")
    sys.stdout.flush()


def main():
    raw = sys.stdin.readline()
    try:
        call = json.loads(raw) if raw.strip() else {}
    except json.JSONDecodeError as e:
        emit(None, "error", error=f"invalid tool_call JSON: {e}")
        return

    call_id = call.get("id")
    params = call.get("params", {}) or {}
    coin = str(params.get("coin", "bitcoin")).lower().strip()
    vs = str(params.get("vs_currency", "usd")).lower().strip()

    try:
        price = fetch_live(coin, vs)
        source = "live (CoinGecko)"
    except Exception as e:  # noqa: BLE001 — any failure degrades to fallback/error
        if coin in FALLBACK and vs == "usd":
            price = FALLBACK[coin]
            source = f"offline fallback (live fetch failed: {type(e).__name__})"
        else:
            emit(call_id, "error", error=f"could not fetch {coin}/{vs}: {e}")
            return

    content = f"{coin} = {price:,.4f} {vs.upper()} [{source}]"
    emit(call_id, "success", content=content)


if __name__ == "__main__":
    main()
