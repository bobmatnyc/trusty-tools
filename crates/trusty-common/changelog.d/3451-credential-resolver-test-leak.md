Security
- `credentials::resolver`'s precedence tests no longer print a resolved
  credential when they fail. The env tier reads real credential variables, so
  a bare `assert_eq!` on the result echoed whatever the process held — which
  is how a live OpenRouter key reached test output. Actual values now render
  through `redact_secret`, and a test provokes the failure path to prove it
  (#3451).
