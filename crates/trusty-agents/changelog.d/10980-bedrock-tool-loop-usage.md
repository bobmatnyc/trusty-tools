Fixed

- Bedrock tool-loop dispatches now publish `LlmRequested`/`LlmResponded` events and write one `usage.jsonl` row carrying the exact Bedrock model id, runner `bedrock` and token counts, so the Costs tab and event stream see Bedrock assistant turns (#10980). The persona turn logs the returned token usage instead of discarding it.
