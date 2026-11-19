Changed
- Agents tm deploys and `tm install --reset-agents` writes now carry `omitClaudeMd: true`, so dispatched subagents start without CLAUDE.md while the PM keeps it. Needs Claude Code v2.1.271+; older versions ignore the key (#7684).
