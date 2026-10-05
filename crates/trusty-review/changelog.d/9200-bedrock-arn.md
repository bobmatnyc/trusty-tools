Fixed

- A reviewer, verifier or MCP `reviewer_model` set to a Bedrock model ARN
  (`application-inference-profile`, `inference-profile` or `foundation-model`)
  now runs on Bedrock Converse. Before, the Bedrock provider rejected every ARN
  for lacking a `us.`-style prefix, so the review failed to start (#9200).
- An ARN call goes to the region inside the ARN, ahead of `TRUSTY_AWS_REGION`
  and `AWS_REGION`, because Bedrock resolves an ARN only in its own region. The
  new `BedrockProvider::new_in_region` takes an explicit region, which wins
  over the ARN's (#9200).
- Cost for an `inference-profile` or `foundation-model` ARN is priced as the
  model id the ARN names. An `application-inference-profile` ARN does not name
  its model, so a warning log line reports it as unpriced and the review footer
  shows `est. unpriced` in place of a dollar estimate (#9200).
- An ARN's 12-digit account id is masked as `****` in the review footer, in
  Bedrock error messages (including ARNs that AWS quotes back), in the
  model-id validation error and in Bedrock log lines. The ARN sent to AWS is
  not masked. The validation error now says a Bedrock model ARN is accepted
  (#9200).
