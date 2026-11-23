Fixed
- The pause sanitizer keeps a JSON key's closing quote (`"token": "[REDACTED]"`), redacts a JWT after a credential key as one value across its dots, and redacts a letter-only value after `=` or `:` (#10595).
