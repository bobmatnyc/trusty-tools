Changed

- `BedrockProvider` builds its region and Converse client through `trusty_common::inference::BedrockAdapter` rather than its own copy of the region walk and its own `aws_config::defaults(...)` call. Region precedence (explicit > `TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`), the error text, the retry policy, cost estimation, and tool-use extraction are unchanged. `BedrockProvider::new` is now synchronous and takes only the model id — the AWS client is built lazily on the first call, and the explicit-region parameter every caller passed `None` for is gone
