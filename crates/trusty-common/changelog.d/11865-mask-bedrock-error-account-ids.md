Fixed
- A failed Bedrock send no longer exposes an AWS account id: `BedrockError` messages mask the 12-digit account id of every ARN, in the model id and in the AWS error text (#11865).
