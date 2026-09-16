Fixed

- `tm issue transition <n> <state>` against an issue already in `<state>` now prints `#<n> is already \`<state>\` — no change made` on stderr and exits 0, instead of failing the self-edge as an invalid transition; no label, assignee or comment is written (refs [#8003](https://github.com/bobmatnyc/trusty-tools/issues/8003))
