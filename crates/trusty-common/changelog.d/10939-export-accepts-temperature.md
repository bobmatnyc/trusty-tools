Changed
- `claude_temperature::accepts_temperature` is now public (behind the `bedrock` or `inference-client` feature), so downstream request builders reuse the Opus/Sonnet 5.5 temperature rule (#10939).
