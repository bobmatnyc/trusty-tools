Fixed

- Every bundled PM/agent asset that names `TodoWrite` (or the `TaskCreate` task-tool family) now pairs the mention with the condition and fallback — `core.md`'s PM Allowlist, `tm-delegation-patterns`, `tm-circuit-breaker`, `tm-session-pause`, `tm-session-management`, and both non-default output styles no longer read as if the harness always exposes the tool (#2799)
- A regression test, `bundled_assets_never_mandate_gated_task_tools_unconditionally`, walks the whole `src/assets/` tree and fails on any future unconditional `TodoWrite`/`TaskCreate` mention, not just the three sites #8097 covered (#2799)
