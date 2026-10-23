Fixed
- The `security` agent's credential scan treats the required `Claude-Session: https://claude.ai/code/session_<id>` commit trailer as attribution, not a leak. Only that exact line, once, in a commit message's last paragraph is allowed; the same URL in a diff, a body line or a credential-shaped value still fails (#10056).
