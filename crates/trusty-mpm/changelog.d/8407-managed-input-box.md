Fixed
- `tm sessions output` and `tm sessions activity` now report a managed session's input box as `empty`, `suggestion` or `typed`, classified from an escaped capture of its own pane; an unreadable pane still reads `unknown`. The activity JSON gains an additive `input_box` field (#8407).
