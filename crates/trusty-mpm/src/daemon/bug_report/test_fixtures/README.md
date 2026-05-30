# test_fixtures — Bug-Report Unit Test Fixtures

This directory holds static fixtures used by the bug-report module's unit tests.

## `test_rsa_key.pem`

A 2048-bit RSA private key in PKCS#1 PEM format, generated once and committed for
use by `token.rs` JWT signing/verification unit tests.

**This key contains no real secrets and is intentionally public.** It is used
exclusively to verify that `encode_jwt_rs256` produces a well-formed JWT with
the correct claims (`iss`, `iat`, `exp`). It has no authorization to any real
GitHub App or API.

To regenerate (e.g. after key rotation for testing purposes):

```bash
openssl genrsa -traditional 2048 > crates/trusty-mpm/src/daemon/bug_report/test_fixtures/test_rsa_key.pem
```
