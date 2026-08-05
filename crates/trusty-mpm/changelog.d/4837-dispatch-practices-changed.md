Changed

- `BASE-AGENT.md` "Never Directly Monitor a Declarative Process" no longer recommends `<command> 2>&1 | tm compress`, which took its exit code from `tm compress` and masked a failing gate. The trim now reads from the captured file (`tm compress --tool "cargo test" < /tmp/gate.txt`) and only when the verdict is non-zero
