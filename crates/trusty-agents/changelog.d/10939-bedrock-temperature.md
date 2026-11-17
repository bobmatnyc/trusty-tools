Fixed
- Bedrock Converse requests to Claude Opus 5.5 and Sonnet 5.5 no longer carry `temperature`, which those models reject with a `ValidationException`. The assistant tool loop, delegated children and single-turn calls all use trusty-common's `accepts_temperature`; other models keep their configured temperature (#10939).
