---
name: prototype-then-port
description: "Prototype in Python while the shape changes, then port the settled parts to Rust. Use when starting a tool or script whose behaviour is not settled, when deciding whether a Python prototype should become Rust, or when doing the port. Gives the port trigger, the step path and the checklist; mappings, idioms, parity and pitfalls live in references/."
user-invocable: false
metadata:
  version: "0.1.0"
category: agent-reference
tags: [rust, python, port, prototyping, migration]
effort: low
---

# Prototype in Python, then port to Rust

Prototype in Python while the shape changes. Port the settled parts to Rust
when a measured cost and a named consumer justify it. Freeze the behaviour
first, prove parity second, cut over last.

Source: Owner ruling 2026-10-30.

## Port trigger

Port only when all three hold:

1. **Stable.** No behaviour change for an agreed period, and no open change
   requests against it.
2. **Measured cost.** A recorded number: latency, memory, startup, deploy
   friction, or type bugs that reached users. Record it before the port.
3. **Named consumer.** Callers that need the Rust build, by name.

Keep the Python when the behaviour still moves, the cost is small, the
tool wraps a Python-only library, or no caller needs the port.

## Step path

1. **Capture golden outputs** from the Python on real inputs.
2. **Write the interface contract**: flags, JSON shapes, exit codes, env
   vars, files touched. Grep callers; list unused surface for dropping.
3. **Pin the Python source** to a commit or tag. Changes after the pin get
   replayed into the port or wait.
4. **Map types first**, then port module by module, leaves first.
5. **Keep the interface byte-compatible** with the contract. Set serializer
   formats explicitly.
6. **Run the side-by-side harness** and meet the parity bar.
7. **Cut over** in one change. Keep the Python as a shim only for a named
   fallback period, then delete it.
8. **Record the port** in the ledger (`docs/ports.md` or your equivalent).

## Checklist

```
[ ] Port trigger met: stable, measured cost, named consumer
[ ] Golden corpus captured and committed
[ ] Interface contract written; unused surface dropped or listed
[ ] Python source pinned
[ ] Types mapped; serializer formats set explicitly
[ ] Errors mapped to exit codes and messages
[ ] Harness passes with normalizations listed; parity bar met
[ ] Live check in the real environment
[ ] Callers switched in one change; fallback named with a removal date
[ ] Python removed after fallback; ledger row updated
```

## Load next

| File | Read it when |
|---|---|
| `references/type-mapping.md` | mapping a dataclass, pydantic model, enum, dict, datetime, path or number |
| `references/idioms.md` | porting exceptions, CLI parsing, logging, subprocess, HTTP, regex, JSON, generators or global state |
| `references/parity.md` | capturing golden outputs, writing the harness, setting the parity bar, running shadow mode |
| `references/incremental.md` | choosing PyO3 for one hot spot instead of a whole binary, or keeping a Python shim during cutover |
| `references/pitfalls.md` | before cutover; review the list against the port's data and I/O |
