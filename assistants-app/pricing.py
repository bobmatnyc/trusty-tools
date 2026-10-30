"""Bedrock token pricing for the assistants app.

Why: the smoke test and the chat server must report the same cost per call.
What: Sonnet 5.5 list prices and the per-call cost estimate.
Test: `test_estimate_cost_uses_sonnet_pricing` in
`assistants-app/tests/test_server.py`.
"""

from __future__ import annotations

# USD per million tokens (Sonnet 5.5).
INPUT_PRICE_PER_MTOK = 3.0
OUTPUT_PRICE_PER_MTOK = 15.0


def estimate_cost(input_tokens: int, output_tokens: int) -> float:
    """Returns the estimated USD cost of one call at Sonnet 5.5 pricing."""
    return (
        input_tokens * INPUT_PRICE_PER_MTOK + output_tokens * OUTPUT_PRICE_PER_MTOK
    ) / 1_000_000
