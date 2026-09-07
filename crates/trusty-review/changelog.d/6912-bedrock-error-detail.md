Fixed

- Bedrock Converse failures now report the AWS error code and message (for example
  `ResourceNotFoundException: Model use case details have not been submitted for this
  account.`) instead of the SDK's flattened literal `service error`, which made a wrong
  region, a missing credential, and an unapproved model read identically (#6912).
