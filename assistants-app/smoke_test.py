"""Bedrock smoke test for the tm-agents assistants app (Slice 1.1).

Why: proves AWS credentials, region, and model access work before the
prototype server is built on top of `converse_stream`.
What: streams one reply from Bedrock to stdout, then prints token usage and
the estimated cost at Sonnet 5.5 pricing.
Test: run `python assistants-app/smoke_test.py`; exit 0 means success.
"""

from __future__ import annotations

import os
import sys

import boto3
from botocore.exceptions import BotoCoreError, ClientError

from pricing import estimate_cost

# Sonnet 5.5 has no on-demand throughput under its bare model ID
# (`anthropic.claude-sonnet-5-5`); Bedrock requires the US inference profile.
MODEL_ID = os.environ.get("BEDROCK_MODEL_ID", "us.anthropic.claude-sonnet-5-5")
PROFILE = os.environ.get("AWS_PROFILE", "cto")
REGION = os.environ.get("AWS_REGION", "us-east-1")
PROMPT = "Hello, how can you help me today?"


def main() -> int:
    client = boto3.Session(profile_name=PROFILE, region_name=REGION).client(
        "bedrock-runtime"
    )
    try:
        response = client.converse_stream(
            modelId=MODEL_ID,
            messages=[{"role": "user", "content": [{"text": PROMPT}]}],
        )
    except (ClientError, BotoCoreError) as err:
        print(f"Bedrock call failed: {err}", file=sys.stderr)
        return 1

    usage: dict[str, int] | None = None
    for event in response["stream"]:
        if "contentBlockDelta" in event:
            text = event["contentBlockDelta"]["delta"].get("text", "")
            print(text, end="", flush=True)
        elif "metadata" in event:
            usage = event["metadata"].get("usage")
    print()

    if usage is None:
        print("Stream ended without usage metadata", file=sys.stderr)
        return 1

    input_tokens = usage["inputTokens"]
    output_tokens = usage["outputTokens"]
    cost = estimate_cost(input_tokens, output_tokens)
    print(
        f"Input tokens: {input_tokens}, Output tokens: {output_tokens}, "
        f"Estimated cost: ${cost:.4f}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
