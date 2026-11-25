Breaking
- `InferenceError` gains a `Bedrock(BedrockError)` variant: a failed Bedrock `Converse`/`ConverseStream` send now arrives as this variant instead of `Provider(String)`. Its display text and its `is_retryable`/`is_alarm` answers are unchanged; an exhaustive `match` on `InferenceError` needs one more arm (#10892).
