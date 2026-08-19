Fixed

- The Bitbucket auth check and the LLM API-key check now take their environment
  values as arguments instead of reading `BITBUCKET_TOKEN`,
  `BITBUCKET_APP_PASSWORD`, `OPENROUTER_API_KEY` and `OPENAI_API_KEY` inline.
  Their tests previously removed those variables process-wide, which is `unsafe`
  under the 2024 edition and raced the rest of the suite: the restore put
  `OPENROUTER_API_KEY` back while a concurrent test was resolving a credential,
  failing it 40 runs out of 40. No test in `validator_tests.rs` mutates the
  process environment any more, and the present-key branches are covered for the
  first time.
