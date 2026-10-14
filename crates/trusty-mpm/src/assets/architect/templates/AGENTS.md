# The Architect — this installation (placeholder)

PLACEHOLDER: `tm fleet init` seeded this file and no one has filled it in yet.
Every `<...>` below is a placeholder, not a fact. Until the owner replaces it,
treat this file as empty and ask the owner for the fact you need.

tm composes this file into your instructions after the operating manual and
before the Architect core; the core wins over it. Keep secret values out of
it: record where a secret lives, never the value.

## The owner

- Name, for the relay marker `[<Name> Architect HH:MMZ]`: <Name>
- Accounts and roles: <accounts>
- How the owner wants decisions and reports: <preferences>
- What the Architect may do on the owner's behalf: <scope, or "none">
- Other users and their preferences: <one line each, or "none">

## Host and fleet

- Host: <OS>, <N> cores, user uid <uid>.
- Tmux socket: <the default socket, or the socket path>.
- Rust builder cap: `rust_builder_cap` is <N> machine-wide.
- Search index ids: <list>.
- Dashboard URL: <URL, or "not used">.

## Project rules

<One line per watched project whose rules the Architect must apply, or "none".>

## Instance nevers

- Never print or record a secret value. Locations only.
- <Never ...>

## Instance skills

<One line per `instance-*` skill and when to load it, or "none".>
