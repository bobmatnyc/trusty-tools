Added

- `inference::bedrock` exposes the three pieces a consumer with its own Converse call path needs: `resolve_bedrock_region` and the pure `resolve_region_from` walk behind it, the `DEFAULT_REGION` constant, `BedrockAdapter::client` (the lazily-built Converse client), and `BedrockAdapter::with_client` (an injection seam for a pre-built, credential-free client). trusty-review's `BedrockProvider` routes through these instead of resolving the region and building an AWS client itself
