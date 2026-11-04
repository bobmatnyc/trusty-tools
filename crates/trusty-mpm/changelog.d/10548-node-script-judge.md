Fixed
- `tm hook --pm-guard` no longer refuses `node --check <file>` or `node -c <file>` for what the file contains: a syntax check parses the file and never runs it. Any other node option beside the check, or an inline `NODE_OPTIONS` prefix, keeps the body judged as a run (#10548).
- `tm hook --pm-guard` reads a node script body as JavaScript before judging it, so a member access such as `x.key`, `cfg.env` or `lic.license` no longer reads as a secret-bearing file. A secret file named in a string, template or regex literal still refuses (#10548, #7266).
