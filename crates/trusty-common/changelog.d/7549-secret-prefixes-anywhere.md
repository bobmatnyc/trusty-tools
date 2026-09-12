Fixed

- Secret detection now matches a known provider prefix (`sk-`, `ghp_`, `xoxb-`, …) at any delimiter boundary in a token, not only at its start, so a key written after a prepended segment, an `=`, a quote, or inside a JSON pair is redacted instead of being rescued as a segmented identifier (#7549).
