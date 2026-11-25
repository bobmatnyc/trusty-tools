Added
- `inference::BedrockError`: the typed cause of a failed Bedrock send — `Api { status, code, message }`, `Transport`, `MissingCredential`, `Timeout`, `Other`. A missing AWS credential is `MissingCredential` whichever SDK phase reports it (#10892).
- `inference::bedrock::arn`: the Bedrock model-ARN parser (`parse`, `BedrockArn`, `ArnKind`, `is_unpriced`) and `mask_account_ids`, moved unchanged from trusty-review so every Bedrock consumer shares them (#10892).
